//! The async `prepare()` pass that runs BEFORE the synchronous `lower()`:
//! everything the planner needs that requires a database round-trip or a
//! clock read, gathered once per request.
//!
//! Three jobs, run in this order:
//!
//! 1. **Reject `Store::Rollup` dimensions** (`environment`/`handled` on
//!    Issues — the `issue_dimensions` rollup table doesn't exist until S3)
//!    with `PlanError::NotYetSupported`, before any query runs. Failing fast
//!    here avoids a pointless environment-lookup round-trip for a query that
//!    can never succeed. `release` on Issues is NOT one of these: it bridges
//!    to `error_events` through a correlated EXISTS instead of waiting on the
//!    rollup, so it is `Store::Column` and reaches `lower()` like any other
//!    predicate.
//! 2. **Batch-resolve every `environment` name** appearing anywhere in the
//!    tree — inside `TypedValue::List`, inside `Or` branches, under `Not` —
//!    with ONE query, rather than one lookup per predicate.
//! 3. **Classify cost and clamp.** `sauron_query::classify(node)`; a
//!    `Cost::Scan` query gets a `Clamp` bounding how far back its window may
//!    reach — unless it is an Issues query whose scan never leaves the
//!    `issues` table (see the nuance note below).
//!
//! ## The Issues/tiering nuance
//!
//! An Issues query whose scanning predicates all hit `issues`' own columns
//! (e.g. a `title` wildcard) never reaches `error_events`, which is the only
//! table the tier worker actually drops rows from — so clamping it would be
//! purely a *cost* safety valve, never a *coverage* necessity, and the cost is
//! not there either: `issues` holds one row per fingerprint, not one per
//! event, and an `is:unresolved title:*boom*` query already scans it
//! unclamped today. A query with free text, a tag predicate, a JSON-root
//! predicate or a wildcard over a bridged column (`release`, `screen`, …), by
//! contrast, becomes a correlated subquery into `error_events` and stays
//! clamped.
//!
//! Telling those apart needs the `Resource` the tree was resolved against (a
//! plain `level:error` predicate is an issues column on `Resource::Issues`
//! but the row itself on `Resource::Occurrences`), which is why `prepare`
//! takes one. The set of local columns is `issues::LOCAL_COLUMNS`, pinned to
//! the `leaf` dispatch by a test there; `sauron_query::scan_rests_outside`
//! does the attribution walk. Every other resource is clamped uniformly,
//! because on those the scanned row IS the tiered row.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use sauron_query::{classify, scan_rests_outside, Cost, ResolvedNode, Resource, Store, TypedValue};

use crate::query_plan::{PlanError, PrepCtx};
use crate::schema::{app_environments, environments};

/// A time-window bound the caller must additionally apply — never a
/// substitute for an explicit `since` the caller already has, only ever a
/// tightening of it.
///
/// `field` names the window generically ("since") rather than as a physical
/// column: mapping it to a concrete column (`issues.last_seen` vs
/// `error_events.occurred_at`) is the caller's job, since it also owns the
/// `since_days` ceiling the clamp is merged with (`search::resolve_window`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clamp {
    pub field: &'static str,
    pub to_days: i64,
    pub reason: &'static str,
}

/// Everything the synchronous `lower()` needs that required an async step.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub ctx: PrepCtx,
    pub cost: Cost,
    pub clamp: Option<Clamp>,
}

/// Run the three jobs above, in order, and assemble the `PrepCtx` the
/// synchronous `lower()` needs.
pub async fn prepare(
    node: &ResolvedNode,
    resource: Resource,
    app_id: Uuid,
    now: DateTime<Utc>,
    conn: &mut AsyncPgConnection,
) -> Result<Prepared, PlanError> {
    // Job 1 — before any query runs.
    reject_rollups(node)?;

    // Job 2 — one round-trip for every environment name anywhere in the tree.
    // De-duplicated here (not inside `collect_environment_names`, which stays
    // a faithful "every occurrence, in order" trace for its own unit tests) so
    // a query repeating `environment:prod` several times still issues one
    // lookup per distinct name, not one per occurrence.
    let mut seen = HashSet::new();
    let names: Vec<String> = collect_environment_names(node)
        .into_iter()
        .filter(|n| seen.insert(n.clone()))
        .collect();
    let environments = resolve_environments(conn, app_id, names).await?;
    let ctx = PrepCtx { environments, now };

    // Job 3 — cost + clamp.
    let cost = classify(node);
    let clamp = clamp_for(node, resource, cost);

    Ok(Prepared { ctx, cost, clamp })
}

