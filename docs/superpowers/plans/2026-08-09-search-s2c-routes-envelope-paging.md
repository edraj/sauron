# Search S2c — routes, envelope, keyset paging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the already-built query language on the wire — `query=`, `sort=` and `cursor=` on the three searched list endpoints, behind a response envelope with stable keyset paging, and document the grammar so a developer can use it without reading the source.

**Architecture:** S1 (`sauron-query`) and S2b (`sauron-db::query_plan`) are built and green; nothing in them changes. This slice is the seam between them and axum: parse `query=` (or bridge `filter=`/`q=` through `from_legacy`) into a `ResolvedNode`, run the async `prepare` pass, `lower` it to a boxed diesel fragment, add a keyset predicate, and serialise the rows into an envelope instead of a bare array. The dashboard's three list clients move to the envelope and gain a shared cursor pager.

**Tech Stack:** Rust / axum 0.7 / diesel-async / Postgres 16; Svelte 5 (runes only) / TypeScript / vitest.

## Global Constraints

- **Never create a branch, never commit.** Changes are staged; the user commits. (Spec §14, and the repo-wide rule.) Every task ends at "verify", not at "commit".
- Endpoints accepting repeated params **must** use `axum_extra::extract::Query` — plain `axum::Query` silently drops repeats. Today only `routes/issues.rs` and `routes/analytics.rs` import it.
- Authorization stays two-layered and both layers are mandatory: `authorize_app`-family call in the handler after the single pool checkout, **and** the tenant key in every query's WHERE clause including nested subqueries.
- Svelte 5 runes only. House UI components only — a raw `<button>` renders as a browser-default grey box because the global reset only sets font and cursor.
- New env vars go in `crates/sauron-core/src/config.rs`, the README table, **and** `.env.example`. (This slice adds none.)
- `backend/diesel.toml` must keep **no** `[print_schema] file =` key. After any task touching `sauron-db`, verify `schema.rs` gained **no partition-child `table!` blocks** — names matching `_\d{4}_\d{2}_\d{2}$` or `_default$`. Do **not** assert a fixed table count: the count grows legitimately with every migration (it is 45 as of migration 46; the "27" in older notes was true only during S1). The corruption signature is partition children and a redeclared `error_events (id, occurred_at)` primary key, not a number.
- Migrations run **synchronously** across live child partitions inside one transaction. `CONCURRENTLY` is not available. Say so in the migration's own comment.

## Corrections to the spec, found while planning

These supersede the spec where they conflict. Fix the spec in Task 9.

1. **Migration numbering is stale.** Spec §7 says S3 is migration 26 and S5 is 27. The tree is already at `2026-08-09-000046_channel_config_enc`. This slice's migration is **`2026-08-09-000047_analytics_keyset_index`**.
2. **`analytics_events` has no keyset tiebreaker.** Migration 25 added `issues_app_last_seen_id_idx (app_id, last_seen DESC, id DESC)` and `error_events_issue_time_id_idx (issue_id, occurred_at DESC, id DESC)`, but the closest analytics index is `analytics_project_idx (app_id, occurred_at DESC)` — no `id`. Without the tiebreaker, deep paging on Events is exactly the duplicate-rows bug this slice exists to fix. Task 1 adds it.
3. **CI does have Postgres and Redis** (`.github/workflows/ci.yml:32-51`, `TEST_DATABASE_URL`/`TEST_REDIS_URL`). The "pure code only" constraint applied to S1's crate design, not to this slice. Route behaviour is tested through the real router with the `tests/http_*.rs` harness.
4. **`sort=` is restricted to keyset-backed orderings.** A sort with no supporting `(…, id)` index cannot page stably, and silently returning duplicate rows is the defect being fixed. Unsupported sorts return 400 with the allowed list. More orderings arrive with their indexes in a later slice.

---

## File Structure

**Backend**
- `backend/migrations/2026-08-09-000047_analytics_keyset_index/{up,down}.sql` — the missing tiebreaker index.
- `backend/crates/sauron-db/src/query_plan/cursor.rs` (new) — opaque `(timestamp, uuid)` cursor codec. Pure; no diesel, no I/O.
- `backend/bins/sauron-api/src/routes/search.rs` (new) — the shared seam: query-source resolution, sort whitelist, envelope struct, count policy. One module so the three handlers cannot drift.
- `backend/bins/sauron-api/src/routes/issues.rs` — `list` moves to the envelope.
- `backend/bins/sauron-api/src/routes/analytics.rs` — `events_list` moves to the envelope. There is no `events.rs`; `main.rs:562-565` routes `/v1/apps/{app_id}/events/list` to `routes::analytics::events_list`.
- `backend/bins/sauron-api/tests/http_search.rs` (new) — equivalence and paging tests.

**Frontend**
- `dashboard/src/lib/api/search.ts` (new) — the `SearchEnvelope<T>` type and a `withEnvelope` helper.
- `dashboard/src/lib/api/{issues,events}.ts` — return envelopes.
- `dashboard/src/lib/components/CursorPagination.svelte` (new).
- `dashboard/src/lib/models/cursor-page.ts` (+ `.test.ts`) — the pure page-state reducer, so paging logic is unit-testable without a DOM.

**Docs**
- `wiki/Search.md` — rewritten. Line 12 currently promises *"no query language, no operators"*, which this slice makes an active lie.

---

### Task 1: Keyset tiebreaker index for analytics_events

**Files:**
- Create: `backend/migrations/2026-08-09-000047_analytics_keyset_index/up.sql`
- Create: `backend/migrations/2026-08-09-000047_analytics_keyset_index/down.sql`

**Interfaces:**
- Consumes: nothing.
- Produces: index `analytics_events_app_time_id_idx`, relied on by Task 6's keyset predicate.

- [ ] **Step 1: Write `up.sql`**

```sql
-- The keyset tiebreaker analytics_events never got.
--
-- Migration 25 gave issues (app_id, last_seen DESC, id DESC) and error_events
-- (issue_id, occurred_at DESC, id DESC). The closest analytics index is
-- analytics_project_idx (app_id, occurred_at DESC) — no id column. A keyset
-- cursor ordered by (occurred_at DESC, id DESC) can still seek with it, but
-- rows sharing an occurred_at have no index-level order, so a page boundary
-- landing inside such a group repeats or skips rows. That is the exact defect
-- this slice exists to remove, so the index has to exist before the cursor does.
--
-- Builds SYNCHRONOUSLY across every live child partition inside this
-- transaction, holding locks on the parent and each child. analytics_events is
-- a hot-write table: this needs a maintenance window. CONCURRENTLY is not an
-- option — migrations run in a transaction and this is a partitioned parent.
CREATE INDEX analytics_events_app_time_id_idx
    ON analytics_events (app_id, occurred_at DESC, id DESC);
```

- [ ] **Step 2: Write `down.sql`**

```sql
DROP INDEX IF EXISTS analytics_events_app_time_id_idx;
```

- [ ] **Step 3: Run the migration against a scratch database**

