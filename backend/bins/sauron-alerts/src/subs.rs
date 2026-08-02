//! The personal-subscription evaluation pass: load, coalesce, probe, fan out
//! by app id, throttle, enqueue.
//!
//! Producers never send mail. This module only INSERTs into
//! `notification_queue`; `drain.rs` is the single place that turns queue rows
//! into `mail_outbox` rows. That split is what lets `sauron-monitor`
//! participate without ever learning about SMTP, and it is what makes delivery
//! exclusive across replicas.

use std::sync::Arc;

use chrono::Utc;
use sauron_alerts::subscription::{coalesce, spike_fires, Probe, SubConditions, SubInput, SubKind};
use sauron_core::Config;
use sauron_db::models::NotificationSubscription;
use sauron_db::repo::{self, QueueInsert};
use sauron_db::PgPool;
use sauron_redis::RedisStore;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use uuid::Uuid;

/// Visit `items` starting at `offset % len`, wrapping.
///
/// A single global probe ceiling is a cross-tenant starvation vector: a handful
/// of self-registered accounts saturating it would silently stop evaluating a
/// paying tenant's subscriptions. The ceiling is therefore per-org, and this
/// rotation makes a clip move around instead of always landing on the same
/// alphabetically-unlucky tenant.
fn rotate<T: Clone>(items: &[T], offset: u64) -> Vec<T> {
    if items.is_empty() {
        return Vec::new();
    }
    let start = (offset % items.len() as u64) as usize;
    items[start..]
        .iter()
        .chain(items[..start].iter())
        .cloned()
        .collect()
}

/// `min(20 × app_count, 200) + 1`.
///
/// A probe spans several apps, so the shipped fixed 20 lets one noisy app
/// starve the rest. The `+ 1` is the truncation sentinel: if the full count
/// comes back, the rendered email says "and more".
fn issue_limit(app_count: usize) -> i64 {
    (20i64.saturating_mul(app_count.max(1) as i64)).min(200) + 1
}

/// The `dedup_key` infix that marks a truncation-sentinel queue row.
///
/// The sentinel travels as an ordinary queue row so it gets the same
/// delivery-time coverage re-check as the issues it summarises. `drain.rs`
/// matches on this to sort it last — "and more" printed in the middle of a
/// list conveys nothing.
pub(crate) const TRUNCATION_MARKER: &str = ":truncated:";

/// One notification the evaluator decided to send, before throttling.
struct Candidate {
    subscription_id: Uuid,
    project_id: Uuid,
    app_id: Uuid,
    throttle_seconds: i32,
    env_enrollments: Vec<Uuid>,
    includes_unattributed: bool,
    kind: String,
    dedup_key: String,
    severity: String,
    title: String,
    body: String,
}

