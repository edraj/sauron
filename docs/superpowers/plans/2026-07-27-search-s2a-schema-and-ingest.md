# Search S2a — schema & ingest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land migrations 24 and 25, persist the two ingest fields the pipeline currently throws away (`mechanism.handled`, `sdk`), and add the indexes the query planner will need — with **no user-visible change**.

**Architecture:** Two migrations against a partitioned schema, matching `schema.rs`/`models.rs` edits, and three small pipeline edits. Nothing in this slice reads the new column or the new indexes; S2b's planner does. That is deliberate: this is the only part of S2 with a `down.sql` and the only part touching the ingest hot path, so it ships and is verified alone.

**Tech Stack:** Postgres 16 (partitioned parents), diesel 2.3 + diesel-async, `diesel_migrations`.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-27-pro-search-and-saved-views-design.md`. Read §7 (migrations) and §13 (risks) before starting.
- **Every migration runs inside a transaction** (`MigrationHarness::run_pending_migrations`), and `error_events` / `analytics_events` are **partitioned parents**. `CREATE INDEX CONCURRENTLY` is therefore impossible. Every index goes on the **parent only** — children inherit with Postgres-generated names, so never name a child index.
- `crates/sauron-db/src/schema.rs` is **hand-maintained** despite its `@generated` header. `Queryable` binds **positionally**, so a new column must be appended in the same physical position `ALTER TABLE … ADD COLUMN` puts it: last.
- House migration style, per `0021`/`0022`/`0023`: `--` line comments only, no numeric prefix in the header, the header is a prose *rationale* (why, not what), `down.sql` reverses statement order and uses `IF EXISTS`.
- Hard gates from `backend/`: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- libduckdb must be on the library path for a workspace build. It is nested by version and target triple:
  ```bash
  export DUCKDB_LIB_DIR=$(ls -d "$(git rev-parse --show-toplevel)"/.cache/duckdb/*/*/ | head -1)
  export LD_LIBRARY_PATH=$DUCKDB_LIB_DIR:$LD_LIBRARY_PATH
  ```
- Never `cargo test --all-features` — it rebuilds DuckDB from source.
- **No `CREATE EXTENSION`.** There are zero in the entire repo and the RPM/bare-metal path does not guarantee superuser at migrate time. Everything here is core Postgres.
- **Never create a git branch. Never commit.** Leave changes staged; the user commits.

---

## File Structure

| File | Responsibility |
|---|---|
| `backend/migrations/2026-07-27-000024_event_handled_sdk/{up,down}.sql` | Add `error_events.handled` |
| `backend/migrations/2026-07-27-000025_search_indexes/{up,down}.sql` | Curated btrees, JSONB GINs, keyset indexes, duplicate cleanup |
| `backend/crates/sauron-db/src/schema.rs` | Append `handled` to the `error_events` block |
| `backend/crates/sauron-db/src/models.rs` | Append `handled` to `ErrorEvent` and `NewErrorEvent` |
| `backend/crates/sauron-core/src/envelope.rs` | Add `sdk` to `IngestJob` |
| `backend/bins/sauron-ingest/src/main.rs` | Populate `IngestJob.sdk` from the envelope header |
| `backend/crates/sauron-pipeline/src/process.rs` | Write `handled` and `sdk` onto `NewErrorEvent` |

---

### Task 1: Migration 24 — the `handled` column

**Files:**
- Create: `backend/migrations/2026-07-27-000024_event_handled_sdk/up.sql`
- Create: `backend/migrations/2026-07-27-000024_event_handled_sdk/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs` (the `error_events` block; `extra -> Jsonb,` is currently its last entry)
- Modify: `backend/crates/sauron-db/src/models.rs` (`ErrorEvent` and `NewErrorEvent`)

**Interfaces:**
- Consumes: nothing.
- Produces: `error_events.handled` as `Nullable<Bool>` in `schema.rs`; `pub handled: Option<bool>` as the **last** field of both `ErrorEvent` and `NewErrorEvent`.

The migration is named `event_handled_sdk` for traceability with the spec, but it carries **no DDL for `sdk`** — that column already exists (`init/up.sql:106`, re-declared on the parent by `0011/up.sql:28`) and has simply never been written. Populating it is Task 3.

- [ ] **Step 1: Write `up.sql`**

```sql
-- `handled` records whether the SDK saw the error caught or uncaught — the single
-- most-used filter in any crash reporter ("did this take the app down?"). Every
-- SDK has always sent it as `mechanism.handled` and the pipeline has always
-- dropped it on the floor, so `is:unhandled` was inexpressible.
--
-- Deliberately NULLable, with NO DEFAULT and no backfill. Rows written before
-- this migration genuinely do not know, and `handled = false` AND `handled = true`
-- must BOTH exclude them: folding unknown into either bucket would report every
-- pre-upgrade error as handled, which is the exact opposite of what an on-call
-- engineer filtering for crashes needs. `has:handled` selects the known rows.
-- SQL three-valued logic gives this for free; the only requirement is that
-- nothing ever writes a fallback value.
--
-- ADD COLUMN on the partitioned parent propagates to every existing partition.
ALTER TABLE error_events ADD COLUMN handled BOOLEAN;
```

- [ ] **Step 2: Write `down.sql`**

```sql
ALTER TABLE error_events DROP COLUMN IF EXISTS handled;
```

- [ ] **Step 3: Update `schema.rs`**

In the `error_events!` block, append **after** the current final entry `extra -> Jsonb,`:

```rust
        handled -> Nullable<Bool>,