```bash
cd backend && DATABASE_URL="$TEST_DATABASE_URL" cargo run --bin sauron-migrate
```

Expected: exits 0, no error mentioning `analytics_events_app_time_id_idx`.

- [ ] **Step 4: Confirm the planner will use it**

`gen_random_uuid()` is **volatile**, so Postgres cannot use it as an index probe key and will pick a seq scan no matter how good the index is. Use a literal or a bound parameter:

```bash
psql "$TEST_DATABASE_URL" -c "PREPARE p(uuid) AS SELECT id, occurred_at FROM analytics_events WHERE app_id = \$1 ORDER BY occurred_at DESC, id DESC LIMIT 50; EXPLAIN EXECUTE p('00000000-0000-0000-0000-000000000000');"
```

Expected: an `Index Scan` or `Index Only Scan` whose name ends `_app_id_occurred_at_id_idx`, and **no** `Sort` node. The name will be the CHILD partition's inherited index (e.g. `analytics_events_default_app_id_occurred_at_id_idx`), not the parent's declared `analytics_events_app_time_id_idx` — that is normal for a partitioned table and is not a failure.

An empty table still yields a seq scan regardless, so seed a few thousand rows first. If seeding proves impractical (foreign keys to apps/projects), assert instead that the index exists on the parent and record that you did so rather than reporting a plan you did not observe:

```bash
psql "$TEST_DATABASE_URL" -c "SELECT indexname FROM pg_indexes WHERE tablename = 'analytics_events';"
```

- [ ] **Step 5: Verify schema.rs did not drift**

```bash
cd backend && git diff --stat HEAD -- crates/sauron-db/src/schema.rs diesel.toml
grep -oP 'diesel::table! \{\s*\K\w+' crates/sauron-db/src/schema.rs | grep -E '_[0-9]{4}_[0-9]{2}_[0-9]{2}$|_default$'
```

Expected: the first command prints nothing (this task must not touch either file); the second prints nothing (no partition children). If the second prints names like `analytics_events_default`, `diesel.toml` regrew its `[print_schema] file =` key — revert `schema.rs` from git and remove the key.

---

### Task 2: Cursor codec

**Files:**
- Create: `backend/crates/sauron-db/src/query_plan/cursor.rs`
- Modify: `backend/crates/sauron-db/src/query_plan/mod.rs` (add `pub mod cursor;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct Cursor { pub ts: DateTime<Utc>, pub id: Uuid }`
  - `pub fn encode(c: &Cursor) -> String`
  - `pub fn decode(s: &str) -> Result<Cursor, CursorError>`
  - `pub enum CursorError { Malformed, BadTimestamp, BadUuid }` (implements `std::fmt::Display`)

- [ ] **Step 1: Write the failing tests**

Create `backend/crates/sauron-db/src/query_plan/cursor.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample() -> Cursor {
        Cursor {
            ts: Utc.with_ymd_and_hms(2026, 8, 9, 12, 30, 45).unwrap(),
            id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
        }
    }

    #[test]
    fn round_trips() {
        let c = sample();
        assert_eq!(decode(&encode(&c)).unwrap(), c);
    }

    #[test]
    fn preserves_sub_second_precision() {
        // Page boundaries land between events milliseconds apart; truncating to
        // whole seconds would re-emit every row inside the truncated second.
        let c = Cursor {
            ts: Utc.timestamp_micros(1_786_000_000_123_456).unwrap(),
            id: sample().id,
        };
        assert_eq!(decode(&encode(&c)).unwrap().ts, c.ts);
    }

    #[test]
    fn is_url_safe() {
        // It travels in a query string; + and / would need escaping and the
        // padding = is a routine source of double-encoding bugs.
        let s = encode(&sample());
        assert!(!s.contains('+') && !s.contains('/') && !s.contains('='), "got {s}");
    }

    #[test]
    fn rejects_garbage_rather_than_panicking() {
        for bad in ["", "!!!!", "Zm9v", "e30", "########"] {
            assert!(decode(bad).is_err(), "{bad} should not decode");
        }
    }

    #[test]
    fn rejects_a_truncated_cursor() {
        let s = encode(&sample());
        assert!(decode(&s[..s.len() - 3]).is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test -p sauron-db cursor`
Expected: FAIL — `cannot find type Cursor in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to the same file, above the test module:

```rust
//! Opaque cursor for keyset pagination over a `(timestamp, uuid)` tuple.
//!
//! Opaque, not secret. It encodes only values the caller just received in the
//! response body, so there is nothing to hide and nothing to sign — it is
//! base64url purely so clients treat it as a token to echo back rather than a
//! structure to build themselves. Every list this slice touches orders by a
//! timestamp with `id` as the tiebreaker, so one shape serves all three.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub ts: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorError {
    Malformed,
    BadTimestamp,
    BadUuid,
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CursorError::Malformed => "cursor is not a valid pagination token",
            CursorError::BadTimestamp => "cursor timestamp is invalid",
            CursorError::BadUuid => "cursor id is invalid",
        })
    }
}

/// `<rfc3339-micros>|<uuid>`, base64url without padding.
///
/// RFC 3339 rather than an epoch integer so a cursor stays legible in a log
/// line while debugging, and micros rather than seconds because consecutive
/// events routinely share a second.
pub fn encode(c: &Cursor) -> String {
    let raw = format!(
        "{}|{}",
        c.ts.format("%Y-%m-%dT%H:%M:%S%.6fZ"),
        c.id
    );
    URL_SAFE_NO_PAD.encode(raw)
}

pub fn decode(s: &str) -> Result<Cursor, CursorError> {
    let bytes = URL_SAFE_NO_PAD.decode(s).map_err(|_| CursorError::Malformed)?;
    let text = String::from_utf8(bytes).map_err(|_| CursorError::Malformed)?;
    let (ts_s, id_s) = text.split_once('|').ok_or(CursorError::Malformed)?;
    let ts = DateTime::parse_from_rfc3339(ts_s)
        .map_err(|_| CursorError::BadTimestamp)?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id_s).map_err(|_| CursorError::BadUuid)?;
    Ok(Cursor { ts, id })
}
```

- [ ] **Step 4: Register the module**

In `backend/crates/sauron-db/src/query_plan/mod.rs`, beside the existing `pub mod events;` line, add:

```rust
pub mod cursor;
```

- [ ] **Step 5: Confirm `base64` is already a dependency**

```bash
cd backend && grep -n '^base64' crates/sauron-db/Cargo.toml || grep -rn 'base64' crates/sauron-db/Cargo.toml
```

If absent, add `base64 = { workspace = true }` and confirm the workspace root already pins it (`grep -n '^base64' Cargo.toml`). Do not introduce a second version.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd backend && cargo test -p sauron-db cursor`
Expected: PASS, 5 tests.

- [ ] **Step 7: Lint**

Run: `cd backend && cargo fmt --check && cargo clippy -p sauron-db --all-targets -- -D warnings`
Expected: clean.

---