/// Evaluate every enabled non-uptime subscription once.
///
/// Uptime is NOT evaluated here: it is event-driven and enqueued inline by
/// `sauron-monitor`, exactly as `monitor_down`/`monitor_up` alert rules are.
pub async fn evaluate_subscriptions(
    pool: &PgPool,
    redis: &RedisStore,
    cfg: &Config,
    tick_counter: u64,
) -> anyhow::Result<()> {
    let mut conn = sauron_db::conn(pool).await?;
    let subs = repo::enabled_subscriptions_by_kinds(
        &mut conn,
        &["error_spike", "error_new_issue", "error_regression"],
    )
    .await?;
    if subs.is_empty() {
        return Ok(());
    }

    // Every scope and every environment set resolved in BATCHED queries, never
    // one per subscription. Three round trips total, whatever N is: the env
    // child rows, the app-scope ancestries, the project-scope app lists. Doing
    // this per subscription would be N round trips per tick against a pool of 8
    // shared with the drain, which is the blow-up the probe coalescing further
    // down exists to prevent.
    let sub_ids: Vec<Uuid> = subs.iter().map(|s| s.id).collect();
    let env_rows = repo::subscription_envs_for(&mut conn, &sub_ids).await?;

    let mut app_scope_ids: Vec<Uuid> = Vec::new();
    let mut project_scope_ids: Vec<Uuid> = Vec::new();
    for s in subs.iter() {
        match s.scope_type.as_str() {
            "app" => app_scope_ids.push(s.scope_id),
            _ => project_scope_ids.push(s.scope_id),
        }
    }
    app_scope_ids.sort_unstable();
    app_scope_ids.dedup();
    project_scope_ids.sort_unstable();
    project_scope_ids.dedup();

    // `scope_id` has no FK, so a row can outlive its target. An id absent from
    // these results is an unresolvable scope and its subscription is skipped,
    // never guessed at.
    let live_app_scopes = repo::app_ancestries(&mut conn, &app_scope_ids).await?;
    let project_apps = repo::apps_for_projects(&mut conn, &project_scope_ids).await?;

    let mut inputs: Vec<SubInput> = Vec::with_capacity(subs.len());
    for (index, s) in subs.iter().enumerate() {
        let Some(kind) = SubKind::parse(&s.kind) else {
            continue;
        };
        let app_ids: Vec<Uuid> = match s.scope_type.as_str() {
            "app" => {
                if live_app_scopes.iter().any(|(a, _, _)| *a == s.scope_id) {
                    vec![s.scope_id]
                } else {
                    continue;
                }
            }
            _ => project_apps
                .iter()
                .filter(|(project_id, _)| *project_id == s.scope_id)
                .map(|(_, app_id)| *app_id)
                .collect(),
        };
        if app_ids.is_empty() {
            continue;
        }
        inputs.push(SubInput {
            index,
            org_id: s.org_id,
            kind,
            cond: SubConditions::from_value(kind, &s.conditions),
            catalogue_envs: env_rows
                .iter()
                .filter(|(sid, _)| *sid == s.id)
                .map(|(_, e)| *e)
                .collect(),
            app_ids,
        });
    }

    // One crossing of the catalogue -> enrollment bridge over the union of every
    // app in play.
    let mut all_apps: Vec<Uuid> = inputs.iter().flat_map(|i| i.app_ids.clone()).collect();
    all_apps.sort_unstable();
    all_apps.dedup();
    let enrollments = repo::live_enrollments_for_apps(&mut conn, &all_apps).await?;
    // Don't hold a pooled connection across the fan-out: this pool is 8 for the
    // whole process and is shared with the drain.
    drop(conn);

    let probes = coalesce(&inputs);

    // Per-ORG ceiling, applied in rotating order.
    let mut org_ids: Vec<Uuid> = probes.iter().map(|p| p.key.org_id).collect();
    org_ids.sort_unstable();
    org_ids.dedup();
    let ceiling = cfg.notify_subs_max_probes_per_org.clamp(1, 1000);
    let mut allowed: Vec<usize> = Vec::new();
    for org_id in rotate(&org_ids, tick_counter) {
        let mine: Vec<usize> = probes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.key.org_id == org_id)
            .map(|(i, _)| i)
            .collect();
        if mine.len() > ceiling {
            // Observable rather than inferred: "we are not evaluating your
            // subscriptions" must appear in the log, with the org named.
            warn!(
                org = %org_id,
                probes = mine.len(),
                skipped = mine.len() - ceiling,
                "subscription probe ceiling reached"
            );
        }
        allowed.extend(mine.into_iter().take(ceiling));
    }

    // The same Semaphore(4) bound the rule evaluator uses, for the same reason.
    let sem = Arc::new(Semaphore::new(4));
    let now = Utc::now();
    let subs = Arc::new(subs);
    let enrollments = Arc::new(enrollments);
    let mut handles = Vec::with_capacity(allowed.len());
    for probe_idx in allowed {
        let probe = probes[probe_idx].clone();
        let pool = pool.clone();
        let redis = redis.clone();
        let sem = sem.clone();
        let subs = subs.clone();
        let enrollments = enrollments.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            if let Err(e) = run_probe(&pool, &redis, &probe, &subs, &enrollments, now).await {
                warn!(error = %e, "subscription probe failed");
            }
        }));
    }
    for h in handles {
        if let Err(e) = h.await {
            warn!(error = %e, "subscription probe task panicked");
        }
    }
    Ok(())
}