```

It must be last. `ALTER TABLE … ADD COLUMN` appends physically and `Queryable` binds positionally, so any other position silently binds the wrong fields to the wrong columns.

- [ ] **Step 4: Update `models.rs`**

Append as the final field of **both** structs — `ErrorEvent` and `NewErrorEvent`:

```rust
    pub handled: Option<bool>,
```

- [ ] **Step 5: Verify the migration applies and reverses**

The crate build itself catches a malformed migration directory (a missing `up.sql`/`down.sql` fails the build):

```bash
cd backend && cargo build -p sauron-db
```

Then, against a running Postgres:

```bash
cd backend && diesel migration run && diesel migration revert && diesel migration run
```

Expected: all three succeed. Confirm the column exists and is nullable with no default:

```bash
psql "$DATABASE_URL" -c "\d error_events" | grep handled
```
Expected: `handled | boolean | | |` — nullable, no default.

- [ ] **Step 6: Verify gates**

```bash
cd backend && cargo fmt --all -- --check && cargo clippy -p sauron-db --all-targets -- -D warnings && cargo test -p sauron-db
```

Expected: clean. Leave staged; do not commit.

---

### Task 2: Migration 25 — indexes

**Files:**
- Create: `backend/migrations/2026-07-27-000025_search_indexes/up.sql`
- Create: `backend/migrations/2026-07-27-000025_search_indexes/down.sql`

**Interfaces:**
- Consumes: Task 1's migration must be numbered before this one.
- Produces: no Rust-visible change. Indexes only.

Three corrections to earlier drafts of the spec, all verified by `EXPLAIN` against the live database — do not revert to the older shapes:

1. `(app_id, environment_id, last_seen DESC)` named columns from **two different tables**. `error_events` has no `last_seen`; `issues` has no `environment_id`. The intended index is on `error_events (app_id, environment_id, occurred_at DESC)`.
2. A `first_seen` btree **already exists** — `issues_app_first_seen_idx (app_id, first_seen DESC)` from `0020/up.sql:43-44`. Creating another would repeat the duplication this migration exists to fix.
3. The two duplicate `issues` indexes are **not** redundant prefixes of `issues_list_idx (app_id, status, last_seen DESC)`. `last_seen` sits behind an equality on `status`, and Postgres 16 has no index skip scan, so the default issues list (no status predicate) falls back to a `Sort`. They must be **replaced** by a wider index, not simply dropped.

- [ ] **Step 1: Write `up.sql`**

```sql
-- Indexes for the query planner landing in the next slice. Three groups:
-- the curated dimensions that become filterable, the JSONB roots that become
-- reachable by arbitrary path, and the keyset-cursor support that makes deep
-- paging stable.
--
-- Every CREATE INDEX here builds SYNCHRONOUSLY across all live child partitions
-- inside this migration's transaction, holding locks on the parent and each
-- child. error_events is the hottest-write table in the schema. This needs a
-- maintenance window.
--
-- CONCURRENTLY is not an option: migrations run in a transaction and these are
-- partitioned parents. Indexes are declared on the parent only; children inherit
-- them under Postgres-generated names, so a later DROP on the parent cascades.

