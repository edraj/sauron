//! The revocation sweep: self-disable subscriptions whose owner lost reach.
//!
//! Lives in the library crate, not in `sauron-api`'s route tree, because it has
//! two callers in two processes — the grant-mutation handlers (synchronous) and
//! the `sauron-alerts` daily slot (the backstop). One copy, one predicate.

// `anyhow::Result`, not `QueryResult`: `sauron-alerts` has no direct diesel
// dependency (`sauron-db` re-exports `AsyncPgConnection` precisely so it does
// not need one), and `diesel::result::Error` converts into `anyhow::Error`
// through the blanket impl, so `?` still works on every `repo::` call. On the
// `sauron-api` side `impl From<anyhow::Error> for ApiError` (`error.rs:64`)
// makes the call sites unchanged.
use sauron_auth::rbac::{grants_from_rows, perm, reach_for};
use sauron_db::{repo, AsyncPgConnection};
use uuid::Uuid;

use crate::subscription::{covers, QueueTarget, SubKind};

/// Re-evaluate one user's subscriptions in one org and self-disable the ones
/// they can no longer reach. Returns how many were disabled.
///
/// Called synchronously from every grant-mutation site — the same request,
/// after the change commits — because a daily pass alone leaves a 24-hour
/// window in which a revoked member keeps receiving telemetry.
///
/// This deliberately does NOT ask "does this user still have any grants in the
/// org". The overwhelmingly common revocation is partial — moved off a project,
/// an env grant narrowed, a role downgraded so it no longer carries
/// `issue:read` — and in every one of those the answer is still yes.
///
/// Logs at debug, not warn: losing access is normal, not a fault.
pub async fn sweep_user_subscriptions(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> anyhow::Result<usize> {
    let subs = repo::subscriptions_for_user_in_org(conn, user_id, org_id).await?;
    if subs.is_empty() {
        return Ok(0);
    }
    let grants = grants_from_rows(repo::user_grants_in_org(conn, user_id, org_id).await?);
    let issue_reach = reach_for(&grants, perm::ISSUE_READ);
    let monitor_reach = reach_for(&grants, perm::MONITOR_READ);

    let ids: Vec<Uuid> = subs.iter().map(|s| s.id).collect();
    let env_rows = repo::subscription_envs_for(conn, &ids).await?;

    // Batched exactly like the evaluation pass: the daily backstop calls this
    // once per (user, org) pair in the whole install, so a per-subscription
    // query here is a per-subscription query across the entire table.
    let mut app_scope_ids: Vec<Uuid> = Vec::new();
    let mut project_scope_ids: Vec<Uuid> = Vec::new();
    for s in subs.iter().filter(|s| s.kind != "uptime") {
        match s.scope_type.as_str() {
            "app" => app_scope_ids.push(s.scope_id),
            _ => project_scope_ids.push(s.scope_id),
        }
    }
    app_scope_ids.sort_unstable();
    app_scope_ids.dedup();
    project_scope_ids.sort_unstable();
    project_scope_ids.dedup();
    let live_app_scopes = repo::app_ancestries(conn, &app_scope_ids).await?;
    let project_apps = repo::apps_for_projects(conn, &project_scope_ids).await?;

    let mut all_apps: Vec<Uuid> = live_app_scopes.iter().map(|(a, _, _)| *a).collect();
    all_apps.extend(project_apps.iter().map(|(_, a)| *a));
    all_apps.sort_unstable();
    all_apps.dedup();
    let enrollments = repo::live_enrollments_for_apps(conn, &all_apps).await?;

    let mut disabled = 0usize;
    for s in &subs {
        let Some(kind) = SubKind::parse(&s.kind) else {
            continue;
        };
        let still_covered = if kind == SubKind::Uptime {
            // Uptime is authorized at project scope only, exactly as every
            // monitor endpoint is.
            monitor_reach.org || monitor_reach.projects.contains(&s.scope_id)
        } else {
            // `scope_id` has no FK, so the target can be gone. A subscription
            // pointing at nothing can never fire; disable it rather than leave
            // it enabled forever.
            let (project_id, app_ids): (Uuid, Vec<Uuid>) = match s.scope_type.as_str() {
                "app" => match live_app_scopes.iter().find(|(a, _, _)| *a == s.scope_id) {
                    Some((_, project_id, _)) => (*project_id, vec![s.scope_id]),
                    None => (Uuid::nil(), Vec::new()),
                },
                _ => (
                    s.scope_id,
                    project_apps
                        .iter()
                        .filter(|(project_id, _)| *project_id == s.scope_id)
                        .map(|(_, app_id)| *app_id)
                        .collect(),
                ),
            };
            if app_ids.is_empty() {
                false
            } else {
                let catalogue: Vec<Uuid> = env_rows
                    .iter()
                    .filter(|(sid, _)| *sid == s.id)
                    .map(|(_, e)| *e)
                    .collect();
                // Catalogue ids cross to ENROLLMENT ids here. `Reach.envs` holds
                // enrollment ids; a catalogue id compared against it matches
                // nothing and would silently disable every env-narrowed
                // subscription in the install.
                let sub_enrollments: Vec<Uuid> = if catalogue.is_empty() {
                    Vec::new()
                } else {
                    enrollments
                        .iter()
                        .filter(|(_, app, c)| app_ids.contains(app) && catalogue.contains(c))
                        .map(|(e, _, _)| *e)
                        .collect()
                };
                app_ids.iter().all(|app_id| {
                    covers(
                        &issue_reach,
                        &QueueTarget {
                            project_id,
                            app_id: Some(*app_id),
                            env_enrollments: &sub_enrollments,
                            includes_unattributed: sub_enrollments.is_empty(),
                        },
                    )
                })
            }
        };
        if !still_covered {
            repo::disable_subscription(conn, s.id, "access_revoked").await?;
            tracing::debug!(
                subscription = %s.id,
                user = %user_id,
                org = %org_id,
                "subscription disabled: owner no longer reaches its scope"
            );
            disabled += 1;
        }
    }
    Ok(disabled)
}

/// The daily backstop: re-evaluate EVERY enabled subscription.
///
/// The three synchronous call sites in `routes/orgs.rs` cover the grant
/// mutations a human performs deliberately. They do not cover a role's
/// permission list being edited, a project being deleted, or an app being
/// removed — the paths nobody remembered. This pass is what catches those, at
/// the cost of a 24-hour worst case.
///
/// Grouped by `(user_id, org_id)` so the grant load and the batched scope
/// resolution are paid once per pair rather than once per subscription.
pub async fn sweep_revoked_subscriptions(conn: &mut AsyncPgConnection) -> anyhow::Result<usize> {
    let all = repo::enabled_subscriptions_all(conn).await?;
    let mut pairs: Vec<(Uuid, Uuid)> = all.iter().map(|s| (s.user_id, s.org_id)).collect();
    pairs.sort_unstable();
    pairs.dedup();

    let mut disabled = 0usize;
    for (user_id, org_id) in pairs {
        // One tenant's failure must not abandon the rest of the pass. This is
        // the LAST line of defence against a member who kept receiving
        // telemetry after losing access; `?` here would let a single unlucky
        // row silence the backstop for the entire install, once a day, forever.
        match sweep_user_subscriptions(conn, user_id, org_id).await {
            Ok(n) => disabled += n,
            Err(e) => tracing::warn!(
                error = %e,
                user = %user_id,
                org = %org_id,
                "revocation sweep failed for one user"
            ),
        }
    }
    Ok(disabled)
}