/// Run one probe and enqueue whatever it produced.
///
/// Fan-out is BY APP ID, never by positional index. A key-collision bug in the
/// coalescing would otherwise attribute one app's counts to another user's
/// subscription — a telemetry leak inside an email. App ids are globally unique
/// UUIDs, so a wrong attribution requires an id bug rather than a
/// set-membership bug, and the drain's independent reach re-check catches the
/// cross-tenant case even then.
async fn run_probe(
    pool: &PgPool,
    redis: &RedisStore,
    probe: &Probe,
    subs: &[NotificationSubscription],
    enrollments: &[(Uuid, Uuid, Uuid)],
    now: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    let key = &probe.key;
    // The probe's enrollment array: live enrollments of its apps whose
    // CATALOGUE environment is in the key's set. `None` when the set is empty,
    // which means "every environment plus unattributed rows".
    let env_ids: Option<Vec<Uuid>> = if key.catalogue_envs.is_empty() {
        None
    } else {
        Some(
            enrollments
                .iter()
                .filter(|(_, app, catalogue)| {
                    probe.app_ids.contains(app) && key.catalogue_envs.contains(catalogue)
                })
                .map(|(enrollment, _, _)| *enrollment)
                .collect(),
        )
    };
    let env_ref = env_ids.as_deref();
    let includes_unattributed = env_ids.is_none();

    let window = chrono::Duration::seconds(key.cond.window_seconds as i64);
    let mut conn = sauron_db::conn(pool).await?;

    let mut candidates: Vec<Candidate> = Vec::new();
    match key.kind {
        SubKind::ErrorSpike => {
            let from = now - window;
            let prev_from = from - window;
            let current = repo::alert_count_errors_by_app(
                &mut conn,
                &probe.app_ids,
                from,
                now,
                key.cond.level.as_deref(),
                env_ref,
            )
            .await?;
            let baseline = repo::alert_count_errors_by_app(
                &mut conn,
                &probe.app_ids,
                prev_from,
                from,
                key.cond.level.as_deref(),
                env_ref,
            )
            .await?;
            let mins = key.cond.window_seconds / 60;
            for (app_id, c) in current {
                let b = baseline
                    .iter()
                    .find(|(a, _)| *a == app_id)
                    .map(|(_, n)| *n)
                    .unwrap_or(0);
                if !spike_fires(
                    c,
                    b,
                    key.cond.min_count,
                    key.cond.factor_milli as f64 / 1000.0,
                ) {
                    continue;
                }
                for &sub_idx in &probe.subs {
                    let s = &subs[sub_idx];
                    if !subscription_owns_app(s, app_id) {
                        continue;
                    }
                    candidates.push(Candidate {
                        subscription_id: s.id,
                        project_id: Uuid::nil(),
                        app_id,
                        throttle_seconds: s.throttle_seconds,
                        env_enrollments: env_ids.clone().unwrap_or_default(),
                        includes_unattributed,
                        kind: "error_spike".into(),
                        dedup_key: format!("sub:{}:spike:{app_id}", s.id),
                        severity: "warning".into(),
                        title: format!("Error spike in the last {mins}m"),
                        body: format!(
                            "{c} error event(s) in the last {mins} minute(s) vs {b} in the \
                             previous {mins}."
                        ),
                    });
                }
            }
        }
        SubKind::ErrorNewIssue | SubKind::ErrorRegression => {
            // The watermark is the OLDEST `last_evaluated_at` among this probe's
            // subscriptions, floored at one window, so a subscription that fell
            // behind is caught up rather than skipped.
            let since = probe
                .subs
                .iter()
                .filter_map(|i| subs[*i].last_evaluated_at)
                .min()
                .unwrap_or(now - window)
                .max(now - window);
            let limit = issue_limit(probe.app_ids.len());
            let mut issues = match (key.kind, env_ref) {
                (SubKind::ErrorNewIssue, Some(envs)) => {
                    repo::alert_new_issues_env(
                        &mut conn,
                        &probe.app_ids,
                        since,
                        now,
                        key.cond.level.as_deref(),
                        envs,
                        limit,
                    )
                    .await?
                }
                (SubKind::ErrorNewIssue, None) => {
                    repo::alert_new_issues(
                        &mut conn,
                        &probe.app_ids,
                        since,
                        now,
                        key.cond.level.as_deref(),
                        limit,
                    )
                    .await?
                }
                (_, Some(envs)) => {
                    repo::alert_regressed_issues_env(
                        &mut conn,
                        &probe.app_ids,
                        since,
                        now,
                        key.cond.level.as_deref(),
                        envs,
                        limit,
                    )
                    .await?
                }
                (_, None) => {
                    repo::alert_regressed_issues(
                        &mut conn,
                        &probe.app_ids,
                        since,
                        now,
                        key.cond.level.as_deref(),
                        limit,
                    )
                    .await?
                }
            };
            // `issue_limit` asked for one row more than it intends to send. A
            // full result set therefore means "there is at least one more issue
            // that will not be named", and the sentinel row below is what turns
            // that into something the reader can see. Without it a truncated
            // batch is indistinguishable from a complete one, and the reader
            // draws the wrong conclusion from a number that is simply short.
            let truncated = issues.len() as i64 >= limit;
            if truncated {
                issues.truncate((limit - 1).max(0) as usize);
            }

            let verb = if key.kind == SubKind::ErrorNewIssue {
                "New issue"
            } else {
                "Issue regressed"
            };
            for issue in issues {
                for &sub_idx in &probe.subs {
                    let s = &subs[sub_idx];
                    if !subscription_owns_app(s, issue.app_id) {
                        continue;
                    }
                    candidates.push(Candidate {
                        subscription_id: s.id,
                        project_id: Uuid::nil(),
                        app_id: issue.app_id,
                        throttle_seconds: s.throttle_seconds,
                        env_enrollments: env_ids.clone().unwrap_or_default(),
                        includes_unattributed,
                        kind: key.kind.as_str().to_string(),
                        dedup_key: format!("sub:{}:issue:{}", s.id, issue.id),
                        severity: "warning".into(),
                        title: format!("{verb}: {}", issue.title),
                        body: format!(
                            "{verb} ({}) — seen {} time(s).",
                            issue.level, issue.times_seen
                        ),
                    });
                }
            }

            if truncated {
                // The sentinel is a real queue row, not a flag, so it inherits
                // the drain's delivery-time coverage re-check exactly like the
                // issues it summarises. It carries the app id of that
                // subscription's last issue because a row with `app_id = None`
                // is read as UPTIME by `covers` and would be refused to every
                // app- and env-scoped member.
                let sentinels: Vec<Candidate> = probe
                    .subs
                    .iter()
                    .filter_map(|&sub_idx| {
                        let s = &subs[sub_idx];
                        let app_id = candidates
                            .iter()
                            .rev()
                            .find(|c| c.subscription_id == s.id)?
                            .app_id;
                        Some(Candidate {
                            subscription_id: s.id,
                            project_id: Uuid::nil(),
                            app_id,
                            // Never throttled. It is the honesty marker on a
                            // batch that IS being delivered; suppressing it
                            // would leave the undercount silent, which is the
                            // whole failure it exists to prevent.
                            throttle_seconds: 0,
                            env_enrollments: env_ids.clone().unwrap_or_default(),
                            includes_unattributed,
                            kind: key.kind.as_str().to_string(),
                            // The marker is how the drain recognises this row
                            // and sorts it last; the timestamp keeps successive
                            // truncated passes from colliding on the partial
                            // unique index.
                            dedup_key: format!(
                                "sub:{}{TRUNCATION_MARKER}{}",
                                s.id,
                                now.timestamp()
                            ),
                            severity: "info".into(),
                            title: "…and more".into(),
                            // No count. The probe's limit is shared across every
                            // subscription in it, but `subscription_owns_app`
                            // hands each one a different subset, so any number
                            // printed here would be right for some readers and
                            // wrong for the rest.
                            body: "More issues matched than fit in one notification; the list \
                                   above is not complete."
                                .to_string(),
                        })
                    })
                    .collect();
                candidates.extend(sentinels);
            }
        }
        SubKind::Uptime => {}
    }

    if candidates.is_empty() {
        let ids: Vec<Uuid> = probe.subs.iter().map(|i| subs[*i].id).collect();
        repo::touch_subscriptions_evaluated(&mut conn, &ids, now).await?;
        return Ok(());
    }

    // Fill each candidate's project id from its own app, in one query.
    let app_ids: Vec<Uuid> = candidates.iter().map(|c| c.app_id).collect();
    let ancestries = repo::app_ancestries(&mut conn, &app_ids).await?;
    for c in &mut candidates {
        if let Some((_, project_id, _)) = ancestries.iter().find(|(a, _, _)| *a == c.app_id) {
            c.project_id = *project_id;
        }
    }
    candidates.retain(|c| c.project_id != Uuid::nil());

    // Throttle: Redis first, durable fallback when Redis is unreachable.
    // Extending the key with the subscription id is what gives PER-RECIPIENT
    // throttling with no new infrastructure — the org engine's key is per rule.
    // The 250ms timeout exists because `RedisStore` is built with
    // `set_response_timeout(None)` and a command against a dead Redis is
    // measured at 9-19s.
    let mut allowed: Vec<Candidate> = Vec::new();
    for c in candidates {
        if c.throttle_seconds <= 0 {
            allowed.push(c);
            continue;
        }
        let redis_key = format!("sauron:notify:{}:{}", c.subscription_id, c.dedup_key);
        let claimed = match tokio::time::timeout(
            std::time::Duration::from_millis(250),
            redis.set_nx_ex(&redis_key, "1", c.throttle_seconds as u64),
        )
        .await
        {
            Ok(Ok(true)) => true,
            Ok(Ok(false)) => false,
            _ => {
                !repo::notification_recently_queued(
                    &mut conn,
                    c.subscription_id,
                    &c.dedup_key,
                    c.throttle_seconds,
                )
                .await?
            }
        };
        if claimed {
            allowed.push(c);
        }
    }

    let rows: Vec<QueueInsert> = allowed
        .iter()
        .map(|c| QueueInsert {
            subscription_id: c.subscription_id,
            project_id: c.project_id,
            app_id: Some(c.app_id),
            includes_unattributed: c.includes_unattributed,
            kind: &c.kind,
            dedup_key: &c.dedup_key,
            severity: &c.severity,
            title: &c.title,
            body: &c.body,
            link: None,
            env_enrollments: c.env_enrollments.clone(),
        })
        .collect();
    let n = repo::enqueue_notifications(&mut conn, &rows).await?;
    if n > 0 {
        info!(
            enqueued = n,
            kind = key.kind.as_str(),
            "personal notifications enqueued"
        );
    }

    let ids: Vec<Uuid> = probe.subs.iter().map(|i| subs[*i].id).collect();
    repo::touch_subscriptions_evaluated(&mut conn, &ids, now).await?;
    Ok(())
}

