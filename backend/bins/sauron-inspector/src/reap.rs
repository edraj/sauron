//! Retention, on its own hourly cadence, never inside a scan or mask tick.
//!
//! Two independent bounds on findings, because one is not enough: a nightly
//! scan producing 33k findings is 12M rows a year — the exact failure
//! `alert_events`' reaper doc comment warns about.

use sauron_core::Config;
use sauron_db::repo;
use sauron_db::PgPool;
use tracing::{info, warn};

/// Bounded delete batch. The house prune idiom has no LIMIT, and an unbounded
/// cascading DELETE of up to 660k findings is a bloat and lock spike.
const DELETE_BATCH: i64 = 5_000;

pub async fn tick(pool: &PgPool, cfg: &Config, _worker_id: &str) -> anyhow::Result<bool> {
    let mut conn = crate::checkout(pool, cfg).await?;

    match repo::prune_inspector_scans(&mut conn, cfg.inspector_scan_keep, DELETE_BATCH).await {
        Ok(n) if n > 0 => info!(pruned = n, "pruned old inspector scans"),
        Ok(_) => {}
        Err(e) => warn!(error = %e, "pruning inspector scans failed"),
    }

    match repo::prune_inspector_findings(
        &mut conn,
        cfg.inspector_finding_retention_days,
        DELETE_BATCH,
    )
    .await
    {
        Ok(n) if n > 0 => info!(pruned = n, "pruned old inspector findings"),
        Ok(_) => {}
        Err(e) => warn!(error = %e, "pruning inspector findings failed"),
    }

    // Abandoned previews are not audit-relevant, so this ALWAYS runs.
    match repo::prune_mask_previews(&mut conn, cfg.inspector_preview_gc_days).await {
        Ok(n) if n > 0 => info!(pruned = n, "pruned abandoned mask previews"),
        Ok(_) => {}
        Err(e) => warn!(error = %e, "pruning mask previews failed"),
    }

    // Defaults to 0 = NEVER. This table grows per HUMAN ACTION, not per rule
    // evaluation, and it is the record a compliance question is answered from.
    match repo::prune_mask_actions(&mut conn, cfg.inspector_audit_retention_days, DELETE_BATCH)
        .await
    {
        Ok(n) if n > 0 => info!(pruned = n, "pruned terminal mask actions"),
        Ok(_) => {}
        Err(e) => warn!(error = %e, "pruning mask actions failed"),
    }

    // Repairs orphans that predate the cascade in `repo::delete_app` /
    // `delete_project`, plus the paths those two do not own (a direct SQL
    // delete, a retired enrollment). An orphan is not merely untidy: it stays
    // LISTED at `GET /v1/orgs/{org}/inspector/policies` while
    // `DELETE /v1/inspector/policies/{id}` answers 404 forever, because that
    // handler authorizes through an app that no longer exists.
    match repo::prune_orphaned_inspector_policies(&mut conn, DELETE_BATCH).await {
        Ok(n) if n > 0 => info!(pruned = n, "pruned orphaned inspector policies"),
        Ok(_) => {}
        Err(e) => warn!(error = %e, "pruning orphaned inspector policies failed"),
    }

    // Without this the privacy feature is the only UN-ERASABLE store of staff
    // PII in the schema: everywhere else a user row cascades, so deleting a
    // user is the product's de-facto erasure mechanism, and ON DELETE SET NULL
    // plus a denormalized email breaks that by design.
    match repo::pseudonymize_mask_actions(&mut conn, cfg.inspector_audit_pii_days).await {
        Ok(n) if n > 0 => info!(rows = n, "pseudonymized old mask audit rows"),
        Ok(_) => {}
        Err(e) => warn!(error = %e, "pseudonymizing mask audit rows failed"),
    }

    crate::release(conn).await;
    // Always `false`: the reaper must sleep its full interval, never spin.
    Ok(false)
}