### Task 3: The shared search seam — query source, sort whitelist, envelope

**Files:**
- Create: `backend/bins/sauron-api/src/routes/search.rs`
- Modify: `backend/bins/sauron-api/src/routes/mod.rs` (add `pub mod search;`)

**Interfaces:**
- Consumes: `sauron_query::{parse, resolve, from_legacy, ResolvedNode, QueryError}`, `sauron_db::query_plan::cursor`.
- Produces:
  - `pub struct SearchEnvelope<T> { data: Vec<T>, total: i64, total_is_capped: bool, next_cursor: Option<String>, clamped: Option<ClampInfo> }`
  - `pub struct ClampInfo { field: String, to: String, reason: String }`
  - `pub fn resolve_query(query: Option<&str>, filter: &[String], q: Option<&str>, resource: Resource) -> Result<ResolvedNode, ApiError>`
  - `pub fn parse_sort(raw: Option<&str>, allowed: &[&str], default: &str) -> Result<(String, bool), ApiError>` — returns `(column, descending)`
  - `pub const COUNT_CAP: i64 = 10_000;`

- [ ] **Step 1: Write the failing tests**

Create `backend/bins/sauron-api/src/routes/search.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_defaults_when_absent() {
        let (col, desc) = parse_sort(None, &["last_seen", "first_seen"], "last_seen").unwrap();
        assert_eq!((col.as_str(), desc), ("last_seen", true));
    }

    #[test]
    fn sort_accepts_a_leading_minus_for_ascending() {
        // `-` reads as "reverse the default", and the default everywhere here is
        // newest-first, so `-last_seen` is oldest-first.
        let (col, desc) = parse_sort(Some("-last_seen"), &["last_seen"], "last_seen").unwrap();
        assert_eq!((col.as_str(), desc), ("last_seen", false));
    }

    #[test]
    fn sort_rejects_a_column_with_no_keyset_index() {
        // Not cosmetic: an unindexed ordering cannot page stably, and silently
        // returning duplicate rows is the bug this slice removes.
        let err = parse_sort(Some("times_seen"), &["last_seen"], "last_seen").unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("times_seen"), "error should name the bad field: {msg}");
        assert!(msg.contains("last_seen"), "error should list what is allowed: {msg}");
    }

    #[test]
    fn envelope_serialises_the_documented_shape() {
        let env = SearchEnvelope {
            data: vec![1_i32, 2],
            total: 1204,
            total_is_capped: false,
            next_cursor: None,
            clamped: None,
        };
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["total"], 1204);
        // A number, never a display string like "1204+": every client would
        // otherwise have to parse a number back out of it.
        assert!(v["total"].is_number());
        assert_eq!(v["total_is_capped"], false);
        assert!(v["next_cursor"].is_null());
        assert!(v["clamped"].is_null());
        assert_eq!(v["data"], serde_json::json!([1, 2]));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test -p sauron-api search::tests`
Expected: FAIL — `cannot find function parse_sort`.

- [ ] **Step 3: Write the implementation**

Prepend above the test module:

```rust
//! The seam between the query crate, the planner, and axum.
//!
//! One module rather than three copies inside the handlers: the envelope shape,
//! the legacy bridge and the count policy are the parts most likely to drift
//! apart, and drift here is a client-visible inconsistency between two lists
//! that look identical.

use serde::Serialize;

use sauron_query::{from_legacy, parse, resolve, ResolvedNode};

use crate::error::ApiError;

/// Counting stops here when the plan degrades to a scan.
///
/// `total` stays a number and `total_is_capped` carries the nuance, so counting
/// never becomes the expensive part of the request.
pub const COUNT_CAP: i64 = 10_000;

#[derive(Debug, Serialize)]
pub struct ClampInfo {
    pub field: String,
    pub to: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct SearchEnvelope<T> {
    pub data: Vec<T>,
    pub total: i64,
    pub total_is_capped: bool,
    pub next_cursor: Option<String>,
    pub clamped: Option<ClampInfo>,
}

/// Which of the three input shapes the caller used.
///
/// `query=` wins outright when present. `filter=`/`q=` keep working and are
/// bridged into the same AST, so an existing bookmark returns the same rows —
/// that equivalence is what Task 4's test asserts.
pub fn resolve_query(
    query: Option<&str>,
    filter: &[String],
    q: Option<&str>,
    resource: sauron_query::Resource,
) -> Result<ResolvedNode, ApiError> {
    let ast = match query {
        Some(text) if !text.trim().is_empty() => {
            parse(text).map_err(|e| ApiError::BadRequest(e.to_string()))?
        }
        // `from_legacy` takes NO resource — it is a purely syntactic bridge and
        // produces the same untyped `Node` that `parse` does. Field validity is
        // decided one line down, by `resolve`, for both paths alike.
        _ => from_legacy(filter, q).map_err(|e| ApiError::BadRequest(e.to_string()))?,
    };
    resolve(&ast, resource).map_err(|e| ApiError::BadRequest(e.to_string()))
}

/// Returns `(column, descending)`.
///
/// `allowed` is the set of orderings with a supporting `(…, id)` index. Anything
/// else is refused rather than served unstably.
pub fn parse_sort(
    raw: Option<&str>,
    allowed: &[&str],
    default: &str,
) -> Result<(String, bool), ApiError> {
    let spec = raw.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(default);
    let (col, descending) = match spec.strip_prefix('-') {
        Some(rest) => (rest, false),
        None => (spec, true),
    };
    if !allowed.contains(&col) {
        return Err(ApiError::BadRequest(format!(
            "cannot sort by `{col}`; this list supports {} (prefix with `-` to reverse). \
             Other columns need a matching index before they can be paged stably.",
            allowed.join(", ")
        )));
    }
    Ok((col.to_string(), descending))
}
```

- [ ] **Step 4: Signatures these call sites depend on (already verified — do not re-derive)**

```rust
// crates/sauron-query/src/lib.rs re-exports all of these.
pub enum Resource { Issues, Occurrences, Events, Sessions, Devices, Persons, Transactions }
pub fn parse(input: &str) -> Result<Node, QueryError>;
pub fn from_legacy(filters: &[String], q: Option<&str>) -> Result<Node, QueryError>;   // no Resource
pub fn resolve(node: &Node, r: Resource) -> Result<ResolvedNode, QueryError>;
```

One nuance carried from S1's ledger: `QueryError`'s `at` is a **byte offset** when it came from `parse`/`resolve` but a **filter-array index** when it came from `from_legacy`. Do not render it as a caret position into the `query=` string on the legacy path.

- [ ] **Step 5: Register the module**

In `backend/bins/sauron-api/src/routes/mod.rs`, beside the sibling `pub mod` lines:

```rust
pub mod search;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd backend && cargo test -p sauron-api search::tests`
Expected: PASS, 4 tests.

- [ ] **Step 7: Lint**

Run: `cd backend && cargo fmt --check && cargo clippy -p sauron-api --all-targets -- -D warnings`
Expected: clean.