-- 1. Keyset support. `issues_app_last_seen_id_idx` is the index the two indexes
--    dropped below are genuine prefixes of, and it also serves the cursor's
--    ROW(last_seen, id) < ROW(?, ?) comparison as an Index Cond. Dropping the
--    duplicates WITHOUT this replacement regresses the default issues list to a
--    Sort, because issues_list_idx buries last_seen behind an equality on status.
CREATE INDEX issues_app_last_seen_id_idx ON issues (app_id, last_seen DESC, id DESC);
DROP INDEX IF EXISTS issues_app_last_seen_idx;   -- duplicate added by 0020
DROP INDEX IF EXISTS issues_last_seen_idx;       -- the original, renamed by 0002

--    Same trick for the occurrences list: the old index is a strict prefix of
--    the new one, so it is genuinely redundant once this exists.
CREATE INDEX error_events_issue_time_id_idx ON error_events (issue_id, occurred_at DESC, id DESC);
DROP INDEX IF EXISTS error_events_issue_idx;

-- 2. Curated dimensions. Three-column, time-trailing, mirroring the shape 0020
--    established: tenant key, then the filtered dimension, then the sort column.
CREATE INDEX error_events_app_env_time_idx     ON error_events     (app_id, environment_id, occurred_at DESC);
CREATE INDEX error_events_app_level_time_idx   ON error_events     (app_id, level,          occurred_at DESC);
CREATE INDEX error_events_app_release_time_idx ON error_events     (app_id, release,        occurred_at DESC);
CREATE INDEX analytics_events_app_env_time_idx ON analytics_events (app_id, environment_id, occurred_at DESC);
CREATE INDEX analytics_events_app_rel_time_idx ON analytics_events (app_id, release,        occurred_at DESC);

-- 3. JSONB roots. `jsonb_ops`, NOT the `jsonb_path_ops` used for `tags` in 0018:
--    path_ops is smaller but answers containment only, and key existence
--    (`has:extra.cartValue` -> `col ? 'cartValue'`) is precisely what it cannot
--    do. Measured on a seeded database: `col ? $1` is an Index Cond at cost 8.6
--    under jsonb_ops, versus a Seq Scan without it.
--
--    `tags` keeps its jsonb_path_ops GIN from 0018 — not touched here.
--    `event_user` and `sdk` are deliberately skipped: both are small fixed-shape
--    objects the catalog already classes Bounded, so a GIN would cost writes to
--    serve a predicate the planner never index-seeks.
CREATE INDEX error_events_context_gin      ON error_events     USING gin (context    jsonb_ops);
CREATE INDEX error_events_contexts_gin     ON error_events     USING gin (contexts   jsonb_ops);
CREATE INDEX error_events_extra_gin        ON error_events     USING gin (extra      jsonb_ops);
CREATE INDEX analytics_events_contexts_gin ON analytics_events USING gin (contexts   jsonb_ops);
CREATE INDEX analytics_events_extra_gin    ON analytics_events USING gin (extra      jsonb_ops);
CREATE INDEX analytics_events_props_gin    ON analytics_events USING gin (properties jsonb_ops);
```

- [ ] **Step 2: Write `down.sql`**

Reverse order. The three dropped indexes are recreated with their original definitions so a revert restores the previous plan shapes exactly.

```sql
DROP INDEX IF EXISTS analytics_events_props_gin;
DROP INDEX IF EXISTS analytics_events_extra_gin;
DROP INDEX IF EXISTS analytics_events_contexts_gin;
DROP INDEX IF EXISTS error_events_extra_gin;
DROP INDEX IF EXISTS error_events_contexts_gin;
DROP INDEX IF EXISTS error_events_context_gin;

DROP INDEX IF EXISTS analytics_events_app_rel_time_idx;
DROP INDEX IF EXISTS analytics_events_app_env_time_idx;
DROP INDEX IF EXISTS error_events_app_release_time_idx;
DROP INDEX IF EXISTS error_events_app_level_time_idx;
DROP INDEX IF EXISTS error_events_app_env_time_idx;