// ===========================================================================
// Job 1 — reject Rollup dimensions up front.
// ===========================================================================

/// Walk the whole tree and error on the first `Store::Rollup` dimension
/// found. Run before the environment batch query so a query that can never
/// succeed never causes a database round-trip.
fn reject_rollups(node: &ResolvedNode) -> Result<(), PlanError> {
    match node {
        ResolvedNode::Pred(p) => {
            if matches!(p.dim.store, Store::Rollup) {
                Err(PlanError::NotYetSupported {
                    field: p.dim.name.to_string(),
                })
            } else {
                Ok(())
            }
        }
        ResolvedNode::Text(_) => Ok(()),
        ResolvedNode::Not(inner) => reject_rollups(inner),
        ResolvedNode::And(v) | ResolvedNode::Or(v) => v.iter().try_for_each(reject_rollups),
    }
}

// ===========================================================================
// Job 2 — batch-resolve environment names.
// ===========================================================================

/// Every `environment` value named anywhere in the tree — inside
/// `TypedValue::List` (`environment:[a,b]`), inside `Or` branches, under
/// `Not` — in encounter order, duplicates included. Pure and database-free so
/// it is unit-testable without Postgres; `prepare` de-duplicates the result
/// itself before querying.
///
/// Matches on `Store::Column("environment_id")` — exactly what
/// `OccurrencesLower`/`EventsLower`'s `environment_leaf!` macros match on —
/// rather than on `p.dim.name == "environment"`, so this stays correct by
/// construction even though the catalog also has an `environment` dimension
/// on Issues (`Store::Rollup`, already rejected by job 1 before this runs).
pub(crate) fn collect_environment_names(node: &ResolvedNode) -> Vec<String> {
    let mut out = Vec::new();
    walk_environment_names(node, &mut out);
    out
}

fn walk_environment_names(node: &ResolvedNode, out: &mut Vec<String>) {
    match node {
        ResolvedNode::Pred(p) => {
            if matches!(p.dim.store, Store::Column("environment_id")) {
                collect_value_names(&p.value, out);
            }
        }
        ResolvedNode::Text(_) => {}
        ResolvedNode::Not(inner) => walk_environment_names(inner, out),
        ResolvedNode::And(v) | ResolvedNode::Or(v) => {
            for n in v {
                walk_environment_names(n, out);
            }
        }
    }
}

/// `environment`'s value is a bare `Str` for `Eq`/`Ne`, or a `List` of them
/// for `In`; `Has` carries no value (`Absent`) and contributes nothing.
fn collect_value_names(value: &TypedValue, out: &mut Vec<String>) {
    match value {
        TypedValue::Str(s) => out.push(s.clone()),
        TypedValue::List(items) => {
            for item in items {
                collect_value_names(item, out);
            }
        }
        _ => {}
    }
}

/// One query for every distinct name collected above: `SELECT name, id FROM
/// environments WHERE app_id = $1 AND name = ANY($2)`. A name with no
/// matching row is left `None` — the leaf mappers already lower that to
/// `Uuid::nil()`, which matches nothing, never "no filter" (see
/// `occurrences.rs`/`events.rs`'s `resolve_environment`).
async fn resolve_environments(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    names: Vec<String>,
) -> Result<HashMap<String, Option<Uuid>>, PlanError> {
    // Every collected name starts absent so one missing from the query result
    // (no row for that name in this app) still ends up `None`, not omitted.
    let mut resolved: HashMap<String, Option<Uuid>> =
        names.iter().cloned().map(|n| (n, None)).collect();

    if names.is_empty() {
        return Ok(resolved);
    }

    // The ids wanted here are the ENROLLMENTS', because that is what the event
    // tables store in `environment_id`; the catalogue entry only carries the
    // name being matched.
    //
    // `retired_at IS NULL` on the enrollment is load-bearing: a name is only
    // unique among LIVE rows, so retiring `staging` and creating a fresh
    // `staging` leaves two enrollments reachable by that name. Without this
    // filter, `load()` returns both and whichever is last in the (unordered)
    // result set wins in the map below, so a filter on the current `staging`
    // could silently resolve to the retired row. Retiring a catalogue entry
    // retires its enrollments in the same transaction, so this single predicate
    // covers both levels and cannot disagree with itself.
    let rows: Vec<(String, Uuid)> = app_environments::table
        .inner_join(environments::table.on(environments::id.eq(app_environments::environment_id)))
        .filter(app_environments::app_id.eq(app_id))
        .filter(environments::name.eq_any(&names))
        .filter(app_environments::retired_at.is_null())
        .select((environments::name, app_environments::id))
        .load(conn)
        .await
        .map_err(|e| PlanError::Database(e.to_string()))?;

    for (name, id) in rows {
        resolved.insert(name, Some(id));
    }
    Ok(resolved)
}