---

### Task 4: Issues list — query, sort, cursor, envelope

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/issues.rs` (`ListQuery` at :20-38, `list` at :109)
- Create: `backend/bins/sauron-api/tests/http_search.rs`

**Interfaces:**
- Consumes: everything Task 2 and Task 3 produce.
- Produces: `GET /v1/apps/{app_id}/issues` answering `SearchEnvelope<Issue>`. Sort whitelist `["last_seen", "first_seen"]`, default `last_seen`. Cursor tuple `(last_seen, id)`, backed by `issues_app_last_seen_id_idx`.

- [ ] **Step 1: Write the failing test**

Create `backend/bins/sauron-api/tests/http_search.rs`. Copy the `TestServer`, `swap_database`, `free_port` and JWT helpers verbatim from `tests/http_workflows.rs:15-120` — they are duplicated per test binary on purpose (see that file's doc comment). Then:

```rust
/// The equivalence that makes the legacy bridge safe to ship: an old bookmark
/// and its `query=` spelling must select the same rows, in the same order.
#[tokio::test]
async fn query_and_filter_return_identical_rows() {
    let Some(srv) = TestServer::start().await else { return }; // skips without TEST_DATABASE_URL
    let (app_id, token) = srv.seed_app_with_issues(&[
        ("unresolved", "error", 5),
        ("resolved", "error", 50),
        ("unresolved", "warning", 500),
    ]).await;

    let legacy = srv
        .get_json(&format!("/v1/apps/{app_id}/issues?filter=status:eq:unresolved&filter=level:eq:error"), &token)
        .await;
    let modern = srv
        .get_json(&format!("/v1/apps/{app_id}/issues?query=status:unresolved%20level:error"), &token)
        .await;

    let ids = |v: &serde_json::Value| -> Vec<String> {
        v["data"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(ids(&legacy), ids(&modern), "legacy and query= disagree");
    assert_eq!(legacy["total"], modern["total"]);
}

/// The defect this slice exists to remove.
#[tokio::test]
async fn deep_paging_never_repeats_a_row() {
    let Some(srv) = TestServer::start().await else { return };
    // All sharing one last_seen, so every page boundary lands inside a tie
    // group — the case a (last_seen)-only index cannot order.
    let (app_id, token) = srv.seed_issues_sharing_a_timestamp(120).await;

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let url = match &cursor {
            Some(c) => format!("/v1/apps/{app_id}/issues?limit=25&cursor={c}"),
            None => format!("/v1/apps/{app_id}/issues?limit=25"),
        };
        let page = srv.get_json(&url, &token).await;
        seen.extend(page["data"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()));
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    let mut deduped = seen.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(seen.len(), deduped.len(), "a row was returned on two pages");
    assert_eq!(deduped.len(), 120, "paging did not reach every row");
}

#[tokio::test]
async fn an_unsupported_sort_is_refused_not_served_unstably() {
    let Some(srv) = TestServer::start().await else { return };
    let (app_id, token) = srv.seed_app_with_issues(&[("unresolved", "error", 1)]).await;
    let (status, body) = srv
        .get_status_and_body(&format!("/v1/apps/{app_id}/issues?sort=times_seen"), &token)
        .await;
    assert_eq!(status, 400);
    assert!(body.contains("times_seen"), "error should name the field: {body}");
}
```

Write the three `srv.seed_*` helpers alongside, inserting rows with `sauron_db::models::NewIssue` exactly as `http_workflows.rs` does.

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test -p sauron-api --test http_search`
Expected: FAIL — the response is a bare array, so `v["data"]` is null and `ids()` panics on `as_array`.

- [ ] **Step 3: Widen `ListQuery`**

In `routes/issues.rs`, add to the struct at :20 (keep the existing `environment_id` comment intact — it explains why that field is deliberately absent):

```rust
    /// The query language. Wins over `filter`/`q` when non-empty.
    pub query: Option<String>,
    /// `column` or `-column`. Restricted to keyset-backed orderings.
    pub sort: Option<String>,
    /// Opaque token from the previous page's `next_cursor`.
    pub cursor: Option<String>,
```

- [ ] **Step 4: Rewrite the handler body**

Replace the `filter`/`search` block in `list` with the seam. Keep the existing `authorized_read_scope_with_perms` call and the `symbolicate::text_search_reach` narrowing exactly as they are — permissions are unchanged by this slice.

```rust
    let node = super::search::resolve_query(
        q.query.as_deref(),
        &q.filter,
        q.q.as_deref().filter(|s| !s.is_empty()),
        sauron_query::Resource::Issues,
    )?;
    let prepared = sauron_db::query_plan::prepare::prepare(&node, app_id, Utc::now(), &mut conn)
        .await
        .map_err(map_plan_error)?;
    let (sort_col, descending) =
        super::search::parse_sort(q.sort.as_deref(), &["last_seen", "first_seen"], "last_seen")?;
    let after = match q.cursor.as_deref() {
        Some(c) => Some(
            sauron_db::query_plan::cursor::decode(c)
                .map_err(|e| ApiError::BadRequest(e.to_string()))?,
        ),
        None => None,
    };
```

Then add this to `backend/crates/sauron-db/src/repo.rs` and call it. Fetch `limit + 1` rows: the extra row is how you learn whether a next page exists without a second query.

```rust
/// `limit + 1` rows, newest first, optionally starting after a keyset cursor.
///
/// The caller truncates back to `limit`; the surplus row is the has-more probe.
pub async fn search_issues(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    node: &ResolvedNode,
    ctx: &PrepCtx,
    descending: bool,
    after: Option<Cursor>,
    limit: i64,
) -> Result<Vec<Issue>, PlanError> {
    use crate::schema::issues::dsl as i;

    let predicate = crate::query_plan::lower(node, &IssuesLower, ctx)?;
    // The tenant key in the WHERE clause is the mandatory second layer; the
    // handler's authorize_app call is the first. Neither substitutes for the other.
    let mut q = i::issues.filter(i::app_id.eq(app_id)).filter(predicate).into_boxed();

    // Keyset, not OFFSET: (last_seen, id) is a total order thanks to
    // issues_app_last_seen_id_idx, so a row inserted mid-walk cannot shift a
    // later page onto rows an earlier page already returned.
    if let Some(c) = after {
        q = if descending {
            q.filter(i::last_seen.lt(c.ts).or(i::last_seen.eq(c.ts).and(i::id.lt(c.id))))
        } else {
            q.filter(i::last_seen.gt(c.ts).or(i::last_seen.eq(c.ts).and(i::id.gt(c.id))))
        };
    }
    let q = if descending {
        q.order((i::last_seen.desc(), i::id.desc()))
    } else {
        q.order((i::last_seen.asc(), i::id.asc()))
    };
    Ok(q.limit(limit + 1).load::<Issue>(conn).await?)
}
```

`sort_col` selects between this and a `first_seen` twin; keep the tie-breaker column (`id`) identical in both so every ordering stays total.

Build the envelope:

```rust
    let has_more = rows.len() as i64 > q.limit;
    rows.truncate(q.limit as usize);
    let next_cursor = has_more.then(|| {
        rows.last().map(|r| sauron_db::query_plan::cursor::encode(
            &sauron_db::query_plan::cursor::Cursor { ts: r.last_seen, id: r.id },
        ))
    }).flatten();
    Ok(Json(super::search::SearchEnvelope {
        data: rows,
        total,
        total_is_capped,
        next_cursor,
        clamped: prepared.clamp.map(|c| super::search::ClampInfo {
            field: c.field.to_string(),
            to: format!("{}d", c.to_days),
            reason: c.reason.to_string(),
        }),
    }))
```

The `clamped` mapping is where `Clamp`'s generic `field: "since"` becomes this resource's real column — `prepare` deliberately does not know which resource it ran for, so naming it is the caller's job.

- [ ] **Step 5: Add the count**

Add alongside `search_issues` in `repo.rs`:

```rust
/// `(total, capped)`.
///
/// Counting is exact while the plan is index-backed and stops at the cap once it
/// degrades to a scan, so counting never becomes the expensive part of the
/// request. `lower` is called a second time rather than the fragment being
/// cloned — `Frag` is a boxed trait object and is consumed by the first query.
pub async fn count_issues(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    node: &ResolvedNode,
    ctx: &PrepCtx,
    cap: i64,
) -> Result<(i64, bool), PlanError> {
    use crate::schema::issues::dsl as i;

    let predicate = crate::query_plan::lower(node, &IssuesLower, ctx)?;
    // Count the ids of at most cap+1 matching rows: cap+1 is the sentinel that
    // distinguishes "exactly cap" from "more than cap" without counting them all.
    let ids: Vec<Uuid> = i::issues
        .filter(i::app_id.eq(app_id))
        .filter(predicate)
        .select(i::id)
        .limit(cap + 1)
        .load(conn)
        .await?;
    let n = ids.len() as i64;
    Ok(if n > cap { (cap, true) } else { (n, false) })
}
```

Call it as:

```rust
    let (total, total_is_capped) = sauron_db::repo::count_issues(
        &mut conn, app_id, &node, &prepared.ctx, super::search::COUNT_CAP,
    ).await.map_err(map_plan_error)?;
```

- [ ] **Step 5a: Write `map_plan_error`**

`PlanError` must not leak as a 500 — most of its variants are the caller's fault. Add to `routes/search.rs`:

```rust
/// A rejected plan is a bad request, not a server fault. The two exceptions are
/// genuine internal failures and must stay 500 so they page someone.
pub fn map_plan_error(e: sauron_db::query_plan::PlanError) -> ApiError {
    use sauron_db::query_plan::PlanError as P;
    match e {
        P::Database(inner) => ApiError::Internal(inner.to_string()),
        other => ApiError::BadRequest(other.to_string()),
    }
}
```

Check `PlanError`'s real variants first (`grep -n "pub enum PlanError" -A20 backend/crates/sauron-db/src/query_plan/mod.rs`) and match every one explicitly rather than with a catch-all, so a variant added later forces a decision here instead of silently becoming a 400.

- [ ] **Step 6: Confirm the extractor**

```bash
cd backend && grep -n "use axum_extra::extract::Query" bins/sauron-api/src/routes/issues.rs
```

Expected: present. `ListQuery.filter` is a `Vec<String>`; plain `axum::Query` would silently drop the repeats and the equivalence test would fail for a reason that looks like a planner bug.

- [ ] **Step 7: Run the tests**

Run: `cd backend && cargo test -p sauron-api --test http_search`
Expected: PASS, 3 tests. If `TEST_DATABASE_URL` is unset they skip — export it and re-run; a skipped test proves nothing.

- [ ] **Step 8: Lint**

Run: `cd backend && cargo fmt --check && cargo clippy -p sauron-api --all-targets -- -D warnings`

---

### Task 5: Occurrences list — same treatment

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/issues.rs` — the handler behind `/v1/apps/{app_id}/issues/{issue_id}/events` (`main.rs:546`). Note the path segment is `events`, not `occurrences`; `OccurrencesLower` currently has **no call site anywhere** — this task is its first consumer.
- Modify: `backend/bins/sauron-api/tests/http_search.rs`

**Interfaces:**
- Consumes: Task 3's seam (`resolve_query`, `parse_sort`, `SearchEnvelope`, `COUNT_CAP`), Task 2's cursor, and `search::map_plan_error` added in Task 4 Step 5a.
- Produces: `SearchEnvelope<ErrorEvent>`. Sort whitelist `["occurred_at"]`, default `occurred_at`. Cursor tuple `(occurred_at, id)`, backed by `error_events_issue_time_id_idx`.

- [ ] **Step 1: Write the failing test**

Add to `http_search.rs`:

```rust
#[tokio::test]
async fn occurrences_page_stably_within_one_issue() {
    let Some(srv) = TestServer::start().await else { return };
    let (app_id, issue_id, token) = srv.seed_issue_with_occurrences(90).await;
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            Some(c) => format!("/v1/apps/{app_id}/issues/{issue_id}/events?limit=20&cursor={c}"),
            None => format!("/v1/apps/{app_id}/issues/{issue_id}/events?limit=20"),
        };
        let page = srv.get_json(&url, &token).await;
        seen.extend(page["data"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()));
        match page["next_cursor"].as_str() { Some(c) => cursor = Some(c.into()), None => break }
    }
    let mut d = seen.clone(); d.sort(); d.dedup();
    assert_eq!(seen.len(), d.len());
    assert_eq!(d.len(), 90);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test -p sauron-api --test http_search occurrences`
Expected: FAIL on `v["data"]` being null.

- [ ] **Step 3: Widen the query struct**

```rust
    pub query: Option<String>,
    pub sort: Option<String>,
    pub cursor: Option<String>,
```

- [ ] **Step 4: Add the repo function**

In `backend/crates/sauron-db/src/repo.rs`:

```rust
/// `limit + 1` occurrences of one issue, newest first.
///
/// Scoped by issue id AND app id. The issue id is the narrower predicate, but
/// the app id is the tenant key and the WHERE-clause layer is mandatory
/// regardless of whether another column happens to imply it.
pub async fn search_occurrences(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    issue_id: Uuid,
    node: &ResolvedNode,
    ctx: &PrepCtx,
    descending: bool,
    after: Option<Cursor>,
    limit: i64,
) -> Result<Vec<ErrorEvent>, PlanError> {
    use crate::schema::error_events::dsl as e;

    let predicate = crate::query_plan::lower(node, &OccurrencesLower, ctx)?;
    let mut q = e::error_events
        .filter(e::app_id.eq(app_id))
        .filter(e::issue_id.eq(issue_id))
        .filter(predicate)
        .into_boxed();

    // Backed by error_events_issue_time_id_idx (issue_id, occurred_at DESC, id DESC).
    if let Some(c) = after {
        q = if descending {
            q.filter(e::occurred_at.lt(c.ts).or(e::occurred_at.eq(c.ts).and(e::id.lt(c.id))))
        } else {
            q.filter(e::occurred_at.gt(c.ts).or(e::occurred_at.eq(c.ts).and(e::id.gt(c.id))))
        };
    }
    let q = if descending {
        q.order((e::occurred_at.desc(), e::id.desc()))
    } else {
        q.order((e::occurred_at.asc(), e::id.asc()))
    };
    Ok(q.limit(limit + 1).load::<ErrorEvent>(conn).await?)
}
```

- [ ] **Step 5: Wire the handler**

```rust
    let node = super::search::resolve_query(
        q.query.as_deref(),
        &q.filter,
        q.q.as_deref().filter(|s| !s.is_empty()),
        sauron_query::Resource::Occurrences,
    )?;
    let prepared = sauron_db::query_plan::prepare::prepare(&node, app_id, Utc::now(), &mut conn)
        .await
        .map_err(super::search::map_plan_error)?;
    let (_sort_col, descending) =
        super::search::parse_sort(q.sort.as_deref(), &["occurred_at"], "occurred_at")?;
    let after = match q.cursor.as_deref() {
        Some(c) => Some(
            sauron_db::query_plan::cursor::decode(c)
                .map_err(|e| ApiError::BadRequest(e.to_string()))?,
        ),
        None => None,
    };
    let mut rows = sauron_db::repo::search_occurrences(
        &mut conn, app_id, issue_id, &node, &prepared.ctx, descending, after, q.limit,
    ).await.map_err(super::search::map_plan_error)?;

    let has_more = rows.len() as i64 > q.limit;
    rows.truncate(q.limit as usize);
    let next_cursor = has_more
        .then(|| rows.last().map(|r| sauron_db::query_plan::cursor::encode(
            &sauron_db::query_plan::cursor::Cursor { ts: r.occurred_at, id: r.id },
        )))
        .flatten();
    Ok(Json(super::search::SearchEnvelope {
        data: rows,
        total,
        total_is_capped,
        next_cursor,
        clamped: prepared.clamp.map(|c| super::search::ClampInfo {
            field: "occurred_at".to_string(),
            to: format!("{}d", c.to_days),
            reason: c.reason.to_string(),
        }),
    }))
```

Note `field: "occurred_at"`, not `c.field` — `Clamp.field` is the generic `"since"` because `prepare` does not know which resource it ran for. Naming the real column is the caller's job, and it differs from Task 4's `last_seen`.

Add a `count_occurrences` mirroring Task 4's `count_issues`, with the same `app_id` + `issue_id` predicate pair.

- [ ] **Step 4: Run the test**

Run: `cd backend && cargo test -p sauron-api --test http_search occurrences`
Expected: PASS.

- [ ] **Step 5: Lint**

Run: `cd backend && cargo fmt --check && cargo clippy -p sauron-api --all-targets -- -D warnings`

---

### Task 6: Events list — same treatment

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/analytics.rs`, fn `events_list` (routed at `main.rs:562-565`)
- Modify: `backend/bins/sauron-api/tests/http_search.rs`

**Interfaces:**
- Consumes: Task 3's seam, Task 2's cursor, Task 1's index, and `search::map_plan_error` from Task 4 Step 5a.
- Produces: `SearchEnvelope<AnalyticsEvent>`. Sort whitelist `["occurred_at"]`, default `occurred_at`. Cursor tuple `(occurred_at, id)`, backed by `analytics_events_app_time_id_idx` from Task 1.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn events_page_stably_across_a_shared_timestamp() {
    let Some(srv) = TestServer::start().await else { return };
    // Every row shares one occurred_at: without Task 1's id tiebreaker this is
    // precisely where pages overlap.
    let (app_id, token) = srv.seed_events_sharing_a_timestamp(75).await;
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            Some(c) => format!("/v1/apps/{app_id}/events/list?limit=20&cursor={c}"),
            None => format!("/v1/apps/{app_id}/events/list?limit=20"),
        };
        let page = srv.get_json(&url, &token).await;
        seen.extend(page["data"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()));
        match page["next_cursor"].as_str() { Some(c) => cursor = Some(c.into()), None => break }
    }
    let mut d = seen.clone(); d.sort(); d.dedup();
    assert_eq!(seen.len(), d.len(), "Task 1's index is missing or unused");
    assert_eq!(d.len(), 75);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test -p sauron-api --test http_search events`
Expected: FAIL.

- [ ] **Step 3: Widen the query struct**

```rust
    pub query: Option<String>,
    pub sort: Option<String>,
    pub cursor: Option<String>,
