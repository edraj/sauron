//! The notification drain: claim, re-check reach, group by user, render one
//! email, hand it to `mail_outbox`.
//!
//! Rendering deliberately does NOT go through `sauron_alerts::deliver` or build
//! an `AlertContext`: `render::email_subject` would stamp `[Sauron/info]` on it
//! and `render::email_body` would sign it "— Sauron alerting". Personal mail
//! must not carry alert-engine branding.
//!
//! `sauron_mail::text::html_escape` does NOT escape the single quote, so
//! anything rendered through the house layout must double-quote every
//! attribute.
//!
//! This process ENQUEUES into `mail_outbox` and never drains it — `sauron-api`
//! is the sole drainer, so `sauron-alerts` needs no SMTP configuration and
//! personal mail cannot be delivered twice by two processes.

use std::collections::HashMap;

use chrono::Utc;
use sauron_alerts::subscription::{covers, QueueTarget};
use sauron_auth::rbac::{grants_from_rows, perm, reach_for, Reach};
use sauron_core::Config;
use sauron_db::models::{NewMailOutbox, NotificationQueueItem};
use sauron_db::repo;
use sauron_db::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// The per-user hourly cap degrades delivery to one digest; it never discards.
fn should_digest(sent_messages_last_hour: i64, cap: i64) -> bool {
    sent_messages_last_hour >= cap
}

/// One row keeps its own subject; several become a counted digest.
fn digest_subject(rows: usize, first_title: &str) -> String {
    if rows <= 1 {
        first_title.to_string()
    } else {
        format!("{rows} Sauron notifications")
    }
}

/// Claim and deliver whatever is due, looping until the batch is short or the
/// wall-clock budget is spent.
///
/// A single 200-row batch per 30s tick is ~400 rows/minute, and two shapes
/// exceed that routinely: every `daily` subscriber's rows come due at the same
/// bucket boundary, and a broad incident enqueues across many subscribers at
/// once. Each pass logs pending depth and the oldest pending `deliver_after`,
/// because nothing else in the system would reveal a backlog — `status='sent'`
/// means only "handed to the outbox".
pub async fn drain_notification_queue(pool: &PgPool, cfg: &Config) -> anyhow::Result<()> {
    let batch = cfg.notify_subs_batch.clamp(1, 5000);
    let budget = std::time::Duration::from_millis(cfg.notify_drain_budget_ms.clamp(500, 60_000));
    let started = std::time::Instant::now();

    loop {
        let mut conn = sauron_db::conn(pool).await?;
        let claimed = repo::claim_due_notifications(&mut conn, batch).await?;
        if claimed.is_empty() {
            drop(conn);
            break;
        }
        let taken = claimed.len();
        deliver_batch(&mut conn, cfg, claimed).await?;
        let (depth, oldest) = repo::notification_queue_depth(&mut conn).await?;
        drop(conn);
        info!(delivered = taken, pending = depth, oldest = ?oldest, "notification drain pass");

        if taken < batch as usize || started.elapsed() >= budget {
            break;
        }
    }
    Ok(())
}

