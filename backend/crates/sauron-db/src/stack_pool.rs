//! Content-addressed stacktrace pooling ("Tier 1") — migration 0068.
//!
//! A repeated exception's stacktrace is byte-identical across every occurrence
//! of an issue (it is what the fingerprint groups on), yet each occurrence
//! stores its own copy — measured at ~25% of the row on realistic payloads,
//! with 5 distinct traces across 199,990 rows on a duplicate-heavy run. This
//! module moves the trace into `error_stack_blobs` once per DISTINCT value and
//! leaves a 32-byte content address on the row.
//!
//! # The one row per occurrence contract is untouched
//!
//! Pooling changes physical layout only: every occurrence still has its own
//! `error_events` row, so `COUNT(*)`, alert thresholds and keyset pagination
//! are byte-for-byte unaffected. That property is what separates this from
//! row-collapse, which was rejected outright (the product decision is that
//! every occurrence's breadcrumbs/context/timestamp are kept).
//!
//! # Write gating and read unconditionality
//!
//! Writers pool only when `INGEST_STACK_POOLING` is truthy — default OFF, so a
//! deployment that never sets it is byte-identical to pre-0068 behaviour.
//! Readers ([`hydrate`]) run UNCONDITIONALLY: rows written while the flag was
//! on must read correctly after it is turned off again, and pre-0068 rows
//! (NULL `stacktrace_sha256`) pass through untouched.
//!
//! # Why there is no refcount
//!
//! `symbol_blobs`' hand-maintained refcount drifted (31% of blob bytes
//! unreachable) because `ON DELETE CASCADE` bypasses application decrements —
//! repaired by migration 0067's recompute trigger. This table skips the
//! counter entirely: liveness is derived (`EXISTS` via the partial index
//! `error_events_stack_sha_idx`), the GC sweep deletes only what nothing
//! references, and the FK from `error_events.stacktrace_sha256` makes a
//! premature free a loud constraint error instead of silent corruption.
//!
//! # What must change together with this module
//!
//! Every consumer that reads `stacktrace` raw sees [`PLACEHOLDER`] (`[]`) for
//! pooled rows. The integrated call sites are: repo read paths (hydrated),
//! the `stack:` query lowerer (ORs a pool subquery), the PII retro-mask
//! (de-pools its scope first — masking a shared blob in place would rewrite
//! every row sharing it, across tenants), and the Parquet cold export (joins
//! the pool so cold files carry full traces and no hash column). A NEW reader
//! of the raw column must either hydrate or join.

use std::sync::OnceLock;

use diesel::prelude::*;
use diesel::sql_types::BigInt;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use sha2::{Digest, Sha256};

use crate::models::NewErrorEvent;
use crate::schema::error_stack_blobs;

/// What a pooled row's inline `stacktrace` column holds. An empty array — not
/// NULL — so the column keeps its NOT NULL shape and every legacy reader that
/// forgot to hydrate degrades to "no frames" rather than a crash or a decode
/// error. Rows whose REAL trace is empty are never pooled (nothing to save),
/// so `sha256 IS NOT NULL` alone decides whether hydration applies.
pub fn placeholder() -> serde_json::Value {
    serde_json::Value::Array(vec![])
}

/// Whether writers pool. Read once: the flag is deployment configuration, not
/// something to toggle mid-process, and a per-batch env read would put a
/// syscall on the hottest write path in the system.
pub fn pooling_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("INGEST_STACK_POOLING")
            .map(|v| matches!(v.trim(), "1" | "true" | "on" | "yes"))
            .unwrap_or(false)
    })
}

#[derive(Debug, Insertable)]
#[diesel(table_name = error_stack_blobs)]
pub struct NewErrorStackBlob {
    pub sha256: Vec<u8>,
    pub content: serde_json::Value,
}

/// Move each row's stacktrace into a blob, leaving the placeholder + address.
///
/// Returns the DISTINCT blobs this batch introduces, deduplicated by hash —
/// the common case is one blob for thousands of rows (that is the whole
/// point), and handing duplicates to a multi-row INSERT would bloat the wire
/// for nothing. `ON CONFLICT DO NOTHING` in [`insert_blobs`] carries no
/// "cannot affect row a second time" hazard (that error is specific to
/// `DO UPDATE`), so the dedup here is size hygiene, not correctness.
///
/// The hash is over the serialized JSON exactly as this process would have
/// written it inline. Two logically-equal traces serialized differently would
/// simply make two blobs — wasteful, never wrong.
pub fn intern(rows: &mut [NewErrorEvent]) -> Vec<NewErrorStackBlob> {
    let mut blobs: Vec<NewErrorStackBlob> = Vec::new();
    for row in rows.iter_mut() {
        // Empty or non-array traces stay inline: pooling `[]` swaps a 4-byte
        // datum for a 32-byte reference, strictly negative.
        let has_frames = row
            .stacktrace
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if !has_frames {
            continue;
        }
        let bytes = match serde_json::to_vec(&row.stacktrace) {
            Ok(b) => b,
            // Unserializable JSON cannot happen for a Value, but the fallback
            // is the safe direction: leave it inline.
            Err(_) => continue,
        };
        let hash = Sha256::digest(&bytes).to_vec();
        let content = std::mem::replace(&mut row.stacktrace, placeholder());
        row.stacktrace_sha256 = Some(hash.clone());
        if !blobs.iter().any(|b| b.sha256 == hash) {
            blobs.push(NewErrorStackBlob {
                sha256: hash,
                content,
            });
        }
    }
    blobs
}