```

- [ ] **Step 4: Add the repo function**

```rust
/// `limit + 1` analytics events for one app, newest first.
///
/// The keyset tuple is (occurred_at, id), backed by the index Task 1 adds. This
/// function is the reason that index exists: without the `id` column the tuple
/// is not a total order and pages overlap wherever rows share a timestamp.
pub async fn search_events(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    node: &ResolvedNode,
    ctx: &PrepCtx,
    descending: bool,
    after: Option<Cursor>,
    limit: i64,
) -> Result<Vec<AnalyticsEvent>, PlanError> {
    use crate::schema::analytics_events::dsl as a;

    let predicate = crate::query_plan::lower(node, &EventsLower, ctx)?;
    let mut q = a::analytics_events
        .filter(a::app_id.eq(app_id))
        .filter(predicate)
        .into_boxed();

    if let Some(c) = after {
        q = if descending {
            q.filter(a::occurred_at.lt(c.ts).or(a::occurred_at.eq(c.ts).and(a::id.lt(c.id))))
        } else {
            q.filter(a::occurred_at.gt(c.ts).or(a::occurred_at.eq(c.ts).and(a::id.gt(c.id))))
        };
    }
    let q = if descending {
        q.order((a::occurred_at.desc(), a::id.desc()))
    } else {
        q.order((a::occurred_at.asc(), a::id.asc()))
    };
    Ok(q.limit(limit + 1).load::<AnalyticsEvent>(conn).await?)
}
```

- [ ] **Step 5: Wire the handler**

```rust
    let node = super::search::resolve_query(
        q.query.as_deref(),
        &q.filter,
        q.q.as_deref().filter(|s| !s.is_empty()),
        sauron_query::Resource::Events,
    )?;
    let prepared = sauron_db::query_plan::prepare::prepare(&node, app_id, Utc::now(), &mut conn)
        .await
        .map_err(super::search::map_plan_error)?;
    let (_sort_col, descending) =
        super::search::parse_sort(q.sort.as_deref(), &["occurred_at"], "occurred_at")?;
    let after = match q.cursor.as_deref() {
        Some(c) => Some(
            sauron_db::query_plan::cursor::decode(c)
                .map_err(|e| ApiError::BadRequest(e.to_string()))?,
        ),
        None => None,
    };
    let mut rows = sauron_db::repo::search_events(
        &mut conn, app_id, &node, &prepared.ctx, descending, after, q.limit,
    ).await.map_err(super::search::map_plan_error)?;

    let has_more = rows.len() as i64 > q.limit;
    rows.truncate(q.limit as usize);
    let next_cursor = has_more
        .then(|| rows.last().map(|r| sauron_db::query_plan::cursor::encode(
            &sauron_db::query_plan::cursor::Cursor { ts: r.occurred_at, id: r.id },
        )))
        .flatten();
    Ok(Json(super::search::SearchEnvelope {
        data: rows,
        total,
        total_is_capped,
        next_cursor,
        clamped: prepared.clamp.map(|c| super::search::ClampInfo {
            field: "occurred_at".to_string(),
            to: format!("{}d", c.to_days),
            reason: c.reason.to_string(),
        }),
    }))
