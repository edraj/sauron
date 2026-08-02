//! The retro-mask job. Each `inspector_mask_actions` row is simultaneously the
//! queue, the cursor, the progress meter and the audit record.
//!
//! Event tables are append-only, so mask UPDATEs never contend with ingest for
//! row locks. The shared cost is WAL, buffer cache and 13 index updates per
//! `error_events` row: MEASURED 186 us/row on `extra`, 136 us/row on `tags`. A
//! 2000-row batch is ~0.37 s of write; with the 200 ms pause that is a ~65%
//! duty cycle. A 210k-row full pass is ~60 s of write plus roughly a doubling
//! of live tuples until autovacuum catches up, and a pass covers the whole
//! TIER_HOT_DAYS window — budget from the row count that window actually
//! holds, not from a sample day.
//!
//! The job deliberately does NOT run VACUUM — it sets `vacuum_advised` and
//! emits a `warn!`, because an unattended VACUUM is exactly the kind of
//! surprise an operator should authorize.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sauron_core::Config;
use sauron_db::repo::{self, BatchCursor, BatchOutcome};
use sauron_db::{AsyncPgConnection, PgPool};
use sauron_inspector::columns::{self, ColumnKind};
use sauron_inspector::path::parse_mask_path;
use sauron_inspector::targets::{validate_target, MaskTarget};
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{checkout, release};

/// The permission the requester must still hold at claim time. Named here
/// rather than importing `sauron-auth`, so this binary keeps its dependency
/// list to the four crates it actually needs.
const PII_MANAGE: &str = "pii:manage";

/// The oldest instant this pass may write to, recomputed PER DAY.
///
/// Reuses `symbolicate_with`'s expression and its comment "never write into
/// cold/exported partitions": an exported partition already holds the raw
/// bytes in immutable Parquet, so masking the Postgres copy buys nothing while
/// paying the full write cost, and a partition that is exported-but-not-
/// yet-dropped is on the tier worker's critical path.
///
/// A floor computed from `tier_hot_days` alone is NOT sufficient however long
/// the window is, because `sauron-tier` defers the drop to a later cycle than
/// the export. The watermark plus one tier tick is the real boundary.
pub fn day_floor(
    now: DateTime<Utc>,
    tier_hot_days: i64,
    watermark: Option<DateTime<Utc>>,
    tier_tick_secs: i64,
) -> DateTime<Utc> {
    let hot = now - Duration::days(tier_hot_days);
    match watermark {
        Some(w) => hot.max(w + Duration::seconds(tier_tick_secs)),
        None => hot,
    }
}

/// Halve the batch when any target carries a wildcard.
pub fn batch_size(base: i64, targets: &[MaskTarget]) -> i64 {
    if targets.iter().any(|t| t.path.contains("[*]")) {
        (base / 2).max(1)
    } else {
        base
    }
}

/// Deserialize the frozen target list into ENUMS.
///
/// SQL identifiers cannot be bound, so the batch statements interpolate the
/// table and column names. This process is not the one that validated them, so
/// "validated in Rust at write time" is not a control — an unknown pair fails
/// the action instead of reaching an interpolated identifier in an unattended
/// UPDATE running with full DB rights.
pub fn parse_targets(v: &Value) -> Result<Vec<MaskTarget>, String> {
    let arr = v
        .as_array()
        .ok_or_else(|| "targets is not an array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let t: MaskTarget = serde_json::from_value(item.clone())
            .map_err(|e| format!("unknown mask target: {e}"))?;
        validate_target(&t).map_err(|e| format!("invalid mask target: {e:?}"))?;
        out.push(t);
    }
    if out.is_empty() {
        return Err("targets is empty".to_string());
    }
    Ok(out)
}

/// The `text[]` a jsonb target lowers to, plus whether it is a wildcard.
fn path_parts(t: &MaskTarget) -> (Vec<String>, bool) {
    match parse_mask_path(&t.path) {
        Ok(p) if p.wildcard => (p.sub_array(), true),
        Ok(p) => (p.text_array(), false),
        Err(_) => (Vec::new(), false),
    }
}

