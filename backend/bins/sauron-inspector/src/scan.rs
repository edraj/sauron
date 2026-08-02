//! The scan executor: recompute a frozen scan's units, run ONE unit per
//! tick, flush, yield.
//!
//! One unit per tick rather than one whole scan, so the tick is short and the
//! lease heartbeat is frequent. A scan that has held its lease for a full
//! `INSPECTOR_LEASE_SECS` without finishing is a bug, not a design.

use chrono::Duration;
use sauron_core::Config;
use sauron_db::models::InspectorPolicy;
use sauron_db::repo::{self, FindingDelta};
use sauron_db::{AsyncPgConnection, PgPool};
use sauron_inspector::columns;
use sauron_inspector::detect::{self, Detector};
use sauron_inspector::matching::{self, TrackedKey};
use sauron_inspector::prefilter;
use sauron_inspector::redact;
use sauron_inspector::targets::{PolicyTargetType, ScanPair};
use sauron_inspector::units::{units_for, Unit};
use sauron_inspector::walk;
use serde_json::json;
use std::collections::HashMap;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{checkout, release};

/// Freeze a policy into a scan row for the scheduler. Returns whether a scan
/// was actually queued.
pub async fn enqueue_for_policy(
    conn: &mut AsyncPgConnection,
    cfg: &Config,
    policy: &InspectorPolicy,
    trigger: &str,
    requested_by: Option<Uuid>,
) -> anyhow::Result<bool> {
    match repo::enqueue_scan_for_policy(conn, cfg, policy, trigger, requested_by).await? {
        repo::EnqueueOutcome::Queued(scan) => {
            info!(scan_id = %scan.id, units = scan.units_total, "queued inspector scan");
            Ok(true)
        }
        repo::EnqueueOutcome::AlreadyActive => Ok(false),
        repo::EnqueueOutcome::TargetGone => {
            warn!(policy_id = %policy.id, "policy target is no longer in its org; not scanning");
            Ok(false)
        }
        // Rejected at the API with a 400; if one reaches here it must not
        // produce a confident false negative.
        repo::EnqueueOutcome::NoMatchers => {
            warn!(policy_id = %policy.id, "policy has neither tracked keys nor detectors; not scanning");
            Ok(false)
        }
        repo::EnqueueOutcome::FullySubtracted => {
            warn!(policy_id = %policy.id, "every target pair is covered by a narrower policy");
            Ok(false)
        }
    }
}

/// The phase-2 accumulator key. Bounded by keys x columns per unit, which is
/// what keeps worker RSS flat regardless of scan size.
type AccKey = (String, String, String, String);