```

Add a `count_events` mirroring Task 4's `count_issues`, scoped by `app_id`.

- [ ] **Step 6: Confirm the extractor on this file too**

```bash
cd backend && grep -n "axum_extra::extract::Query" bins/sauron-api/src/routes/analytics.rs
```

Expected: present (the spec notes this file already imports it).

- [ ] **Step 7: Run the whole suite**

Run: `cd backend && cargo test -p sauron-api --test http_search && cargo test --workspace`
Expected: PASS. Workspace was 538 green before this slice; it should be 538 + the new tests.

- [ ] **Step 8: Lint**

Run: `cd backend && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`

---

### Task 7: Dashboard API clients move to the envelope

**Files:**
- Create: `dashboard/src/lib/api/search.ts`
- Modify: `dashboard/src/lib/api/issues.ts:26-38`, `dashboard/src/lib/api/events.ts:12-24`
- Modify: every call site the compiler flags

**Interfaces:**
- Consumes: the envelope from Tasks 4-6.
- Produces:
  - `export interface SearchEnvelope<T> { data: T[]; total: number; total_is_capped: boolean; next_cursor: string | null; clamped: { field: string; to: string; reason: string } | null }`
  - `listIssues(appId, opts): Promise<SearchEnvelope<Issue>>`
  - `listEvents(appId, opts): Promise<SearchEnvelope<AnalyticsEvent>>`
  - Both `opts` gain `query?: string`, `sort?: string`, `cursor?: string`.

- [ ] **Step 1: Write `search.ts`**

```ts
/**
 * The envelope the searched list endpoints answer.
 *
 * `total` is always a number; `total_is_capped` carries the nuance. The server
 * deliberately does not return a display string like "1204+" — that would make
 * every caller parse a number back out of it.
 */