fn is_text_column(t: &MaskTarget) -> bool {
    columns::find(t.table.as_sql(), t.column.as_sql())
        .map(|c| c.kind == ColumnKind::Text)
        .unwrap_or(false)
}

/// One action per tick, run to completion or to cancellation.
///
/// `LIMIT 1` on the claim is deliberate: masking is heavy write and one action
/// at a time per worker IS the throttle; N workers take N different actions.
pub async fn tick(pool: &PgPool, cfg: &Config, worker_id: &str) -> anyhow::Result<bool> {
    let mut conn = checkout(pool, cfg).await?;
    let claimed =
        repo::claim_mask_action(&mut conn, "mask", worker_id, cfg.inspector_claim_stale_secs)
            .await?;
    let Some(action) = claimed else {
        release(conn).await;
        return Ok(false);
    };

    // AUTHORIZATION IS RE-CHECKED AT CLAIM. Confirm re-authorizes, but the
    // action then sits in `pending` — with one slot per worker and a 200 ms
    // inter-batch pause, a backlog can be hours deep. A member whose grant was
    // revoked, or whose account was deactivated (which revokes refresh tokens
    // and touches nothing queued), must not have their queued destruction run.
    if let Some(user_id) = action.requested_by {
        match repo::user_is_active_with_app_permission(
            &mut conn,
            user_id,
            action.app_id,
            PII_MANAGE,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                repo::fail_mask_action(
                    &mut conn,
                    action.id,
                    "requester no longer holds pii:manage on this app, or is deactivated",
                )
                .await?;
                release(conn).await;
                return Ok(true);
            }
            Err(e) => {
                warn!(action_id = %action.id, error = %e, "could not re-authorize mask requester");
                release(conn).await;
                return Ok(true);
            }
        }
    }

    let targets = match parse_targets(&action.targets) {
        Ok(t) => t,
        Err(reason) => {
            repo::fail_mask_action(&mut conn, action.id, &reason).await?;
            release(conn).await;
            return Ok(true);
        }
    };
    let limit = batch_size(cfg.inspector_mask_batch, &targets);
    let now = Utc::now();
    let mut cold_boundary = day_floor(now, cfg.tier_hot_days, None, cfg.tier_tick_secs as i64);
    let mut cancelled = false;

    // --- phase 'hot'. OLDEST day first, so the rows closest to the tier
    // boundary — the ones about to become permanently unreachable — go first.
    //
    // `<=`, not `<`. TODAY is a maskable day: its partition exists and holds
    // rows the moment an event is ingested. With `<` the retro-mask stopped at
    // yesterday, and the `tail_sweep` phase below does NOT cover the gap — it
    // filters `received_at >= action.started_at`, so it only catches rows that
    // arrive DURING the mask, never the ones already sitting in today's
    // partition when the operator hit confirm. Measured end to end: two rows
    // holding `properties->>'email' = victim@example.com`, dated today, on an
    // action that reported `phase=finished`, `rows_masked=0` and an EMPTY
    // error — an irreversible destruction that the operator confirmed by
    // typing the app slug, reported as success, with the PII still in
    // plaintext. Silent non-destruction is worse than a loud failure here.
    let mut day = action
        .day_cursor
        .unwrap_or_else(|| (now - Duration::days(cfg.tier_hot_days)).date_naive());
    while day <= now.date_naive() && !cancelled {
        // Recomputed PER DAY, watermark re-read with it.
        let wm = repo::get_watermark(&mut conn, "error_events")
            .await
            .unwrap_or(None);
        cold_boundary = day_floor(now, cfg.tier_hot_days, wm, cfg.tier_tick_secs as i64);
        if day.and_hms_opt(0, 0, 0).unwrap().and_utc() < cold_boundary {
            // `cold_rows_skipped` counts ROWS, not days: an operator reading
            // it next to `rows_masked` on a `done` action is comparing two
            // row counts, and the CSV header, the Audit column and the
            // MaskDialog all say rows. Count exactly what this day WOULD have
            // masked, with the same predicates the mask uses.
            let mut skipped: i64 = 0;
            for t in targets.iter().filter(|t| t.table.is_partitioned()) {
                skipped += if is_text_column(t) {
                    repo::count_batch_text(&mut conn, t.table, t.column, action.app_id, day)
                        .await
                        .unwrap_or(0)
                } else {
                    let (path, _) = path_parts(t);
                    if path.is_empty() {
                        0
                    } else {
                        repo::count_batch_jsonb(
                            &mut conn,
                            t.table,
                            t.column,
                            action.app_id,
                            day,
                            &path,
                        )
                        .await
                        .unwrap_or(0)
                    }
                };
            }
            if skipped > 0 {
                repo::add_cold_skip(&mut conn, action.id, skipped).await?;
            }
            day += Duration::days(1);
            continue;
        }
        repo::set_mask_phase(&mut conn, action.id, worker_id, "hot", Some(day)).await?;
        for t in targets.iter().filter(|t| t.table.is_partitioned()) {
            let mut cursor = BatchCursor::default();
            loop {
                let Some(out) = run_partitioned_batch(
                    &mut conn,
                    t,
                    action.app_id,
                    day,
                    cursor,
                    limit,
                    action.id,
                    worker_id,
                )
                .await?
                else {
                    // Fenced out: another worker owns this action now. Abort
                    // rather than retry, or the counters double-count.
                    release(conn).await;
                    return Ok(true);
                };
                if out.status == "cancelling" {
                    cancelled = true;
                    break;
                }
                match out.next_cursor {
                    Some(c) => cursor = c,
                    None => break,
                }
                tokio::time::sleep(std::time::Duration::from_millis(
                    cfg.inspector_mask_pause_ms,
                ))
                .await;
            }
            if cancelled {
                break;
            }
        }
        day += Duration::days(1);
    }

    // --- phase 'default_partition', bounded by the SAME floor.
    //
    // `repo::list_child_partitions` excludes `{table}_default` by design, so
    // those rows are never tiered and never dropped — the longest-lived PII in
    // the system. Rows CANNOT be there inside a covered range (Postgres
    // rejects `CREATE TABLE ... PARTITION OF ...` if the default holds a
    // conflicting row); they are there because their occurred_at is OUTSIDE
    // every explicit range. Which is exactly why the floor still applies: an
    // unbounded sweep would rewrite rows years older than tier_hot_days,
    // contradicting the hot/cold rule and the cold_rows_skipped number.
    if !cancelled {
        repo::set_mask_phase(&mut conn, action.id, worker_id, "default_partition", None).await?;
        for t in targets
            .iter()
            .filter(|t| t.table.is_partitioned() && !is_text_column(t))
        {
            let (path, wildcard) = path_parts(t);
            if wildcard || path.is_empty() {
                continue;
            }
            let mut cursor = BatchCursor::default();
            loop {
                let Some(out) = repo::mask_default_partition_batch(
                    &mut conn,
                    t.table,
                    t.column,
                    action.app_id,
                    cold_boundary,
                    &path,
                    cursor,
                    limit,
                    action.id,
                    worker_id,
                )
                .await?
                else {
                    release(conn).await;
                    return Ok(true);
                };
                if out.status == "cancelling" {
                    cancelled = true;
                    break;
                }
                match out.next_cursor {
                    Some(c) => cursor = c,
                    None => break,
                }
                tokio::time::sleep(std::time::Duration::from_millis(
                    cfg.inspector_mask_pause_ms,
                ))
                .await;
            }
            if cancelled {
                break;
            }
        }
    }

    // --- phase 'companions': one keyset loop per non-partitioned table.
    if !cancelled {
        repo::set_mask_phase(&mut conn, action.id, worker_id, "companions", None).await?;
        for t in targets.iter().filter(|t| !t.table.is_partitioned()) {
            let path = if is_text_column(t) {
                Vec::new()
            } else {
                path_parts(t).0
            };
            let mut after: Option<Uuid> = None;
            loop {
                let Some(out) = repo::mask_rollup_batch(
                    &mut conn,
                    t.table,
                    t.column,
                    action.app_id,
                    &path,
                    after,
                    limit,
                    action.id,
                    worker_id,
                )
                .await?
                else {
                    release(conn).await;
                    return Ok(true);
                };
                if out.status == "cancelling" {
                    cancelled = true;
                    break;
                }
                match out.next_cursor.and_then(|c| c.id) {
                    Some(id) => after = Some(id),
                    None => break,
                }
                tokio::time::sleep(std::time::Duration::from_millis(
                    cfg.inspector_mask_pause_ms,
                ))
                .await;
            }
            if cancelled {
                break;
            }
        }
    }

    // --- phase 'tail_sweep': close the enforcement race ONCE.
    //
    // Between "mask applied" and "every pipeline replica's policy cache
    // refreshes", new rows land unmasked and the retro-mask has already passed
    // them. Keyed on `received_at` while KEEPING an occurred_at range for
    // pruning: `occurred_at` is the CLIENT's timestamp, so a mobile offline
    // queue flushes events whose occurred_at is days old into a partition the
    // day loop already swept.
    if !cancelled {
        repo::set_mask_phase(&mut conn, action.id, worker_id, "tail_sweep", None).await?;
        let received_since = action.started_at.unwrap_or(now);
        let lo = now - Duration::days(cfg.tier_hot_days);
        let hi = now + Duration::days(1);
        for t in targets
            .iter()
            .filter(|t| t.table.is_partitioned() && !is_text_column(t))
        {
            let (path, wildcard) = path_parts(t);
            if wildcard || path.is_empty() {
                continue;
            }
            let mut cursor = BatchCursor::default();
            loop {
                let Some(out) = repo::mask_tail_sweep_batch(
                    &mut conn,
                    t.table,
                    t.column,
                    action.app_id,
                    lo,
                    hi,
                    received_since,
                    &path,
                    cursor,
                    limit,
                    action.id,
                    worker_id,
                )
                .await?
                else {
                    release(conn).await;
                    return Ok(true);
                };
                match out.next_cursor {
                    Some(c) => cursor = c,
                    None => break,
                }
                tokio::time::sleep(std::time::Duration::from_millis(
                    cfg.inspector_mask_pause_ms,
                ))
                .await;
            }
        }
    }

    // Register forward enforcement LAST, so a cancelled or failed pass does not
    // leave the pipeline masking a key the operator stopped masking at rest.
    if !cancelled {
        let rows: Vec<sauron_db::models::NewInspectorMaskedKey> = targets
            .iter()
            .filter(|t| columns::is_maskable_table(t.table.as_sql()))
            .map(|t| sauron_db::models::NewInspectorMaskedKey {
                app_id: action.app_id,
                target_table: t.table.as_sql(),
                target_column: t.column.as_sql(),
                json_path: t.path.as_str(),
                created_by: action.requested_by,
                source_action_id: Some(action.id),
            })
            .collect();
        repo::insert_masked_keys(&mut conn, &rows).await?;
    }

    let refreshed = repo::get_mask_action(&mut conn, action.id).await?;
    let masked = refreshed.map(|a| a.rows_masked).unwrap_or(0);
    let vacuum_advised = masked > 100_000;
    if vacuum_advised {
        warn!(
            action_id = %action.id,
            rows_masked = masked,
            "a large mask pass roughly doubled live tuples; schedule a VACUUM off-peak"
        );
    }
    let status = if cancelled { "cancelled" } else { "done" };
    repo::finish_mask_action(
        &mut conn,
        action.id,
        worker_id,
        status,
        vacuum_advised,
        Some(cold_boundary),
    )
    .await?;
    info!(action_id = %action.id, status, rows_masked = masked, "mask action finished");
    release(conn).await;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn run_partitioned_batch(
    conn: &mut AsyncPgConnection,
    t: &MaskTarget,
    app_id: Uuid,
    day: NaiveDate,
    cursor: BatchCursor,
    limit: i64,
    action_id: Uuid,
    worker_id: &str,
) -> anyhow::Result<Option<BatchOutcome>> {
    if is_text_column(t) {
        return Ok(repo::mask_batch_text(
            conn, t.table, t.column, app_id, day, cursor, limit, action_id, worker_id,
        )
        .await?);
    }
    let (path, wildcard) = path_parts(t);
    if path.is_empty() && !wildcard {
        return Ok(None);
    }
    if wildcard {
        Ok(repo::mask_batch_jsonb_wildcard(
            conn, t.table, t.column, app_id, day, &path, cursor, limit, action_id, worker_id,
        )
        .await?)
    } else {
        Ok(repo::mask_batch_jsonb(
            conn, t.table, t.column, app_id, day, &path, cursor, limit, action_id, worker_id,
        )
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    // Only the tests NAME these two types: the executor reaches them through
    // `MaskTarget`'s fields and never writes the type. Imported at the top of
    // the file they are an `unused_imports` warning, which `-D warnings` makes
    // a hard gate failure.
    use sauron_inspector::targets::{TargetColumn, TargetTable};

    /// The floor is recomputed PER DAY, not once at job start.
    ///
    /// `sauron-tier` defers the drop to a LATER cycle than the export — its own
    /// comment calls this "a real grace window" — and the masker grinds
    /// oldest-day-first for potentially hours. Two silent failures follow from
    /// a floor computed once: the masker updates rows in a partition already
    /// COPY'd to Parquet but not yet dropped, so Postgres shows masked,
    /// Parquet holds raw, and the drop destroys the only masked copy; and a day
    /// dropped mid-run matches zero rows while the action still reports `done`
    /// with `rows_masked > 0`.
    #[test]
    fn the_floor_refuses_a_day_at_or_below_the_watermark() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap();
        let watermark = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 0).unwrap();
        let floor = day_floor(now, 30, Some(watermark), 3600);
        assert!(floor > watermark);
        assert_eq!(floor, watermark + chrono::Duration::seconds(3600));
    }

    #[test]
    fn without_a_watermark_the_floor_is_the_hot_window() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap();
        assert_eq!(
            day_floor(now, 30, None, 3600),
            now - chrono::Duration::days(30)
        );
    }

    /// Wildcard targets halve the batch, because the array rebuild
    /// re-serializes the whole array per row and is measurably more expensive
    /// than the 186 us/row jsonb_set case.
    #[test]
    fn a_wildcard_target_halves_the_batch() {
        let plain = vec![MaskTarget {
            table: TargetTable::ErrorEvents,
            column: TargetColumn::Extra,
            path: "customer.email".into(),
        }];
        // `error_events.breadcrumbs` IS the array, so the path is relative to
        // it and the wildcard is bare — see Task 11.
        let wild = vec![MaskTarget {
            table: TargetTable::ErrorEvents,
            column: TargetColumn::Breadcrumbs,
            path: "[*].data.email".into(),
        }];
        assert_eq!(batch_size(2000, &plain), 2000);
        assert_eq!(batch_size(2000, &wild), 1000);
    }

    /// `targets` is read back out of Postgres in a DIFFERENT PROCESS from the
    /// one that validated it, so an unknown table/column must fail the action
    /// rather than reach an interpolated identifier.
    #[test]
    fn an_unparseable_target_list_is_rejected() {
        let good = serde_json::json!([{"table": "error_events", "column": "extra", "path": "a.b"}]);
        assert!(parse_targets(&good).is_ok());
        let bad = serde_json::json!([{"table": "auth_sessions", "column": "token", "path": ""}]);
        assert!(parse_targets(&bad).is_err());
        let alsobad =
            serde_json::json!([{"table": "error_events", "column": "extra", "path": "a.3.b"}]);
        assert!(parse_targets(&alsobad).is_err());
        assert!(parse_targets(&serde_json::json!([])).is_err());
    }
}