async fn deliver_batch(
    conn: &mut sauron_db::AsyncPgConnection,
    cfg: &Config,
    claimed: Vec<NotificationQueueItem>,
) -> anyhow::Result<()> {
    let queue_ids: Vec<Uuid> = claimed.iter().map(|r| r.id).collect();
    let env_rows = repo::queue_envs_for(conn, &queue_ids).await?;
    let project_ids: Vec<Uuid> = claimed.iter().map(|r| r.project_id).collect();
    let orgs = repo::project_org_batch(conn, &project_ids).await?;

    let mut by_user: HashMap<Uuid, Vec<NotificationQueueItem>> = HashMap::new();
    for row in claimed {
        by_user.entry(row.user_id).or_default().push(row);
    }

    // Byte-for-byte identical to `unsub_signing_key` in
    // `sauron-api/src/routes/notification_prefs.rs` (Task 14 Step 3). This
    // process mints the tokens and that one verifies them; a divergence makes
    // every unsubscribe link fail verification, and that endpoint returns the
    // same body whether verification succeeded or not, so the breakage is
    // completely silent. Change one, change the other.
    let unsub_key = {
        let base = cfg.notify_secret_key.clone().unwrap_or_else(|| {
            cfg.require_jwt_secret()
                .map(String::from)
                .unwrap_or_default()
        });
        sauron_alerts::crypto::derive_unsub_key(base.as_bytes())
    };
    let today = sauron_alerts::crypto::days_since_epoch(Utc::now());

    let branding = sauron_mail::Branding {
        product_name: "Sauron".to_string(),
        // `Config::dashboard_url` is private on purpose ("reach it through
        // `Config::require_dashboard_url`") and is a `Result`, not an `Option`,
        // so reading the field here is E0616 from this crate.
        dashboard_url: cfg.require_dashboard_url().ok().map(String::from),
        footer: "You are receiving this because you subscribed to notifications in Sauron."
            .to_string(),
    };

    for (user_id, rows) in by_user {
        // A deactivated account must never be mailed, whatever its grants say.
        let user = match repo::find_user_by_id(conn, user_id).await? {
            Some(u) if u.is_active => u,
            _ => {
                let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
                repo::drop_notifications(conn, &ids, "dropped_inactive").await?;
                continue;
            }
        };

        let mut survivors: Vec<NotificationQueueItem> = Vec::new();
        let mut dropped: Vec<Uuid> = Vec::new();
        let mut reaches: HashMap<Uuid, (Reach, Reach)> = HashMap::new();
        for row in rows {
            // Re-derive the org from the PROJECT and treat a mismatch with the
            // stored `org_id` as a hard drop. `reach_for`'s org arm is
            // `Scope::Org(_) => reach.org = true` and never compares the org id,
            // so a diverged denormalized column would set `reach.org` and
            // release a foreign tenant's project.
            let Some((_, true_org)) = orgs.iter().find(|(p, _)| *p == row.project_id).copied()
            else {
                dropped.push(row.id);
                continue;
            };
            if true_org != row.org_id {
                warn!(
                    queue_row = %row.id,
                    stored_org = %row.org_id,
                    true_org = %true_org,
                    "queued notification's denormalized org_id diverged from its project"
                );
                dropped.push(row.id);
                continue;
            }
            // Entry rather than `contains_key` + `insert`: `clippy::map_entry`
            // is denied workspace-wide, and the two-lookup form is what it
            // rejects. The memoization itself is the point — one grant load per
            // (user, org) per batch, not one per queued row.
            if let std::collections::hash_map::Entry::Vacant(slot) = reaches.entry(true_org) {
                let grants =
                    grants_from_rows(repo::user_grants_in_org(conn, user_id, true_org).await?);
                slot.insert((
                    reach_for(&grants, perm::ISSUE_READ),
                    reach_for(&grants, perm::MONITOR_READ),
                ));
            }
            let (issue_reach, monitor_reach) = &reaches[&true_org];
            let envs: Vec<Uuid> = env_rows
                .iter()
                .filter(|(q, _)| *q == row.id)
                .map(|(_, e)| *e)
                .collect();
            let reach = if row.kind == "uptime" {
                monitor_reach
            } else {
                issue_reach
            };
            let ok = covers(
                reach,
                &QueueTarget {
                    project_id: row.project_id,
                    app_id: row.app_id,
                    env_enrollments: &envs,
                    includes_unattributed: row.includes_unattributed,
                },
            );
            if ok {
                survivors.push(row);
            } else {
                // Debug, not warn: losing access is normal, not an anomaly.
                debug!(queue_row = %row.id, user = %user_id, "notification dropped: no access");
                dropped.push(row.id);
            }
        }
        repo::drop_notifications(conn, &dropped, "dropped_no_access").await?;
        if survivors.is_empty() {
            continue;
        }

        let cap = cfg.notify_max_emails_per_user_per_hour.clamp(1, 1000);
        let digest = should_digest(repo::sent_messages_last_hour(conn, user_id).await?, cap);

        // A truncation sentinel must read as the LAST line: "…and more" printed
        // between two issue titles says nothing. `sort_by_key` on a bool is a
        // stable partition, so everything else keeps its claim order, and the
        // ids/count used below are unaffected by a reorder.
        survivors.sort_by_key(|r| r.dedup_key.contains(crate::subs::TRUNCATION_MARKER));

        let first_title = survivors[0].title.clone().unwrap_or_default();
        let mut paragraphs: Vec<String> = survivors
            .iter()
            .map(|row| {
                format!(
                    "{} — {}",
                    row.title.clone().unwrap_or_default(),
                    row.body.clone().unwrap_or_default()
                )
            })
            .collect();
        if digest {
            paragraphs.insert(
                0,
                format!(
                    "You have reached {cap} notification emails this hour, so the rest are \
                     grouped into this one message."
                ),
            );
        }

        // `DASHBOARD_URL` fails CLOSED at point of use: unset means the
        // notification still sends, with the unsubscribe footer replaced by a
        // line telling the user where to manage subscriptions. It never bails.
        let mut footnotes: Vec<String> = Vec::new();
        let mut cta = None;
        match branding.link("/account") {
            Ok(account_url) => {
                cta = sauron_mail::Cta::new("Manage subscriptions", account_url).ok();
                // A fresh token per send, so links in live mail always work and
                // one forwarded into an archive stops working after 90 days.
                let token = sauron_alerts::crypto::unsubscribe_token(
                    unsub_key.as_bytes(),
                    survivors[0].subscription_id,
                    user_id,
                    today,
                );
                if let Ok(url) = branding.link(&format!("/unsubscribe?token={token}")) {
                    footnotes.push(format!("To stop these emails, open {url}"));
                }
            }
            Err(_) => footnotes
                .push("Manage these notifications from your account page in Sauron.".to_string()),
        }

        let content = sauron_mail::MailContent {
            subject: digest_subject(survivors.len(), &first_title),
            heading: digest_subject(survivors.len(), &first_title),
            paragraphs,
            cta,
            footnotes,
        };

        let ids: Vec<Uuid> = survivors.iter().map(|r| r.id).collect();
        match sauron_mail::render(&branding, &content) {
            Ok(rendered) => {
                let recipient_key = user.email.trim().to_lowercase();
                let enqueued = repo::enqueue_mail(
                    conn,
                    NewMailOutbox {
                        kind: sauron_mail::MailKind::PersonalNotification.as_str(),
                        recipient: &user.email,
                        recipient_key: &recipient_key,
                        subject: &rendered.subject,
                        body_text: &rendered.text,
                        body_html: &rendered.html,
                        user_id: Some(user_id),
                    },
                    // Past a day the grants snapshot behind this body is too old
                    // to release, and `claim_due_mail` refuses an expired row.
                    86_400,
                    // Zero, and load-bearing: S3 already de-duplicates twice (the
                    // Redis SET NX EX per (subscription, dedup_key) and the
                    // partial unique index behind it), so a per-recipient
                    // suppression window here could only discard mail that
                    // survived both — silent loss no signal in this design would
                    // reveal.
                    0,
                    true,
                )
                .await;
                match enqueued {
                    Ok(_) => {
                        let message_id = Uuid::new_v4();
                        repo::mark_notifications_sent(conn, &ids, message_id).await?;
                    }
                    Err(e) => {
                        warn!(error = %e, user = %user_id, "enqueueing notification mail failed");
                        repo::fail_notifications(
                            conn,
                            &ids,
                            &e.to_string(),
                            repo::MAX_QUEUE_ATTEMPTS,
                        )
                        .await?;
                    }
                }
            }
            Err(e) => {
                // A render failure is usually deterministic — the same body will
                // fail the same way next pass — so the attempts cap inside
                // `fail_notifications` is what terminates it. Nothing else
                // would: a row returned to `pending` is invisible to
                // `requeue_stuck_notifications`.
                warn!(error = %e, user = %user_id, "notification mail did not render");
                repo::fail_notifications(conn, &ids, &e.to_string(), repo::MAX_QUEUE_ATTEMPTS)
                    .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_over_the_hourly_cap_is_digested_never_dropped() {
        // Quiet hours and the cap both DEFER or MERGE; neither discards,
        // because "quiet" and "broken" must not look identical from the user's
        // side — which for an observability product is the worst available
        // outcome.
        assert!(!should_digest(0, 20));
        assert!(!should_digest(19, 20));
        assert!(should_digest(20, 20));
        assert!(should_digest(100, 20));
    }

    #[test]
    fn the_subject_reflects_how_many_rows_the_message_carries() {
        assert_eq!(digest_subject(1, "New issue: boom"), "New issue: boom");
        assert_eq!(
            digest_subject(3, "New issue: boom"),
            "3 Sauron notifications"
        );
        assert_eq!(digest_subject(0, "fallback"), "fallback");
    }
}