export interface SearchEnvelope<T> {
  data: T[];
  total: number;
  total_is_capped: boolean;
  /** null on the last page. */
  next_cursor: string | null;
  /** Set when the planner narrowed the window to keep the query affordable. */
  clamped: { field: string; to: string; reason: string } | null;
}
```

- [ ] **Step 2: Widen the two clients**

In `issues.ts`, add to `ListIssuesParams` and the param builder:

```ts
  if (opts.query) p.set('query', opts.query);
  if (opts.sort) p.set('sort', opts.sort);
  if (opts.cursor) p.set('cursor', opts.cursor);
```

and change the return type:

```ts
export async function listIssues(
  appId: string,
  opts: ListIssuesParams = {},
): Promise<SearchEnvelope<Issue>> {
  // …existing param building…
  const { data } = await api.get<SearchEnvelope<Issue>>(`/v1/apps/${appId}/issues?${p.toString()}`);
  return data;
}
```

Repeat verbatim in `events.ts` with `AnalyticsEvent`.

- [ ] **Step 3: Let the compiler find every call site**

Run: `cd dashboard && npm run check`
Expected: errors wherever a caller treats the result as an array — `Issues.svelte`, `Events.svelte`, and any view-cache wiring. Fix each by reading `.data`, and thread `total` into whatever renders a count.

- [ ] **Step 4: Re-run**

Run: `cd dashboard && npm run check`
Expected: 0 errors.

- [ ] **Step 5: Run the tests**

Run: `cd dashboard && npx vitest run`
Expected: PASS (476 before this slice).

---

### Task 8: `CursorPagination.svelte` and the page-state reducer

**Files:**
- Create: `dashboard/src/lib/models/cursor-page.ts`
- Create: `dashboard/src/lib/models/cursor-page.test.ts`
- Create: `dashboard/src/lib/components/CursorPagination.svelte`
- Modify: `dashboard/src/pages/Issues.svelte`, `dashboard/src/pages/Events.svelte`

**Interfaces:**
- Consumes: `SearchEnvelope<T>`.
- Produces:
  - `export interface CursorPage { stack: string[]; current: string | null; next: string | null }`
  - `export function emptyPage(): CursorPage`
  - `export function advance(p: CursorPage, nextCursor: string | null): CursorPage`
  - `export function goBack(p: CursorPage): CursorPage`
  - `export function canGoBack(p: CursorPage): boolean`

- [ ] **Step 1: Write the failing tests**

```ts
import { describe, it, expect } from 'vitest';
import { emptyPage, advance, goBack, canGoBack } from './cursor-page';

