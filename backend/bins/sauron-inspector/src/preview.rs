//! The preview executor: the identical day loop with `count(*)` instead of
//! UPDATE.
//!
//! Counting `col #> path IS NOT NULL` over an app's hot window is a Parallel
//! Append seq scan — 184 ms per 210k rows measured — with no index that can
//! serve it, since the tags GIN is `jsonb_path_ops` and answers `@>` only.
//! Running that on the API's 16-connection pool is how the whole dashboard
//! goes down, so `POST /mask-preview` returns 202 and the dashboard polls.
//! The preview is auditable for free.
//!
//! There is NO synchronous upper bound. `repo::hot_rows_by_app_scoped` looks
//! like one but is `SELECT app_id, count(*) ... GROUP BY app_id` with NO time
//! predicate, counting every hot row the app ever wrote across all ~20
//! children; its only existing caller runs it on a dedicated connection behind
//! a 60 s Redis cache. Called uncached from every MaskDialog open it holds a
//! pooled connection for tens of seconds — the exact pattern this module
//! exists to avoid. The dialog shows "Counting…" until the worker answers.

use chrono::{Duration, Utc};
use sauron_core::Config;
use sauron_db::repo;
use sauron_db::PgPool;
use sauron_inspector::columns::{self, ColumnKind};
use sauron_inspector::path::parse_mask_path;
use tracing::{info, warn};

use crate::mask::{day_floor, parse_targets};
use crate::{checkout, release};

pub async fn tick(pool: &PgPool, cfg: &Config, worker_id: &str) -> anyhow::Result<bool> {
    let mut conn = checkout(pool, cfg).await?;
    let claimed = repo::claim_mask_action(
        &mut conn,
        "preview",
        worker_id,
        cfg.inspector_claim_stale_secs,
    )
    .await?;
    let Some(action) = claimed else {
        release(conn).await;
        return Ok(false);
    };

    let targets = match parse_targets(&action.targets) {
        Ok(t) => t,
        Err(reason) => {
            repo::fail_mask_action(&mut conn, action.id, &reason).await?;
            release(conn).await;
            return Ok(true);
        }
    };

    let now = Utc::now();
    let wm = repo::get_watermark(&mut conn, "error_events")
        .await
        .unwrap_or(None);
    let cold_boundary = day_floor(now, cfg.tier_hot_days, wm, cfg.tier_tick_secs as i64);

    let mut estimated: i64 = 0;
    // ROWS, not days: `finish_preview` writes this to `cold_rows_skipped`,
    // and the dialog renders it as a row count next to `estimated_rows`.
    let mut cold_rows: i64 = 0;
    // `<=` mirrors the mask's day loop exactly. These two must agree or the
    // dialog's "N rows will be masked" is a number about a different set of
    // rows than the one the mask touches; with `<` both skipped today, so a
    // preview over an app whose only PII arrived today counted 0 and the
    // confirmation dialog told the operator there was nothing to destroy.
    let mut day = (now - Duration::days(cfg.tier_hot_days)).date_naive();
    while day <= now.date_naive() {
        // A cold day is still COUNTED — the count is what makes the dialog's
        // "N rows are already in cold storage and will not be masked" honest —
        // it is just counted into a different bucket.
        let cold = day.and_hms_opt(0, 0, 0).unwrap().and_utc() < cold_boundary;
        let mut day_total: i64 = 0;
        for t in targets.iter().filter(|t| t.table.is_partitioned()) {
            let is_text = columns::find(t.table.as_sql(), t.column.as_sql())
                .map(|c| c.kind == ColumnKind::Text)
                .unwrap_or(false);
            let n = if is_text {
                repo::count_batch_text(&mut conn, t.table, t.column, action.app_id, day).await
            } else {
                match parse_mask_path(&t.path) {
                    // A wildcard's exact count needs the same array rebuild the
                    // mask does; the containment count over the sub-path is the
                    // honest lower bound, and the dialog labels it "up to".
                    Ok(p) => {
                        let path = if p.wildcard {
                            p.sub_array()
                        } else {
                            p.text_array()
                        };
                        repo::count_batch_jsonb(
                            &mut conn,
                            t.table,
                            t.column,
                            action.app_id,
                            day,
                            &path,
                        )
                        .await
                    }
                    Err(_) => Ok(0),
                }
            };
            match n {
                Ok(v) => day_total += v,
                Err(e) => {
                    warn!(action_id = %action.id, error = %e, "preview count failed for a day")
                }
            }
        }
        if cold {
            cold_rows += day_total;
        } else {
            estimated += day_total;
        }
        day += Duration::days(1);
        tokio::time::sleep(std::time::Duration::from_millis(
            cfg.inspector_batch_pause_ms,
        ))
        .await;
    }

    repo::finish_preview(
        &mut conn,
        action.id,
        worker_id,
        estimated,
        cold_rows,
        Some(cold_boundary),
    )
    .await?;
    info!(action_id = %action.id, estimated, cold_rows, "mask preview complete");
    release(conn).await;
    Ok(true)
}