/// Whether this subscription's own scope includes `app_id`.
///
/// A probe's app array is the UNION of its subscriptions' scopes, so a result
/// for app X must only be attributed to the subscriptions that actually cover
/// X — otherwise a shared condition bucket would cross-deliver between users of
/// the same org.
fn subscription_owns_app(s: &NotificationSubscription, app_id: Uuid) -> bool {
    match s.scope_type.as_str() {
        "app" => s.scope_id == app_id,
        // A project-scoped subscription owns every app resolved from its own
        // project query, and `evaluate_subscriptions` built the app list from
        // exactly that query.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orgs_rotate_so_a_clip_does_not_always_land_on_the_same_tenant() {
        let orgs: Vec<u64> = (0..5).collect();
        // A single global probe ceiling is a cross-tenant starvation vector, so
        // the ceiling is per-org AND the visiting order rotates.
        assert_eq!(rotate(&orgs, 0), vec![0, 1, 2, 3, 4]);
        assert_eq!(rotate(&orgs, 2), vec![2, 3, 4, 0, 1]);
        assert_eq!(rotate(&orgs, 7), vec![2, 3, 4, 0, 1]);
        assert_eq!(rotate(&[] as &[u64], 3), Vec::<u64>::new());
    }

    #[test]
    fn the_issue_limit_scales_with_app_count_and_is_capped() {
        // A probe spans several apps, so a fixed 20 lets one noisy app starve
        // the rest — but an unbounded limit would let a 5000-app org pull 100k
        // rows into one tick.
        assert_eq!(issue_limit(1), 21);
        assert_eq!(issue_limit(3), 61);
        assert_eq!(issue_limit(50), 201);
        assert_eq!(issue_limit(0), 21);
    }
}