describe('cursor paging', () => {
  it('starts with nowhere to go back to', () => {
    expect(canGoBack(emptyPage())).toBe(false);
  });

  // A keyset cursor only moves forward, so "previous" is a stack of the
  // cursors already used, not an arithmetic offset.
  it('walks forward and back over the same cursors', () => {
    let p = emptyPage();
    p = advance(p, 'c1');
    p = advance(p, 'c2');
    expect(p.current).toBe('c2');
    expect(canGoBack(p)).toBe(true);
    p = goBack(p);
    expect(p.current).toBe('c1');
    p = goBack(p);
    expect(p.current).toBeNull();
    expect(canGoBack(p)).toBe(false);
  });

  it('marks the last page when the server sends no next cursor', () => {
    const p = advance(emptyPage(), null);
    expect(p.next).toBeNull();
  });

  it('going back past the start is a no-op rather than an error', () => {
    const p = goBack(goBack(emptyPage()));
    expect(p.current).toBeNull();
    expect(canGoBack(p)).toBe(false);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/models/cursor-page.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Write `cursor-page.ts`**

```ts
/**
 * Page state for a keyset-cursor list.
 *
 * A keyset cursor only goes forward — there is no "cursor for page N-1" to ask
 * the server for — so going back means remembering the cursors already used.
 * `stack` holds them; `current` is the cursor that produced what is on screen
 * (null on the first page); `next` is what the server just handed back.
 */
export interface CursorPage {
  stack: string[];
  current: string | null;
  next: string | null;
}

export function emptyPage(): CursorPage {
  return { stack: [], current: null, next: null };
}

export function advance(p: CursorPage, nextCursor: string | null): CursorPage {
  if (p.next === null && p.current !== null && nextCursor === null) {
    return { ...p, next: null };
  }
  return { stack: p.current === null ? p.stack : [...p.stack, p.current], current: p.next, next: nextCursor };
}

export function canGoBack(p: CursorPage): boolean {
  return p.current !== null;
}

export function goBack(p: CursorPage): CursorPage {
  if (p.current === null) return p;
  const stack = [...p.stack];
  const prev = stack.pop() ?? null;
  return { stack, current: prev, next: null };
}
```

If a test fails, fix the reducer against the test — the tests are the specification of the walk, not the code.

- [ ] **Step 4: Run to verify they pass**

Run: `cd dashboard && npx vitest run src/lib/models/cursor-page.test.ts`
Expected: PASS, 4 tests.

- [ ] **Step 5: Write `CursorPagination.svelte`**

House components only — `Button`, not raw `<button>`.

```svelte
<script lang="ts">
  import Button from './ui/Button.svelte';

  interface Props {
    total: number;
    totalIsCapped: boolean;
    canPrev: boolean;
    canNext: boolean;
    busy?: boolean;
    onprev: () => void;
    onnext: () => void;
  }
  let { total, totalIsCapped, canPrev, canNext, busy = false, onprev, onnext }: Props = $props();
</script>

<div class="pager">
  <span class="count muted">
    {total.toLocaleString()}{totalIsCapped ? '+' : ''}
    {total === 1 && !totalIsCapped ? 'result' : 'results'}
  </span>
  <div class="controls">
    <Button size="sm" variant="secondary" disabled={!canPrev || busy} onclick={onprev}>Previous</Button>
    <Button size="sm" variant="secondary" disabled={!canNext || busy} onclick={onnext}>Next</Button>
  </div>
</div>

<style>
  .pager { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 14px; }
  .count { font-size: 12.5px; }
  .controls { display: flex; gap: 8px; }
</style>
```

The `+` suffix is a **display** concern and lives here, which is exactly why the API returns a number and a boolean rather than the string `"1204+"`.

- [ ] **Step 6: Wire it into Issues and Events**

Hold `CursorPage` in `$state`, pass `cursor: page.current ?? undefined` into the client call, and call `advance` with `envelope.next_cursor` after each load. Reset to `emptyPage()` whenever the query, filters or date range change — a cursor from one result set is meaningless against another.

- [ ] **Step 7: Verify in the browser**

Start the dev server via the `dashboard` launch config, open the Issues page, and page forward past the end and back to the start. Confirm no row appears twice and the count matches. Check `read_console_messages` for errors.

- [ ] **Step 8: Run the gates**

Run: `cd dashboard && npm run check && npx vitest run`
Expected: 0 errors; all tests pass.

---

### Task 9: `wiki/Search.md` and the spec correction

**Files:**
- Modify: `wiki/Search.md`
- Modify: `docs/superpowers/specs/2026-07-27-pro-search-and-saved-views-design.md` (§7 migration numbers, §12 S2c row)

**Interfaces:**
- Consumes: the grammar frozen in S1 — `sauron-query/src/catalog.rs` is the single source of truth for field names.
- Produces: documentation matching shipped behaviour.

- [ ] **Step 1: Rewrite `wiki/Search.md`**

Line 12 currently reads *"no query language, no operators"*. Replace the "Two mechanisms" section with the real grammar. Cover, with a worked example each: bare terms; `field:value`; `!` negation; `AND`/`OR` and that adjacency means AND; parenthesised grouping; `field:[a,b,c]` lists; `>`/`>=`/`<`/`<=` on numbers and durations; `field:~text` for a literal substring (and why it is distinct from `*` wildcards); `has:field`; the `is:` namespace; `tag:<key>=<value>` including the escape hatch for keys with characters outside `[A-Za-z0-9_.-]`.

State the limits plainly: `MAX_DEPTH = 8`, `MAX_TERMS = 64`.

Document the envelope and `cursor=`, and that `filter=`/`q=` still work and parse to the same tree.

- [ ] **Step 2: Verify every documented field actually exists**

```bash
cd backend && grep -o 'name: "[a-z_.]*"' crates/sauron-query/src/catalog.rs | sed 's/name: "//;s/"//' | sort > /tmp/catalog-fields.txt
grep -o '`[a-z_.]*:' ../wiki/Search.md | tr -d '`:' | sort -u > /tmp/doc-fields.txt
comm -13 /tmp/catalog-fields.txt /tmp/doc-fields.txt
```

Expected: empty output. Anything listed is documented but not implemented — remove it or implement it. (The catalog-generated anti-rot test itself lands in S6; this manual check covers the gap until then.)

- [ ] **Step 3: Correct the spec**

In §7, change the S3 migration from 26 to the next free number at that time, and the S5 migration from 27 likewise; add a note that numbering is assigned when the slice starts, not in the spec. In §12, mark the S2c row done and record that `sort=` shipped restricted to keyset-backed columns.

- [ ] **Step 4: Update the programme ledger**

Append to `.superpowers/sdd/progress.md`: what shipped, the four spec corrections above, and the carry-over list for S3.

- [ ] **Step 5: Final gates**

```bash
cd backend && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cd ../dashboard && npm run check && npx vitest run
```

Expected: all clean. **Do not commit** — leave everything staged for the user.

---

## Carry-over, explicitly out of scope

- **Pagination and sorting on the other ~16 `DataTable` pages.** This slice makes the three searched lists pageable. `DataTable` is a presentational shell that takes raw `<tr>` markup from its parent, so sortable headers are a component change belonging with the S4 UI work. Roles, Account, SourceMaps, Devices, Monitors and Storage fetch whole lists and want client-side paging, which needs no server change at all.
- **`SearchBar.svelte`** replacing `FilterBar` — S4, and it depends on the `Popover`/`Combobox` primitives that do not exist yet.
- **Value autocomplete** — needs `issue_dimensions`, which is S3.
- **Saved views** and the `view:write` permission — S5.
- **Sessions, Devices, Users, Transactions** gaining server-side filtering — S6.
- **The `jsonb_ops` GINs** measured and dropped in S2a. Migration 25's `up.sql` holds the reasoning; revisit only with a measured query mix, and never by simply re-adding them.

---

### Task 4b: Restore per-environment issue statistics

**Added mid-execution by human ruling during Task 4, after review found that the planner path had
silently replaced per-environment issue statistics with app-wide ones.** Full task text lives in the
execution workspace at `.superpowers/sdd/2026-08-09-search-s2c-routes-envelope-paging/task-4b-brief.md`
— it is long, and the reasoning about the two rejected alternatives belongs with it.

Summary: two-phase. Phase 1 keyset-pages issue IDs through the planner unchanged; phase 2 re-derives
`times_seen`/`users_seen`/`first_seen`/`last_seen`/`level`/`culprit`/`title` for exactly that page,
from `error_events` under the caller's `EnvFilter`, and overwrites them on the returned rows. Skipped
entirely for `EnvFilter::All`. **Issues-only** — `error_events` and `analytics_events` have a real
`environment_id` column, so Tasks 5 and 6 must not copy it. Ordering remains by stored `last_seen`,
which is inherent to keyset paging and is documented rather than fixed.