/// One unit per tick.
pub async fn tick(pool: &PgPool, cfg: &Config, worker_id: &str) -> anyhow::Result<bool> {
    let mut conn = checkout(pool, cfg).await?;
    let claimed = repo::claim_one_scan(&mut conn, worker_id, cfg.inspector_lease_secs).await?;
    let Some(scan) = claimed else {
        release(conn).await;
        return Ok(false);
    };

    if scan.attempts > cfg.inspector_max_attempts {
        repo::finish_scan(
            &mut conn,
            scan.id,
            worker_id,
            "failed",
            "partial",
            "",
            "exceeded INSPECTOR_MAX_ATTEMPTS; one unit is failing repeatedly",
        )
        .await?;
        release(conn).await;
        return Ok(true);
    }

    // Recompute the unit list from the FROZEN inputs. Nothing about the live
    // policy is read here.
    let pairs: Vec<ScanPair> = scan
        .targets
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let a = p.get(0)?.as_str()?.parse().ok()?;
                    let e = p
                        .get(1)
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok());
                    Some(ScanPair {
                        app_id: a,
                        app_env_id: e,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let tables: Vec<String> = scan.params["tables"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let level = scan.params["level"]
        .as_str()
        .and_then(PolicyTargetType::from_sql)
        .unwrap_or(PolicyTargetType::App);
    let units = units_for(&pairs, &tables, scan.window_from, scan.window_to, level);

    let idx = scan.cursor["unit_index"].as_u64().unwrap_or(0) as usize;
    if idx >= units.len() {
        let coverage = if scan.coverage == "partial" {
            "partial"
        } else {
            "full"
        };
        repo::finish_scan(
            &mut conn,
            scan.id,
            worker_id,
            "succeeded",
            coverage,
            &scan.coverage_note,
            "",
        )
        .await?;
        release(conn).await;
        return Ok(true);
    }

    let keys = matching::parse_tracked_keys(&scan.params["tracked_keys"]);
    let dets = detect::parse_detectors(&scan.params["detectors"]);
    let unit = units[idx].clone();
    let outcome = run_unit(&mut conn, cfg, &scan, &unit, &keys, &dets, idx, worker_id).await;
    release(conn).await;

    match outcome {
        Ok(Some(cancelled)) if cancelled => {
            let mut conn = checkout(pool, cfg).await?;
            repo::finish_scan(
                &mut conn,
                scan.id,
                worker_id,
                "cancelled",
                "partial",
                "stopped by an operator",
                "",
            )
            .await?;
            release(conn).await;
        }
        // The fence rejected the flush: another worker owns this scan now.
        // Abort the unit rather than retrying, or `match_count +
        // excluded.match_count` double-counts.
        Ok(None) => warn!(scan_id = %scan.id, "flush fenced out; another worker owns this scan"),
        Ok(Some(_)) => {}
        Err(e) => {
            warn!(scan_id = %scan.id, unit = idx, error = %e, "scan unit failed");
            let mut conn = checkout(pool, cfg).await?;
            repo::note_scan_coverage(&mut conn, scan.id, "partial", &format!("unit {idx} failed"))
                .await?;
            release(conn).await;
        }
    }

    // The duty cycle. The whole feature is off by default, work proceeds in
    // INSPECTOR_BATCH_ROWS chunks, and this pause is what keeps the ingest
    // working set resident — the risk is buffer-cache eviction and CPU, not
    // lock contention (a seq scan takes ACCESS SHARE, which does not conflict
    // with INSERT's ROW EXCLUSIVE).
    tokio::time::sleep(std::time::Duration::from_millis(
        cfg.inspector_batch_pause_ms,
    ))
    .await;
    Ok(true)
}

/// Run one unit to completion. `Ok(None)` = fenced out; `Ok(Some(true))` =
/// cancellation requested.
#[allow(clippy::too_many_arguments)]
async fn run_unit(
    conn: &mut AsyncPgConnection,
    cfg: &Config,
    scan: &sauron_db::models::InspectorScan,
    unit: &Unit,
    keys: &[TrackedKey],
    dets: &[Detector],
    idx: usize,
    worker_id: &str,
) -> anyhow::Result<Option<bool>> {
    let (table, app_id) = match unit {
        Unit::Ranged { table, app_id, .. }
        | Unit::DefaultSweep { table, app_id }
        | Unit::Rollup { table, app_id } => (table.clone(), *app_id),
    };

    // The policy's OPT-IN column set, frozen into params at enqueue. Reading
    // it is what makes `breadcrumbs`, `sdk`, `debug_meta`, `stacktrace`,
    // `stacktrace_symbolicated`, `identities.alias_id`/`distinct_id` and
    // `workflows.cancel_reason` reachable at all — every one of them is
    // `default_on: false`, so `default_columns` alone can never return them
    // and a rollup unit for `identities`/`workflows` would scan nothing.
    // NULL (the shipped default) means "the default set".
    let cols: Vec<&'static columns::ScanColumn> = match scan.params["scan_columns"].as_array() {
        Some(names) => names
            .iter()
            .filter_map(|v| v.as_str())
            // `find` is the allowlist: a name from a downgraded binary or a
            // hand-edited row is dropped, never interpolated.
            .filter_map(|n| columns::find(&table, n))
            .collect(),
        None => columns::default_columns(&table),
    };
    // Nothing to read for THIS table, which is the normal case for an explicit
    // `scan_columns`: no column name is shared by all five scanned tables, so
    // `["tags", "stacktrace_symbolicated"]` leaves `transactions` (which only
    // exposes `url`) with an empty set. Returning early WITHOUT flushing left
    // the cursor on that unit, and since every claim does `attempts + 1` while
    // only a flush resets it, the scan re-claimed the same unit until
    // INSPECTOR_MAX_ATTEMPTS and finalized `failed` — measured directly:
    // units_done stuck at 30/47, "exceeded INSPECTOR_MAX_ATTEMPTS; one unit is
    // failing repeatedly". That wedge is on the ONLY path to `breadcrumbs`,
    // `sdk`, `debug_meta`, `stacktrace`, `stacktrace_symbolicated`,
    // `identities.*` and `workflows.cancel_reason`, all of which are
    // `default_on: false`. The unit really is complete, so flush it as such:
    // zero rows, zero findings, cursor advanced, attempts reset.
    if cols.is_empty() {
        let flushed = repo::flush_scan_unit(
            conn,
            scan.id,
            worker_id,
            &json!({"unit_index": idx + 1}),
            (idx + 1) as i32,
            0,
            &[],
        )
        .await?;
        return Ok(flushed.map(|o| o.cancel_requested_at.is_some()));
    }
    let (patterns, text_patterns) = if prefilter::use_prefilter(keys, dets) {
        (
            prefilter::key_patterns(keys),
            prefilter::text_key_patterns(keys),
        )
    } else {
        (Vec::new(), Vec::new())
    };

    // The default child has no time bound at all — that is the whole point of
    // sweeping it — so it gets its own budget instead of the per-unit one.
    let row_cap = match unit {
        Unit::DefaultSweep { .. } => cfg.inspector_default_sweep_rows,
        _ => cfg.inspector_max_phase2_rows_per_unit,
    };

    let mut acc: HashMap<AccKey, FindingDelta> = HashMap::new();
    let mut rows_seen: i64 = 0;
    let mut truncated = false;
    let mut cursor = repo::ScanCursor::default();

    loop {
        let page = repo::scan_window_rows(
            conn,
            &table,
            &cols.iter().map(|c| c.column).collect::<Vec<_>>(),
            app_id,
            unit_shape(unit),
            cursor,
            cfg.inspector_batch_rows,
            &patterns,
            &text_patterns,
        )
        .await?;
        if page.is_empty() {
            break;
        }
        rows_seen += page.len() as i64;
        for row in &page {
            cursor = repo::ScanCursor {
                occurred_at: row.occurred_at,
                id: Some(row.id),
            };
            accumulate(&mut acc, scan, unit, &table, row, keys, dets);
        }
        if rows_seen >= row_cap {
            // Hitting the cap sets match_count_exact = false on this unit's
            // findings and coverage = 'partial' — never a silent truncation.
            truncated = true;
            break;
        }
        if page.len() < cfg.inspector_batch_rows as usize {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(
            cfg.inspector_batch_pause_ms,
        ))
        .await;
    }

    let mut deltas: Vec<FindingDelta> = acc.into_values().collect();
    if truncated {
        for d in &mut deltas {
            d.match_count_exact = false;
        }
    }
    let flushed = repo::flush_scan_unit(
        conn,
        scan.id,
        worker_id,
        &json!({"unit_index": idx + 1}),
        (idx + 1) as i32,
        rows_seen,
        &deltas,
    )
    .await?;
    if truncated {
        let key = match unit {
            Unit::DefaultSweep { .. } => "INSPECTOR_DEFAULT_SWEEP_ROWS",
            _ => "INSPECTOR_MAX_PHASE2_ROWS_PER_UNIT",
        };
        repo::note_scan_coverage(
            conn,
            scan.id,
            "partial",
            &format!("a unit hit {key}; counts are lower bounds"),
        )
        .await?;
    }
    Ok(flushed.map(|o| o.cancel_requested_at.is_some()))
}

/// Which statement shape a unit reads with.
///
/// `DefaultSweep` reads the `_default` CHILD BY NAME with no time predicate
/// at all. Re-running the parent over the scan window — which is what a
/// `Some((window_from, window_to))` range would do — reads exactly the rows
/// the `Ranged` units already read (double-counting `match_count` and
/// `rows_scanned`) while pruning away the only rows this phase exists for:
/// rows are in the default child PRECISELY BECAUSE their `occurred_at` falls
/// outside every explicit range, so a windowed query can never see them.
///
/// `Rollup` reads a non-partitioned companion. `issues`, `event_users` and
/// `identities` have neither an `occurred_at` nor an `environment_id` column,
/// so any predicate on either is `column "occurred_at" does not exist` — and
/// `inspector_policies.rollups` defaults to `["issues","event_users"]`, so
/// that fires on the shipped default policy.
fn unit_shape(unit: &Unit) -> repo::ScanShape {
    match unit {
        Unit::Ranged { day, env_id, .. } => {
            let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
            repo::ScanShape::Ranged {
                env_id: *env_id,
                from: lo,
                to: lo + Duration::days(1),
            }
        }
        Unit::DefaultSweep { .. } => repo::ScanShape::DefaultChild,
        Unit::Rollup { .. } => repo::ScanShape::Rollup,
    }
}

/// Phase 2: parse only the rows that survived the prefilter, walk each scanned
/// column, and fold matches into the accumulator.
fn accumulate(
    acc: &mut HashMap<AccKey, FindingDelta>,
    scan: &sauron_db::models::InspectorScan,
    unit: &Unit,
    table: &str,
    row: &repo::ScanRow,
    keys: &[TrackedKey],
    dets: &[Detector],
) {
    let (env_scope, environment_id) = match unit {
        Unit::Rollup { .. } => ("no_env_column", None),
        Unit::Ranged {
            env_id: Some(e), ..
        } => ("enrollment", Some(*e)),
        _ => ("unattributed", None),
    };
    let partition_kind = match unit {
        Unit::Ranged { .. } => "ranged",
        Unit::DefaultSweep { .. } => "default",
        Unit::Rollup { .. } => "rollup",
    };

    for (column, value) in &row.columns {
        // A TEXT column arrives as `to_jsonb(col)`, i.e. a JSON SCALAR, and
        // the walker returns nothing for a scalar root (its own test asserts
        // `walk(&json!("[Circular]")).is_empty()`). Without this branch none
        // of the ten `default_on` TEXT columns — `error_events.title`,
        // `culprit`, `message`, `exception_value`, `exception_type`,
        // `issues.title`, `culprit`, `transactions.url` — could EVER produce
        // a finding, and those are exactly what the Issues list renders. The
        // column name is the key; there is no path inside a scalar.
        let leaves: Vec<walk::Leaf<'_>> = if value.is_object() || value.is_array() {
            walk::walk(value)
        } else {
            vec![walk::Leaf {
                path: String::new(),
                key: column.to_lowercase(),
                value,
            }]
        };
        for leaf in leaves {
            let (matched_key, detector) = match matching::matched(keys, &leaf) {
                Some(k) => (k.key.clone(), String::new()),
                None => match leaf
                    .value
                    .as_str()
                    .and_then(|s| detect::detect_first(dets, s))
                {
                    Some(d) => (leaf.key.clone(), d.id().to_string()),
                    None => continue,
                },
            };
            // key_path is UNTRUSTED INPUT: object keys are arbitrary
            // dev-controlled UTF-8, so a payload shaped
            // `extra.customers["jane@acme.com"].email` would write raw PII
            // straight into a column every pii:read holder can read with no
            // reveal call and no audit row.
            let key_path = redact::redact_path(&leaf.path);
            let k: AccKey = (
                column.clone(),
                key_path.clone(),
                matched_key.clone(),
                detector.clone(),
            );
            let entry = acc.entry(k).or_insert_with(|| FindingDelta {
                org_id: scan.org_id,
                app_id: match unit {
                    Unit::Ranged { app_id, .. }
                    | Unit::DefaultSweep { app_id, .. }
                    | Unit::Rollup { app_id, .. } => *app_id,
                },
                environment_id,
                env_scope: env_scope.to_string(),
                source_table: table.to_string(),
                source_column: column.clone(),
                key_path,
                matched_key,
                detector,
                value_type: redact::value_type(leaf.value).to_string(),
                match_count: 0,
                match_count_exact: true,
                sample_preview: redact::preview(leaf.value),
                sample_row_id: Some(row.id),
                // NULL on a rollup: `issues`, `event_users` and `identities`
                // have no `occurred_at` column to read one from.
                sample_occurred_at: row.occurred_at,
                partition_kind: partition_kind.to_string(),
                first_seen_at: row.occurred_at,
                last_seen_at: row.occurred_at,
            });
            entry.match_count += 1;
            if let Some(ts) = row.occurred_at {
                if entry.first_seen_at.is_none_or(|f| ts < f) {
                    entry.first_seen_at = Some(ts);
                }
                if entry.last_seen_at.is_none_or(|l| ts > l) {
                    entry.last_seen_at = Some(ts);
                }
            }
        }
    }
}