/// Insert the batch's blobs. MUST run in the same transaction as (and before)
/// the event rows: the FK on `stacktrace_sha256` rejects an event whose blob
/// is not yet visible, and same-transaction insertion is also what makes the
/// GC sweep race-free — a blob a live transaction just wrote is either
/// invisible to the sweep's snapshot or already referenced by commit time.
pub async fn insert_blobs(
    conn: &mut AsyncPgConnection,
    blobs: &[NewErrorStackBlob],
) -> QueryResult<usize> {
    if blobs.is_empty() {
        return Ok(0);
    }
    diesel::insert_into(error_stack_blobs::table)
        .values(blobs)
        .on_conflict(error_stack_blobs::sha256)
        .do_nothing()
        .execute(conn)
        .await
}

/// Swap real traces back into pooled rows, in place.
///
/// One query for the page's DISTINCT hashes — a page of duplicates hits a
/// handful of blobs, which is the measured shape (5 blobs per 200k rows) —
/// then a map lookup per row. Rows with NULL sha256 pass through untouched,
/// which is every pre-0068 row and every row written with pooling off.
///
/// A referenced blob that fails to load is a broken invariant (the FK makes it
/// unreachable short of manual catalog surgery), but the degradation is the
/// placeholder — "no frames" — never an error that takes the page down.
pub async fn hydrate(
    conn: &mut AsyncPgConnection,
    events: &mut [crate::models::ErrorEvent],
) -> QueryResult<()> {
    let mut want: Vec<Vec<u8>> = events
        .iter()
        .filter_map(|e| e.stacktrace_sha256.clone())
        .collect();
    want.sort();
    want.dedup();
    if want.is_empty() {
        return Ok(());
    }
    let found: Vec<(Vec<u8>, serde_json::Value)> = error_stack_blobs::table
        .filter(error_stack_blobs::sha256.eq_any(&want))
        .select((error_stack_blobs::sha256, error_stack_blobs::content))
        .load(conn)
        .await?;
    let by_hash: std::collections::HashMap<Vec<u8>, serde_json::Value> =
        found.into_iter().collect();
    for e in events.iter_mut() {
        if let Some(h) = &e.stacktrace_sha256 {
            if let Some(content) = by_hash.get(h) {
                e.stacktrace = content.clone();
            }
        }
    }
    Ok(())
}

/// De-pool every row in a scope back to inline storage.
///
/// The PII retro-mask rewrites `error_events.stacktrace` IN PLACE by column
/// name. Run against a pooled row that UPDATE would hit the placeholder (its
/// `#> path` selector is NULL there, so the row is silently SKIPPED and the
/// real trace survives unmasked in the pool) — and pointing the mask at the
/// shared blob instead would rewrite the trace for every row sharing it,
/// across apps and tenants. De-pooling first is the resolution: the scope's
/// rows get their own inline copies (losing the storage saving for exactly
/// those rows, which diverge anyway once masked), the shared blob is never
/// touched, and the existing mask SQL then works unchanged. Runs regardless
/// of the write flag — rows pooled last month must mask correctly today.
pub async fn depool_scope(
    conn: &mut AsyncPgConnection,
    app_id: uuid::Uuid,
    lo: chrono::DateTime<chrono::Utc>,
    hi: chrono::DateTime<chrono::Utc>,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE error_events e \
         SET stacktrace = b.content, stacktrace_sha256 = NULL \
         FROM error_stack_blobs b \
         WHERE e.app_id = $1 AND e.occurred_at >= $2 AND e.occurred_at < $3 \
           AND e.stacktrace_sha256 = b.sha256",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .bind::<diesel::sql_types::Timestamptz, _>(lo)
    .bind::<diesel::sql_types::Timestamptz, _>(hi)
    .execute(conn)
    .await
}

/// Grace age below which an unreferenced blob is not swept. Blobs are written
/// in the same transaction as their referencing rows, so a young unreferenced
/// blob "should" be impossible — the grace exists so that even a path that
/// violates that expectation leaks for a day instead of racing the sweep.
pub const STACK_BLOB_SWEEP_GRACE_HOURS: i64 = 24;

/// Delete blobs no partition references. Runs in `sauron-tier`, because
/// partition DROP is the event that orphans blobs — the rows referencing a
/// trace age out wholesale when their partition is dropped, and nothing
/// decrements anything (there is nothing to decrement; see the module doc).
/// The `NOT EXISTS` probe is served by the partial index on every partition,
/// and the FK downgrades any bug here to a constraint error.
pub async fn sweep_orphan_stack_blobs(
    conn: &mut AsyncPgConnection,
    grace_hours: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM error_stack_blobs b \
         WHERE b.created_at < now() - make_interval(hours => $1::int) \
           AND NOT EXISTS (SELECT 1 FROM error_events e \
                            WHERE e.stacktrace_sha256 = b.sha256)",
    )
    .bind::<BigInt, _>(grace_hours)
    .execute(conn)
    .await
}