// ===========================================================================
// Job 3 — classify and clamp.
// ===========================================================================

/// Clamp every `Cost::Scan` query — except an Issues query whose scanning
/// leaves all live on the `issues` table (see the module-level nuance note).
fn clamp_for(node: &ResolvedNode, resource: Resource, cost: Cost) -> Option<Clamp> {
    if cost != Cost::Scan {
        return None;
    }
    if resource == Resource::Issues && !scan_rests_outside(node, &super::issues::is_local) {
        return None;
    }
    Some(Clamp {
        field: "since",
        to_days: scan_clamp_days(),
        reason: "unindexed predicate (a wildcard, substring, or free-text match) \
                 requires a bounded time window",
    })
}

/// Mirrors `sauron_core::config::Config::search_scan_clamp_days` — read
/// directly here rather than through a `Config` value because `prepare`'s
/// interface (fixed by the task brief) takes no `Config`, and re-running
/// `Config::from_env()` per query would re-validate unrelated settings
/// (`JWT_SECRET`, `CORS_ALLOWED_ORIGINS`, …) for the sake of one integer.
/// Defaults to `TIER_HOT_DAYS`: see the field's doc comment in `config.rs`
/// for why that default is both the honest cost bound and the honest
/// coverage bound.
fn scan_clamp_days() -> i64 {
    let tier_hot_days = std::env::var("TIER_HOT_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    std::env::var("SEARCH_SCAN_CLAMP_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(tier_hot_days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sauron_query::{parse, resolve, Resource};

    // -- The three brief-mandated tests, verbatim ---------------------------

    /// `level` and `environment` are both valid on Occurrences (`level` is
    /// R_ISSUE_OCC, `environment` is R_OCC_EVENTS as a real `Store::Column`),
    /// so this resource lets the test exercise both without hitting job 1's
    /// Rollup rejection.
    fn collect_names(q: &str) -> Vec<String> {
        let node = resolve(&parse(q).unwrap(), Resource::Occurrences).unwrap();
        collect_environment_names(&node)
    }

    #[test]
    fn collects_environment_names_from_inside_lists_and_or_branches() {
        let names = collect_names("environment:[a,b] OR (environment:c level:error)");
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    /// `is:unresolved`/`title` are Issues-only fields, so this helper fixes
    /// the resource to Issues.
    fn clamp_for(q: &str) -> Option<Clamp> {
        clamp_on(q, Resource::Issues)
    }

    fn clamp_on(q: &str, r: Resource) -> Option<Clamp> {
        let node = resolve(&parse(q).unwrap(), r).unwrap();
        super::clamp_for(&node, r, classify(&node))
    }

    #[test]
    fn an_all_scan_query_is_clamped() {
        assert!(clamp_for("boom").is_some());
        assert!(clamp_on("message:*boom*", Resource::Occurrences).is_some());
    }

    // -- The Issues exception ------------------------------------------------

    #[test]
    fn an_issues_scan_confined_to_issues_columns_is_not_clamped() {
        // `title`/`culprit`/`type` are `issues` columns: the scan never
        // reaches the tiered table, so there is nothing to bound.
        assert!(clamp_for("title:*boom*").is_none());
        assert!(clamp_for("culprit:~handler").is_none());
        assert!(clamp_for("title:*boom* type:~Timeout").is_none());
        assert!(clamp_for("!title:*boom*").is_none());
    }

    #[test]
    fn a_bounded_bridge_beside_a_local_scan_does_not_reinstate_the_clamp() {
        // The release switcher ANDs `release:<x>` into every list query; an
        // equality bridge is `Bounded`, not a scan, and its EXISTS is bound by
        // its own `since` bind. The only scanning leaf is still local.
        assert!(clamp_for("title:*boom* release:1.4.0").is_none());
        assert!(clamp_for("title:*boom* screen:/checkout").is_none());
        assert!(clamp_for("title:*boom* tag.region:eu").is_none());
    }

    #[test]
    fn an_issues_scan_that_reaches_error_events_is_still_clamped() {
        // Free text matches the child events' payloads; a tag, JSON-root or
        // bridged-column wildcard is a substring scan over `error_events`.
        assert!(clamp_for("boom").is_some());
        assert!(clamp_for("title:*boom* boom").is_some());
        assert!(clamp_for("tag.region:~eu").is_some());
        assert!(clamp_for("extra.token:~sk_live").is_some());
        assert!(clamp_for("screen:*checkout*").is_some());
        assert!(clamp_for("title:*boom* OR screen:*checkout*").is_some());
    }

    #[test]
    fn the_exception_is_issues_only() {
        // `message` is a scan-class column on Occurrences, but there the
        // scanned row IS the tiered row, so the clamp stands.
        assert!(clamp_on("message:*boom*", Resource::Occurrences).is_some());
        assert!(clamp_on("tag.region:~eu", Resource::Occurrences).is_some());
    }

    #[test]
    fn an_indexed_query_is_not_clamped() {
        assert!(clamp_for("is:unresolved").is_none());
    }

    // -- More coverage on the same three pure parts --------------------------

    #[test]
    fn a_bounded_query_is_not_clamped_either_only_scan_is() {
        // `culprit` is text but plain `Eq` on it is `Bounded`, not `Scan` —
        // the clamp is specifically for `Cost::Scan`, not "anything short of
        // `Indexed`".
        assert!(clamp_for("culprit:handler").is_none());
    }

    #[test]
    fn a_negated_environment_is_still_collected() {
        let names = collect_names("!environment:prod");
        assert_eq!(names, vec!["prod"]);
    }

    #[test]
    fn non_environment_predicates_contribute_no_names() {
        let names = collect_names("level:error message:boom");
        assert!(names.is_empty());
    }

    #[test]
    fn free_text_is_never_mistaken_for_an_environment_value() {
        let names = collect_names("production");
        assert!(names.is_empty());
    }

    #[test]
    fn a_has_predicate_contributes_no_name() {
        // `has:environment` carries `TypedValue::Absent`, not a name.
        let names = collect_names("has:environment");
        assert!(names.is_empty());
    }

    #[test]
    fn rollup_dimensions_are_rejected_before_any_query_would_run() {
        let node = resolve(&parse("environment:production").unwrap(), Resource::Issues).unwrap();
        let err = reject_rollups(&node).unwrap_err();
        assert!(matches!(err, PlanError::NotYetSupported { .. }));
        assert!(err.to_string().contains("environment"));
    }

    #[test]
    fn every_rollup_dimension_on_issues_is_rejected() {
        let q = "handled:true";
        let node = resolve(&parse(q).unwrap(), Resource::Issues).unwrap();
        assert!(
            matches!(
                reject_rollups(&node),
                Err(PlanError::NotYetSupported { .. })
            ),
            "{q}"
        );
    }

    // `release` on Issues is `Store::Column`, not `Store::Rollup` (it bridges
    // to `error_events` through a correlated EXISTS) — this job must let it
    // through rather than rejecting it alongside `handled`.
    #[test]
    fn release_on_issues_is_not_rejected_as_a_rollup() {
        let node = resolve(&parse("release:1.0.0").unwrap(), Resource::Issues).unwrap();
        assert!(reject_rollups(&node).is_ok());
    }

    #[test]
    fn a_rollup_nested_under_or_and_not_is_still_rejected() {
        let node = resolve(
            &parse("level:error OR !environment:production").unwrap(),
            Resource::Issues,
        )
        .unwrap();
        assert!(matches!(
            reject_rollups(&node),
            Err(PlanError::NotYetSupported { .. })
        ));
    }

    #[test]
    fn a_query_with_no_rollup_dimension_passes() {
        let node = resolve(&parse("is:unresolved").unwrap(), Resource::Issues).unwrap();
        assert!(reject_rollups(&node).is_ok());
    }

    #[test]
    fn scan_clamp_days_defaults_to_thirty() {
        // No env vars set in the test process (CI has neither `TIER_HOT_DAYS`
        // nor `SEARCH_SCAN_CLAMP_DAYS`) — pins the fallback-of-a-fallback.
        if std::env::var("TIER_HOT_DAYS").is_err()
            && std::env::var("SEARCH_SCAN_CLAMP_DAYS").is_err()
        {
            assert_eq!(scan_clamp_days(), 30);
        }
    }
}