-- Recreate what the up migration replaced, so a revert restores the previous
-- plan shapes rather than leaving the table with neither index.
CREATE INDEX error_events_issue_idx ON error_events (issue_id, occurred_at DESC);
DROP INDEX IF EXISTS error_events_issue_time_id_idx;

CREATE INDEX issues_last_seen_idx ON issues (app_id, last_seen DESC);
CREATE INDEX issues_app_last_seen_idx ON issues (app_id, last_seen DESC);
DROP INDEX IF EXISTS issues_app_last_seen_id_idx;
```

- [ ] **Step 3: Apply and reverse**

```bash
cd backend && diesel migration run && diesel migration revert && diesel migration run
```

Expected: all three succeed with no error.

- [ ] **Step 4: Verify each index is actually chosen**

The point of the migration is plan shapes, not the existence of index objects. Check three representative queries. Substitute a real `app_id` from your database.

```bash
psql "$DATABASE_URL" -c "EXPLAIN SELECT * FROM issues WHERE app_id = '<APP_ID>' ORDER BY last_seen DESC, id DESC LIMIT 50;"
```
Expected: `Index Scan using issues_app_last_seen_id_idx`. **Not** a `Sort`.

```bash
psql "$DATABASE_URL" -c "EXPLAIN SELECT * FROM issues WHERE app_id = '<APP_ID>' AND (last_seen, id) < (now(), '00000000-0000-0000-0000-000000000000') ORDER BY last_seen DESC, id DESC LIMIT 50;"
```
Expected: `Index Scan using issues_app_last_seen_id_idx` with the `ROW(...) < ROW(...)` appearing as an **Index Cond**, not a Filter.

```bash
psql "$DATABASE_URL" -c "SET enable_seqscan=off; EXPLAIN SELECT * FROM error_events WHERE extra ? 'cartValue' LIMIT 10;"
```
Expected: a `Bitmap Index Scan on error_events_extra_gin` (or a per-partition equivalent). If it is a Seq Scan, the GIN opclass is wrong — check you wrote `jsonb_ops`, not `jsonb_path_ops`.

- [ ] **Step 5: Record the write-amplification measurement**

The GINs are the one part of this migration with an ongoing cost. Measure it rather than assuming:

```bash
psql "$DATABASE_URL" -c "SELECT relname, pg_size_pretty(pg_relation_size(indexrelid)) AS idx_size, idx_scan FROM pg_stat_user_indexes WHERE indexrelname LIKE '%_gin' ORDER BY pg_relation_size(indexrelid) DESC;"
```

Record the output in the task report. If any single GIN exceeds the size of its parent table's data, say so explicitly — that is the signal to drop that root from the migration and let the planner class it `Scan` instead.

- [ ] **Step 6: Verify gates**

```bash
cd backend && cargo build -p sauron-db && cargo test -p sauron-db
```

Expected: clean. Leave staged; do not commit.

---

### Task 3: Persist `handled` and `sdk` at ingest

**Files:**
- Modify: `backend/crates/sauron-core/src/envelope.rs` (the `IngestJob` struct)
- Modify: `backend/bins/sauron-ingest/src/main.rs` (where `IngestJob` is constructed)
- Modify: `backend/crates/sauron-pipeline/src/process.rs` (the `NewErrorEvent` literal in `process_error`)

**Interfaces:**
- Consumes: Task 1's `NewErrorEvent.handled` field.
- Produces: `IngestJob.sdk: Option<SdkInfo>`; `NewErrorEvent.handled` and `.sdk` populated from real data.

`mechanism.handled` is already parsed as `Option<bool>` with `#[serde(default)]` inside an `Option<Mechanism>` on the exception item — no wire or SDK change is needed for it. `sdk` needs plumbing because `IngestJob` does not currently carry it, even though it already reaches into `envelope.header` for `environment` and `release`.

- [ ] **Step 1: Add `sdk` to `IngestJob`**

In `backend/crates/sauron-core/src/envelope.rs`, add to the `IngestJob` struct:

```rust
    /// Envelope-scoped SDK identity. `#[serde(default)]` is load-bearing: the
    /// queue is a Redis stream, so during a rolling upgrade jobs serialized by
    /// the previous ingest binary are still in flight and must keep
    /// deserializing against the new struct.
    #[serde(default)]
    pub sdk: Option<SdkInfo>,
```

- [ ] **Step 2: Populate it at the ingest edge**

In `backend/bins/sauron-ingest/src/main.rs`, in the `IngestJob { … }` literal, add:

```rust
        sdk: Some(envelope.header.sdk.clone()),
```

`SdkInfo` derives `Clone`, and `for item in envelope.items` only partially moves the envelope, so `envelope.header` is still accessible inside the loop.

- [ ] **Step 3: Write both fields onto the row**

In `backend/crates/sauron-pipeline/src/process.rs`, in the `NewErrorEvent { … }` literal inside `process_error`, replace the hardcoded `sdk: None,` and append `handled`:

```rust
        sdk: job.sdk.as_ref().and_then(|s| serde_json::to_value(s).ok()),
        handled: exc.and_then(|x| x.mechanism.as_ref()).and_then(|m| m.handled),
```

`exc` is already bound earlier in the function and is still live at the construction site.

**Never write `.unwrap_or(true)` or `.unwrap_or(false)` here.** This single expression is where the design's NULL-means-unknown decision lives; the migration's missing DEFAULT is the other half. A fallback value would make every pre-upgrade error report as handled — the exact failure the design set out to avoid.

Keep `sdk` an **object** (`{"name":…,"version":…}`), not a flattened string: the catalog declares `sdk` as a JSON root, so `sdk.name:sauron.javascript` lowers to a containment match against that shape.

- [ ] **Step 4: Write a test for the NULL semantics**

The pure mapping deserves a test even though the surrounding function needs a database. Add to the existing `#[cfg(test)]` module in `process.rs`, extracting the expression into a small pure helper if that is what it takes to make it testable:

```rust
    #[test]
    fn handled_is_none_when_the_sdk_did_not_say() {
        // Three ways to arrive at unknown: no exception, an exception with no
        // mechanism, and a mechanism that omitted the flag. None may become
        // `Some(true)` — that would classify a real crash as handled.
        assert_eq!(handled_of(None), None);
    }

    #[test]
    fn handled_round_trips_both_known_values() {
        assert_eq!(handled_of(Some(true)), Some(true));
        assert_eq!(handled_of(Some(false)), Some(false));
    }
```

Shape the helper to match how you extracted it; the requirement is that all three unknown paths are asserted to stay `None`.

- [ ] **Step 5: Build and test the workspace**

```bash
cd backend \
  && export DUCKDB_LIB_DIR=$(ls -d ../.cache/duckdb/*/*/ | head -1) \
  && export LD_LIBRARY_PATH=$DUCKDB_LIB_DIR:$LD_LIBRARY_PATH \
  && cargo fmt --all -- --check \
  && cargo clippy --workspace --all-targets -- -D warnings \
  && cargo test --workspace
```

Expected: clean, all tests pass.

- [ ] **Step 6: Verify end to end against the running stack**

This is the step that proves the slice. Rebuild the ingest and worker images, send a real event with an unhandled exception through an SDK or `curl`, and confirm the row:

```bash
psql "$DATABASE_URL" -c "SELECT id, handled, sdk FROM error_events ORDER BY occurred_at DESC LIMIT 5;"
```

Expected: the newest row has `handled` set to `t` or `f` (not NULL) and `sdk` populated as a JSON object with `name` and `version`. Older rows keep `handled = NULL` — confirm that too, because it is the design's central claim.

Record the actual output in the task report.

---

## Definition of done for S2a

- Both migrations apply, revert, and re-apply cleanly.
- `EXPLAIN` confirms `issues_app_last_seen_id_idx` serves both the plain list and the keyset comparison as an Index Cond, and that the default issues list is **not** a `Sort`.
- A freshly ingested event has non-NULL `handled` and a populated `sdk` object; pre-existing rows still have `handled = NULL`.
- The GIN size measurement is recorded.
- `cargo test --workspace` green; fmt and clippy clean.
- **No user-visible change.** No endpoint, response shape, or UI behaviour differs.
- Nothing committed.

## Deployment note

RPM upgrades never re-run `sauron-migrate`, so this slice's two migrations must be applied by hand after upgrading, before the new binaries serve traffic. Both index-building statements lock the hottest-write table in the schema across every partition — schedule a maintenance window.
