# Workflow Grouping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let apps declare named, explicitly-bounded spans of activity (`startWorkflow` / `endWorkflow` / `cancelWorkflow`) across all 5 SDKs, roll them up server-side, and report outcomes, durations, and contained events/issues in the dashboard.

**Architecture:** No new envelope item type. Workflow lifecycle travels as three reserved analytics events (`$workflow_start` / `$workflow_end` / `$workflow_cancel`); attribution travels as two nullable fields (`workflow_id`, `workflow_name`) stamped on error/event/transaction items, exactly like the existing `screen` field. The pipeline upserts a new non-partitioned `workflows` rollup table via `bump_workflow`, mirroring `bump_session`. Abandonment is derived on read from a staleness threshold — no sweeper job.

**Tech Stack:** Rust (axum, diesel + diesel-async, Postgres), Svelte 5 runes + Vite + svelte-spa-router, TypeScript, Dart/Flutter, Python, C#/.NET.

**Spec:** `docs/superpowers/specs/2026-07-29-workflow-grouping-design.md` — read it before starting. It is the authority on semantics; this plan is the authority on sequencing and file paths.

## Global Constraints

- **Workflows are optional everywhere.** `workflow_id` / `workflow_name` are nullable on every wire item and every DB column. An app that never calls `startWorkflow` must behave byte-identically to today. No query path may change when workflow data is absent.
- **NEVER run `git commit`, `git branch`, or `git checkout -b`.** Leave all work uncommitted in the working tree. This plan deliberately contains no commit steps. Do not add any.
- **Telemetry never throws into host app code.** Every new public SDK method returns a status value and wraps its body in the SDK's existing try/catch + debug-logger idiom.
- **Server SDKs (node, python, csharp) must NOT use a module-level global for workflow state.** Use the existing request-isolated scope primitive (`AsyncLocalStorage` / `ContextVar` / `AsyncLocal`). A module global cross-contaminates concurrent requests.
- **Status enum values, verbatim:** `ok`, `already_active`, `not_active`, `name_mismatch`, `invalid_name`, `disabled`.
- **Reserved event names, verbatim:** `$workflow_start`, `$workflow_end`, `$workflow_cancel`.
- **Cancel reasons:** `superseded` (force-replaced), `user` (explicit cancel, no reason given), or a caller string. Cap at 120 chars.
- **Name validation:** trim; reject empty; cap at 120 chars.
- **Abandonment threshold:** 30 minutes, declared once as a single constant in the repo layer.
- **Wire source of truth:** `backend/crates/sauron-core/src/envelope.rs`. Change it first, then mirror into 4 duplicated SDK type files and 5 golden fixtures.
- **SDK version bumps happen in TWO places each** (manifest + in-code constant) per `sdks/PUBLISHING.md:221-227`. js/node/python/csharp `1.2.0 → 1.3.0`; flutter `1.3.0 → 1.4.0`.
- **Dashboard `npm run check` uses `noUnusedLocals` + `noUnusedParameters`** — an unused import fails the build.
- **Migration numbering is global and monotonic.** `000029_env_scope_grants` already exists uncommitted, so this feature starts at `000030`.

## Task Order

Backend foundation (1–5) → SDKs (6–10) → dashboard (11–14) → docs (15). Tasks 6–10 are
mutually independent and may run in parallel once Task 2 lands. Tasks 11–14 need Task 5.

---
### Task 1: Migration — `workflows` table + stamped columns

**Files:**
- Create: `backend/migrations/2026-07-29-000030_workflows/up.sql`
- Create: `backend/migrations/2026-07-29-000030_workflows/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs` (regenerated, do not hand-edit)
- Modify: `backend/crates/sauron-db/src/models.rs` (add `Workflow` model)

**Interfaces:**
- Consumes: nothing.
- Produces: table `workflows`; nullable columns `workflow_id text` + `workflow_name text` on `analytics_events`, `error_events`, `transactions`; diesel table macro `workflows`; struct `Workflow` (Queryable, Serialize) mirroring the column order in `schema.rs`.

**Read first:** `backend/migrations/2026-07-28-000028_issue_env_covering_index/up.sql` for the migration style, and the `sessions` table definition in `backend/migrations/2026-07-14-000004_sessions_devices/up.sql` — `workflows` deliberately mirrors it (not partitioned, `UNIQUE (app_id, <client id>)`).

- [ ] **Step 1: Write `up.sql`**

```sql
CREATE TABLE workflows (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id UUID NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    workflow_id TEXT NOT NULL,
    name TEXT NOT NULL,
    session_id TEXT,
    distinct_id TEXT,
    device_key TEXT,
    release TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    cancel_reason TEXT,
    started_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ,
    last_event_at TIMESTAMPTZ NOT NULL,
    events_count INTEGER NOT NULL DEFAULT 0,
    errors_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT workflows_app_workflow_key UNIQUE (app_id, workflow_id),
    CONSTRAINT workflows_status_check CHECK (status IN ('active', 'completed', 'cancelled'))
);

CREATE INDEX workflows_app_name_started_idx ON workflows (app_id, name, started_at DESC);
CREATE INDEX workflows_app_status_last_event_idx ON workflows (app_id, status, last_event_at DESC);
CREATE INDEX workflows_app_session_idx ON workflows (app_id, session_id);
CREATE INDEX workflows_app_env_idx ON workflows (app_id, environment_id);

ALTER TABLE analytics_events ADD COLUMN workflow_id TEXT;
ALTER TABLE analytics_events ADD COLUMN workflow_name TEXT;
ALTER TABLE error_events ADD COLUMN workflow_id TEXT;
ALTER TABLE error_events ADD COLUMN workflow_name TEXT;
ALTER TABLE transactions ADD COLUMN workflow_id TEXT;
ALTER TABLE transactions ADD COLUMN workflow_name TEXT;

CREATE INDEX analytics_events_app_workflow_idx
    ON analytics_events (app_id, workflow_name, occurred_at DESC)
    WHERE workflow_id IS NOT NULL;
CREATE INDEX error_events_app_workflow_idx
    ON error_events (app_id, workflow_name, occurred_at DESC)
    WHERE workflow_id IS NOT NULL;
CREATE INDEX transactions_app_workflow_idx
    ON transactions (app_id, workflow_name, occurred_at DESC)
    WHERE workflow_id IS NOT NULL;
```

Note: `analytics_events`, `error_events` and `transactions` are RANGE-partitioned parents. `ALTER TABLE … ADD COLUMN` and partial `CREATE INDEX` on a partitioned parent both propagate to all partitions in Postgres 12+, so no per-partition step is needed. `workflows` itself is **not** partitioned — do not add partition machinery to it.

- [ ] **Step 2: Write `down.sql`**

```sql
DROP INDEX IF EXISTS transactions_app_workflow_idx;
DROP INDEX IF EXISTS error_events_app_workflow_idx;
DROP INDEX IF EXISTS analytics_events_app_workflow_idx;

ALTER TABLE transactions DROP COLUMN IF EXISTS workflow_name;
ALTER TABLE transactions DROP COLUMN IF EXISTS workflow_id;
ALTER TABLE error_events DROP COLUMN IF EXISTS workflow_name;
ALTER TABLE error_events DROP COLUMN IF EXISTS workflow_id;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS workflow_name;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS workflow_id;

DROP TABLE IF EXISTS workflows;
```

- [ ] **Step 3: Run the migration and verify it round-trips**

```bash
cd backend
diesel migration run
diesel migration redo   # exercises down.sql then up.sql again
```

Expected: both succeed with no error. If `diesel migration redo` fails, `down.sql` is wrong — fix it before continuing.

- [ ] **Step 4: Regenerate `schema.rs`**

```bash
cd backend
diesel print-schema > crates/sauron-db/src/schema.rs
```

Then inspect the diff: it must contain a new `workflows!` table block and the six added columns on the three event tables, and **nothing else**. If unrelated tables changed, your local DB has drifted — reconcile before continuing.

- [ ] **Step 5: Add the `Workflow` model**

In `backend/crates/sauron-db/src/models.rs`, next to the existing `Session` struct, add a struct whose field order **exactly matches** the column order emitted into `schema.rs` for `workflows` (diesel `Queryable` is positional — a mismatch compiles but returns garbage):

```rust
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = crate::schema::workflows)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Workflow {
    pub id: uuid::Uuid,
    pub app_id: uuid::Uuid,
    pub environment_id: uuid::Uuid,
    pub workflow_id: String,
    pub name: String,
    pub session_id: Option<String>,
    pub distinct_id: Option<String>,
    pub device_key: Option<String>,
    pub release: Option<String>,
    pub status: String,
    pub cancel_reason: Option<String>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_event_at: chrono::DateTime<chrono::Utc>,
    pub events_count: i32,
    pub errors_count: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

Match the import/attribute style of the surrounding structs — if `Session` uses bare `Uuid` / `DateTime<Utc>` via top-of-file `use`, do the same rather than fully-qualifying.

- [ ] **Step 6: Verify it compiles**

```bash
cd backend && cargo check -p sauron-db
```

Expected: clean. A `check_for_backend` error means a field type or order mismatch with `schema.rs`.

---
### Task 2: Wire contract — `workflow_id` / `workflow_name` on three item types

**Files:**
- Modify: `backend/crates/sauron-core/src/envelope.rs` (`ErrorItem`, `AnalyticsItem`, `TransactionItem`, and the in-file `GOLDEN` fixture + its test)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub workflow_id: Option<String>` and `pub workflow_name: Option<String>` on `ErrorItem`, `AnalyticsItem`, `TransactionItem`. This is the contract every SDK in Tasks 6–10 mirrors.

**Read first:** how the existing `screen: Option<String>` field is declared on `ErrorItem` and `AnalyticsItem` in the same file — copy its serde attributes exactly so absent-vs-null behaviour matches.

- [ ] **Step 1: Write the failing test**

Add to the test module at the bottom of `backend/crates/sauron-core/src/envelope.rs`:

```rust
#[test]
fn workflow_fields_round_trip_on_event_item() {
    let json = r#"{
        "type": "event",
        "name": "checkout_step",
        "distinct_id": "u1",
        "properties": {},
        "timestamp": "2026-07-29T00:00:00Z",
        "workflow_id": "wf-123",
        "workflow_name": "checkout"
    }"#;
    let item: EnvelopeItem = serde_json::from_str(json).expect("parses");
    match item {
        EnvelopeItem::Event(e) => {
            assert_eq!(e.workflow_id.as_deref(), Some("wf-123"));
            assert_eq!(e.workflow_name.as_deref(), Some("checkout"));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn workflow_fields_are_omitted_when_absent() {
    let json = r#"{
        "type": "event",
        "name": "plain",
        "distinct_id": "u1",
        "properties": {},
        "timestamp": "2026-07-29T00:00:00Z"
    }"#;
    let item: EnvelopeItem = serde_json::from_str(json).expect("parses");
    let back = serde_json::to_value(&item).expect("serializes");
    assert!(back.get("workflow_id").is_none(), "absent field must not serialize");
    assert!(back.get("workflow_name").is_none());
}
```

Adjust the variant name (`EnvelopeItem::Event`) and any required fields to match the actual definitions in the file — read them first rather than assuming.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd backend && cargo test -p sauron-core workflow_fields
```

Expected: FAIL — `no field 'workflow_id' on type 'AnalyticsItem'`.

- [ ] **Step 3: Add the fields**

On each of `ErrorItem`, `AnalyticsItem`, `TransactionItem`, add — placed immediately after the existing `session_id` field so the struct order tells the story:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
```

If the surrounding fields use a different attribute style (e.g. no `skip_serializing_if`), match `screen`'s attributes instead — consistency with the file wins over the snippet above, **except** that absent must not serialize as `null`, because five SDK golden fixtures assert on exact JSON.

- [ ] **Step 4: Fix every construction site**

`cargo check` will list them. Every place that builds one of these three structs needs the two new fields set to `None` (or threaded through, in the pipeline — that is Task 3). Do not add `..Default::default()` to work around this; the compiler exhaustiveness is the safety net.

```bash
cd backend && cargo check --workspace --all-targets
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-core
```

Expected: PASS, including the pre-existing `GOLDEN` fixture test — which must still pass **unchanged**, proving the new fields are invisible when unused. If the golden test fails, your serde attributes are emitting `null` instead of omitting.

---
### Task 3: Pipeline — stamp columns + `bump_workflow` rollup

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (add `bump_workflow`, `apply_workflow_lifecycle`, `WORKFLOW_STALE_MINUTES`)
- Modify: `backend/crates/sauron-db/src/models.rs` (add `workflow_id` / `workflow_name` to the `NewAnalyticsEvent`, `NewErrorEvent`, `NewTransaction` insertables)
- Modify: `backend/crates/sauron-pipeline/src/process.rs` (thread the fields into inserts; call the new repo fns)
- Test: `backend/crates/sauron-db/tests/workflows.rs` (new)

**Interfaces:**
- Consumes: `workflows` table + `Workflow` model (Task 1); `workflow_id`/`workflow_name` on the three item types (Task 2).
- Produces:
  ```rust
  pub const WORKFLOW_STALE_MINUTES: i64 = 30;

  pub async fn bump_workflow(
      conn: &mut AsyncPgConnection,
      app_id: Uuid,
      environment_id: Uuid,
      workflow_id: &str,
      workflow_name: &str,
      session_id: Option<&str>,
      distinct_id: Option<&str>,
      device_key: Option<&str>,
      release: Option<&str>,
      occurred_at: DateTime<Utc>,
      events_delta: i32,
      errors_delta: i32,
  ) -> Result<(), Error>;

  pub async fn apply_workflow_lifecycle(
      conn: &mut AsyncPgConnection,
      app_id: Uuid,
      environment_id: Uuid,
      workflow_id: &str,
      workflow_name: &str,
      action: WorkflowAction,          // Start | End | Cancel
      cancel_reason: Option<&str>,
      session_id: Option<&str>,
      distinct_id: Option<&str>,
      occurred_at: DateTime<Utc>,
  ) -> Result<(), Error>;

  pub enum WorkflowAction { Start, End, Cancel }
  ```
  Signatures must match `bump_session`'s conventions (connection first, `Result<(), Error>` with the crate's own error type) — read `bump_session` and adapt rather than copying the above literally if it disagrees.

**Read first:** `repo::bump_session` in `backend/crates/sauron-db/src/repo.rs` — the `INSERT … ON CONFLICT DO UPDATE` with `GREATEST(last_event_at)` / `LEAST(started_at)` / counter increments, and its exact `sql_query` + `bind::<Type, _>` chaining. `bump_workflow` is that function with `session_id` swapped for `workflow_id`. Also read its three call sites in `sauron-pipeline/src/process.rs` (error → deltas `(0, 1)`, event → `(1, 0)`, transaction → `(0, 0)`).

- [ ] **Step 1: Write the failing tests**

Create `backend/crates/sauron-db/tests/workflows.rs`, following the harness setup used by `backend/crates/sauron-db/tests/env_scoping.rs` (`mod common;` plus whatever fixture fn it calls to get an app + environment):

```rust
mod common;

#[tokio::test]
async fn bump_workflow_inserts_then_accumulates() {
    // 1. bump_workflow(... events_delta=1, errors_delta=0) at t0
    // 2. bump_workflow(... events_delta=0, errors_delta=1) at t0 + 5min
    // assert: exactly one row; events_count == 1; errors_count == 1;
    //         started_at == t0; last_event_at == t0 + 5min; status == "active"
}

#[tokio::test]
async fn bump_workflow_takes_earliest_start_and_latest_activity() {
    // bump at t0+5min FIRST, then bump at t0 (out-of-order arrival)
    // assert: started_at == t0; last_event_at == t0 + 5min
}

#[tokio::test]
async fn lifecycle_end_marks_completed_and_sets_ended_at() {
    // Start at t0, then End at t0 + 2min
    // assert: status == "completed"; ended_at == t0 + 2min
}

#[tokio::test]
async fn lifecycle_cancel_records_reason() {
    // Start, then Cancel with reason "superseded"
    // assert: status == "cancelled"; cancel_reason == Some("superseded")
}

#[tokio::test]
async fn terminal_status_is_not_reverted_by_a_late_bump_or_late_start() {
    // Start, End, then bump_workflow(events_delta=1) and lifecycle Start again
    // assert: status stays "completed"; ended_at unchanged; events_count incremented
}
```

Fill each body in with real assertions — do not leave the comments as the test.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd backend && cargo test -p sauron-db --test workflows
```

Expected: FAIL to compile — `bump_workflow` not found. (These tests need a live Postgres; use the same `DATABASE_URL` / test-DB mechanism `env_scoping.rs` relies on. If the harness creates an ephemeral DB per test, follow that.)

- [ ] **Step 3: Add `bump_workflow`**

Mirror `bump_session`'s body. The upsert:

```sql
INSERT INTO workflows (
    app_id, environment_id, workflow_id, name, session_id, distinct_id,
    device_key, release, started_at, last_event_at, events_count, errors_count
) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$9,$10,$11)
ON CONFLICT (app_id, workflow_id) DO UPDATE SET
    last_event_at = GREATEST(workflows.last_event_at, EXCLUDED.last_event_at),
    started_at    = LEAST(workflows.started_at, EXCLUDED.started_at),
    events_count  = workflows.events_count + EXCLUDED.events_count,
    errors_count  = workflows.errors_count + EXCLUDED.errors_count,
    session_id    = COALESCE(workflows.session_id, EXCLUDED.session_id),
    distinct_id   = COALESCE(workflows.distinct_id, EXCLUDED.distinct_id),
    device_key    = COALESCE(workflows.device_key, EXCLUDED.device_key),
    release       = COALESCE(workflows.release, EXCLUDED.release),
    updated_at    = now()
```

Note it never touches `status`, `ended_at`, or `cancel_reason` — that is what protects a terminal status from a late-arriving stamped event.

- [ ] **Step 4: Add `WorkflowAction` + `apply_workflow_lifecycle`**

Three statements sharing the same insert prelude. For `Start`:

```sql
ON CONFLICT (app_id, workflow_id) DO UPDATE SET
    name       = EXCLUDED.name,
    started_at = LEAST(workflows.started_at, EXCLUDED.started_at),
    updated_at = now()
```

(Deliberately does **not** set `status = 'active'` — a `$workflow_start` arriving after the end event must only backfill `started_at`/`name`.)

For `End` / `Cancel`, insert with `status` = `'completed'` / `'cancelled'` and:

```sql
ON CONFLICT (app_id, workflow_id) DO UPDATE SET
    status        = CASE WHEN workflows.status = 'active' THEN EXCLUDED.status ELSE workflows.status END,
    ended_at      = CASE WHEN workflows.status = 'active' THEN EXCLUDED.ended_at ELSE workflows.ended_at END,
    cancel_reason = CASE WHEN workflows.status = 'active' THEN EXCLUDED.cancel_reason ELSE workflows.cancel_reason END,
    last_event_at = GREATEST(workflows.last_event_at, EXCLUDED.last_event_at),
    started_at    = LEAST(workflows.started_at, EXCLUDED.started_at),
    updated_at    = now()
```

First terminal transition wins; a second one is ignored.

Also add, near the other tuning constants in `repo.rs`:

```rust
/// A workflow still `active` with no activity for this long is reported as abandoned.
/// Matches the breadcrumb-buffer TTL.
pub const WORKFLOW_STALE_MINUTES: i64 = 30;
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test workflows
```

Expected: all five PASS.

- [ ] **Step 6: Thread the stamp into the insertables**

Add `pub workflow_id: Option<String>` and `pub workflow_name: Option<String>` to the `NewAnalyticsEvent`, `NewErrorEvent`, and `NewTransaction` insert structs in `models.rs`, adjacent to their existing `screen` field.

- [ ] **Step 7: Wire `process.rs`**

At each of the three insert sites, populate the two new fields from the item (`item.workflow_id.clone()`, `item.workflow_name.clone()`) exactly the way `screen` is populated.

Then, immediately after each existing `bump_session` call, add a guarded workflow bump using the same deltas as the session bump:

```rust
if let (Some(wf_id), Some(wf_name)) = (item.workflow_id.as_deref(), item.workflow_name.as_deref()) {
    repo::bump_workflow(
        conn, job.app_id, job.environment_id, wf_id, wf_name,
        item.session_id.as_deref(), distinct_id, device_key, job.release.as_deref(),
        occurred_at, /* events_delta */ 1, /* errors_delta */ 0,
    ).await?;
}
```

Use `(0, 1)` in the error path and `(0, 0)` in the transaction path, matching the session deltas. Adapt the variable names to whatever is in scope at each site.

- [ ] **Step 8: Handle the three reserved lifecycle events**

In the event branch of `process_job`, after the `analytics_events` row is inserted, match on the event name:

```rust
let action = match item.name.as_str() {
    "$workflow_start" => Some(WorkflowAction::Start),
    "$workflow_end" => Some(WorkflowAction::End),
    "$workflow_cancel" => Some(WorkflowAction::Cancel),
    _ => None,
};
```

When `Some`, read `workflow_id` / `workflow_name` from the item's stamped fields, falling back to `item.properties["workflow_id"]` / `["workflow_name"]` if the stamp is absent (a hand-rolled client may only send properties). Read `cancel_reason` from `item.properties["reason"]`, truncated to 120 chars. If no workflow id can be resolved, skip the lifecycle call — never error the job. Then call `apply_workflow_lifecycle`.

The event row is still inserted normally: lifecycle events are real analytics events and must remain visible in the events feed.

- [ ] **Step 9: Add a pipeline-level test and run the suite**

Add one test asserting that processing a `$workflow_start` event followed by a stamped `event` followed by `$workflow_end` produces a single `workflows` row with `status = "completed"` and `events_count = 3` (all three events count — the lifecycle events are events too).

```bash
cd backend && cargo test --workspace
```

Expected: PASS, no pre-existing test broken.

---
### Task 4: Read-side repo functions

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs`
- Test: `backend/crates/sauron-db/tests/workflows.rs` (extend)

**Interfaces:**
- Consumes: `workflows` table, stamped columns, `WORKFLOW_STALE_MINUTES` (Tasks 1–3).
- Produces:
  ```rust
  pub struct WorkflowRow {          // one per workflow NAME
      pub name: String,
      pub started: i64,
      pub completed: i64,
      pub cancelled: i64,
      pub abandoned: i64,
      pub active: i64,
      pub unique_users: i64,
      pub median_duration_ms: Option<f64>,
      pub p95_duration_ms: Option<f64>,
      pub last_seen: DateTime<Utc>,
  }
  pub struct WorkflowDetail {
      pub name: String,
      pub started: i64, pub completed: i64, pub cancelled: i64,
      pub abandoned: i64, pub active: i64, pub unique_users: i64,
      pub median_duration_ms: Option<f64>, pub p95_duration_ms: Option<f64>,
      pub duration_buckets: Vec<HistoBucket>,
      pub top_events: Vec<NameCount>,
      pub top_issues: Vec<WorkflowIssue>,
  }
  pub struct WorkflowRun {
      pub workflow_id: String, pub session_id: Option<String>,
      pub distinct_id: Option<String>, pub status: String,
      pub started_at: DateTime<Utc>, pub ended_at: Option<DateTime<Utc>>,
      pub duration_ms: Option<i64>, pub events_count: i32, pub errors_count: i32,
  }
  pub struct WorkflowSpan {         // for the session timeline lane
      pub workflow_id: String, pub name: String, pub status: String,
      pub started_at: DateTime<Utc>, pub ended_at: Option<DateTime<Utc>>,
  }
  pub struct NameCount { pub name: String, pub count: i64 }
  pub struct WorkflowIssue { pub issue_id: Uuid, pub title: String, pub count: i64 }

  pub async fn workflow_list(conn, app_id, scope: &ReadScope, since_days: i32,
      search: Option<&str>, limit: i64, offset: i64) -> Result<Vec<WorkflowRow>, Error>;
  pub async fn workflow_detail(conn, app_id, scope: &ReadScope, name: &str,
      since_days: i32) -> Result<WorkflowDetail, Error>;
  pub async fn workflow_runs(conn, app_id, scope: &ReadScope, name: &str,
      since_days: i32, status: Option<&str>, limit: i64, offset: i64)
      -> Result<Vec<WorkflowRun>, Error>;
  pub async fn workflow_spans_for_session(conn, app_id, scope: &ReadScope,
      session_id: &str) -> Result<Vec<WorkflowSpan>, Error>;
  ```
  Reuse `HistoBucket` if `repo.rs` already defines one for `DurationHistogram`; otherwise define it as `{ pub bucket: String, pub count: i64 }`.

**Read first:** `repo::screen_list` and `repo::screen_stats` in `repo.rs` — they are the on-read-aggregation template: raw `sql_query`, `ReadScope::sql_fragment(idx)` spliced into the WHERE clause, positional binds in the same order, `QueryableByName` result structs. Also read `backend/crates/sauron-db/src/scope.rs` to get `sql_fragment` / `sql_fragment_for` and the bind-count bookkeeping right. **Never interpolate user input into SQL** — `name`, `search`, and `status` are all bound params.

- [ ] **Step 1: Write the failing tests**

Extend `backend/crates/sauron-db/tests/workflows.rs`:

```rust
#[tokio::test]
async fn workflow_list_derives_abandoned_from_staleness() {
    // Seed 4 rows for name "checkout" in the same app+env:
    //   A: status completed, ended 1min after start
    //   B: status cancelled
    //   C: status active, last_event_at = now()               -> active
    //   D: status active, last_event_at = now() - 45 minutes   -> abandoned
    // assert: one row; started == 4; completed == 1; cancelled == 1;
    //         active == 1; abandoned == 1
}

#[tokio::test]
async fn workflow_list_is_environment_scoped() {
    // Same name seeded in env A and env B; query with a ReadScope for env A
    // assert: counts reflect only env A's rows
}

#[tokio::test]
async fn workflow_list_search_filters_by_name_substring() { /* ... */ }

#[tokio::test]
async fn workflow_runs_filters_by_status_and_paginates() { /* ... */ }

#[tokio::test]
async fn workflow_detail_counts_contained_events_and_issues() {
    // Seed a workflow plus 3 analytics_events and 2 error_events stamped with its
    // workflow_id, the errors sharing one issue_id
    // assert: top_events has the 3 event names; top_issues has 1 entry with count 2
}

#[tokio::test]
async fn workflow_spans_for_session_returns_ordered_spans() { /* ... */ }
```

Write real assertions in place of the comments.

- [ ] **Step 2: Run to verify they fail**

```bash
cd backend && cargo test -p sauron-db --test workflows
```

Expected: FAIL to compile — `workflow_list` not found.

- [ ] **Step 3: Implement `workflow_list`**

The status projection is a single expression reused everywhere — define it once as a Rust `const &str` so list and detail cannot drift:

```rust
const WORKFLOW_EFFECTIVE_STATUS: &str = "\
CASE WHEN w.status = 'active' AND w.last_event_at < now() - make_interval(mins => $STALE$) \
     THEN 'abandoned' ELSE w.status END";
```

Substitute `$STALE$` with `WORKFLOW_STALE_MINUTES` via `format!` — it is an integer constant, not user input, so this is safe.

The query shape:

```sql
SELECT w.name,
       COUNT(*) AS started,
       COUNT(*) FILTER (WHERE eff = 'completed') AS completed,
       COUNT(*) FILTER (WHERE eff = 'cancelled') AS cancelled,
       COUNT(*) FILTER (WHERE eff = 'abandoned') AS abandoned,
       COUNT(*) FILTER (WHERE eff = 'active')    AS active,
       COUNT(DISTINCT w.distinct_id)             AS unique_users,
       percentile_cont(0.5)  WITHIN GROUP (ORDER BY dur) AS median_duration_ms,
       percentile_cont(0.95) WITHIN GROUP (ORDER BY dur) AS p95_duration_ms,
       MAX(w.last_event_at)                      AS last_seen
FROM (
    SELECT w.*, <EFFECTIVE_STATUS> AS eff,
           CASE WHEN w.ended_at IS NOT NULL
                THEN EXTRACT(EPOCH FROM (w.ended_at - w.started_at)) * 1000 END AS dur
    FROM workflows w
    WHERE w.app_id = $1 AND w.started_at >= now() - make_interval(days => $2)
      AND <SCOPE_FRAGMENT>
) w
WHERE ($N IS NULL OR w.name ILIKE '%' || $N || '%')
GROUP BY w.name
ORDER BY started DESC, w.name ASC
LIMIT $... OFFSET $...
```

Only `completed` and `cancelled` rows have an `ended_at`, so `dur` is naturally NULL for active/abandoned and `percentile_cont` ignores them — that is the intended semantic (duration describes finished runs).

Use `COUNT(DISTINCT w.distinct_id)` for unique users, matching the existing exact-count idiom in the read-side repo functions. Do not introduce HLL here; the HLL in this codebase is only for issue-affected-users on the write path.

- [ ] **Step 4: Implement `workflow_detail`**

One query for the outcome/duration aggregate (the same subquery as above, filtered by `name`), plus:

- `duration_buckets`: `width_bucket` over the finished runs, or reuse whatever bucketing `repo`'s existing latency-histogram function uses — prefer reuse.
- `top_events`: `SELECT name, COUNT(*) FROM analytics_events WHERE app_id=$1 AND workflow_name=$2 AND occurred_at >= … AND <scope> AND name NOT LIKE '$workflow%' GROUP BY name ORDER BY 2 DESC LIMIT 10` — the `NOT LIKE` excludes the three reserved lifecycle events, which would otherwise dominate every list.
- `top_issues`: join `error_events` (filtered the same way) to `issues` on `issue_id`, group by issue, `LIMIT 10`.

- [ ] **Step 5: Implement `workflow_runs` and `workflow_spans_for_session`**

`workflow_runs` selects individual rows with the effective-status projection and a computed `duration_ms`, optionally filtered by a bound `status` param (accepting `abandoned` by comparing against the projection, not the raw column). `workflow_spans_for_session` selects `workflow_id, name, status, started_at, ended_at` for one `session_id`, ordered by `started_at ASC`.

- [ ] **Step 6: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test workflows
```

Expected: all PASS. Then `cargo clippy -p sauron-db -- -D warnings` clean.

---
### Task 5: API routes + `workflow` filter on Issues/Events

**Files:**
- Create: `backend/bins/sauron-api/src/routes/workflows.rs`
- Modify: `backend/bins/sauron-api/src/routes/mod.rs` (declare `pub mod workflows;`)
- Modify: `backend/bins/sauron-api/src/main.rs` (register the four routes)
- Modify: `backend/bins/sauron-api/src/routes/sessions.rs` (the session-workflows sub-route, if it belongs there rather than in `workflows.rs` — follow whichever grouping the file layout implies)
- Modify: `backend/bins/sauron-api/src/routes/issues.rs` and `.../events` handler (accept a `workflow` filter field)
- Test: `backend/bins/sauron-api/tests/http_workflows.rs` (new)

**Interfaces:**
- Consumes: `repo::workflow_list`, `workflow_detail`, `workflow_runs`, `workflow_spans_for_session` (Task 4).
- Produces these routes, all guarded by `authorize_app(..., perm::EVENT_READ)` and env-scoped via `super::scope::read_scope(app_id, environment_id)`:
  - `GET /v1/apps/{app_id}/workflows` → `Vec<WorkflowRow>`
  - `GET /v1/apps/{app_id}/workflows/{name}` → `WorkflowDetail`
  - `GET /v1/apps/{app_id}/workflows/{name}/runs` → `Vec<WorkflowRun>`
  - `GET /v1/apps/{app_id}/sessions/{session_id}/workflows` → `Vec<WorkflowSpan>`

**Read first:** `backend/bins/sauron-api/src/routes/screens.rs` end to end — it is the closest template (list + detail, `since_days` query struct, guard, `read_scope`, repo call, `Json(...)`). Copy its structure, including how it validates and clamps `limit`/`offset`.

**Critical:** the dashboard's axios interceptor auto-injects `environment_id` on every `/v1/apps/{id}/…` URL. These routes must **accept** it via `read_scope`. Do **not** add them to any reject-list, and do not call `reject_environment_id`.

- [ ] **Step 1: Write the failing http tests**

Create `backend/bins/sauron-api/tests/http_workflows.rs`, cloning the app/token/env setup from `backend/bins/sauron-api/tests/http_env_scoping.rs`:

```rust
#[tokio::test]
async fn get_workflows_returns_rollup_rows() {
    // seed 2 workflow rows for "checkout" (1 completed, 1 cancelled)
    // GET /v1/apps/{app}/workflows?since_days=30
    // assert 200; body[0].name == "checkout"; started == 2; completed == 1
}

#[tokio::test]
async fn get_workflows_is_environment_scoped() {
    // seed under env A only; GET with environment_id = env B
    // assert 200 and an empty array (not a 400, not env A's data)
}

#[tokio::test]
async fn get_workflows_requires_event_read_permission() {
    // token lacking EVENT_READ -> 403
}

#[tokio::test]
async fn get_workflow_detail_and_runs() {
    // GET /workflows/checkout and /workflows/checkout/runs; assert 200 + shape
}

#[tokio::test]
async fn workflow_name_with_slash_or_unicode_is_handled() {
    // percent-encoded name in the path round-trips to the right row
}

#[tokio::test]
async fn get_session_workflows_returns_spans_in_order() { /* ... */ }

#[tokio::test]
async fn issues_workflow_filter_narrows_results() {
    // two issues, one with error_events stamped workflow_name=checkout
    // GET /v1/apps/{app}/issues?filter=workflow:eq:checkout -> only that issue
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd backend && cargo test -p sauron-api --test http_workflows
```

Expected: FAIL — 404 on the routes.

- [ ] **Step 3: Write `routes/workflows.rs`**

Four handlers. Query params, matching the conventions in `screens.rs`/`sessions.rs`:

```rust
#[derive(Deserialize)]
pub struct WorkflowListQuery {
    #[serde(default)] pub since_days: Option<i32>,
    #[serde(default)] pub search: Option<String>,
    #[serde(default)] pub limit: Option<i64>,
    #[serde(default)] pub offset: Option<i64>,
    #[serde(default)] pub environment_id: Option<Uuid>,
}
```

Clamp `since_days` to `1..=90` defaulting to 30, `limit` to `1..=200` defaulting to 50, `offset` to `>= 0` — mirroring the clamps already used by `screens.rs`. For the runs handler add `status: Option<String>` validated against the set `active | completed | cancelled | abandoned`, returning 400 on anything else.

Each handler body follows the house order exactly: `let mut conn = db(&state).await?;` → `authorize_app(&state, &claims, app_id, perm::EVENT_READ).await?;` → `let scope = super::scope::read_scope(app_id, query.environment_id).await?;` → repo call → `Ok(Json(rows))`.

- [ ] **Step 4: Register the routes**

Add `pub mod workflows;` to `routes/mod.rs`, then in `main.rs` beside the existing screens/sessions registrations:

```rust
.route("/v1/apps/{app_id}/workflows", get(routes::workflows::list))
.route("/v1/apps/{app_id}/workflows/{name}", get(routes::workflows::detail))
.route("/v1/apps/{app_id}/workflows/{name}/runs", get(routes::workflows::runs))
.route("/v1/apps/{app_id}/sessions/{session_id}/workflows", get(routes::workflows::session_spans))
```

Match the exact path-param syntax already used in that file (`{app_id}` vs `:app_id` differs by axum version — copy the neighbours).

- [ ] **Step 5: Add the `workflow` filter field to issues + events**

In the server-side filter handling used by the issues and events list endpoints, add a `workflow` field that predicates on `workflow_name` with the `eq` / `neq` / `contains` operators, bound as a parameter. Follow exactly how the existing `screen` or tag filter field is registered and translated — do not add a new mechanism.

- [ ] **Step 6: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-api --test http_workflows
cd backend && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all PASS, clippy clean.

- [ ] **Step 7: Smoke the endpoints against a running API**

Start the API locally and confirm each route answers 200 with a real token:

```bash
curl -s -H "Authorization: Bearer $TOKEN" \
  "http://localhost:8080/v1/apps/$APP_ID/workflows?since_days=30&environment_id=$ENV_ID" | jq .
```

Expected: a JSON array (empty is fine at this stage). A 400 here means `environment_id` is being rejected — fix the scoping, per the Critical note above.

---
### Task 6: JS SDK (`sdks/js`) — reference implementation

**Files:**
- Create: `sdks/js/src/workflow.ts`
- Modify: `sdks/js/src/api/product.ts` (add `startWorkflow`/`endWorkflow`/`cancelWorkflow`; stamp `track` and `buildTransactionItem`)
- Modify: `sdks/js/src/api/capture.ts` (stamp `captureException` and `captureMessage`)
- Modify: `sdks/js/src/types.ts` (`ErrorItem`, `EventItem`, `TransactionItem`, new `WorkflowStatus` / `WorkflowResult` / `ActiveWorkflow`)
- Modify: `sdks/js/src/index.ts` (export the four functions + types; add to the `Sauron` facade)
- Modify: `sdks/js/src/client.ts` (reset workflow state in `teardown()`)
- Modify: `sdks/js/package.json` + `sdks/js/src/utils.ts` (version `1.2.0` → `1.3.0`)
- Modify: `sdks/js/test/envelope.test.ts` (version assertion)
- Test: `sdks/js/test/workflow.test.ts` (new)

**Interfaces:**
- Consumes: the wire contract from Task 2 (`workflow_id`, `workflow_name`, omitted when null).
- Produces the API every other SDK task mirrors:
  ```ts
  export type WorkflowStatus =
    | 'ok' | 'already_active' | 'not_active'
    | 'name_mismatch' | 'invalid_name' | 'disabled';
  export interface WorkflowResult { status: WorkflowStatus; workflowId?: string }
  export interface ActiveWorkflow { workflowId: string; name: string; startedAt: string }

  function startWorkflow(name: string, options?: { force?: boolean }): WorkflowResult;
  function endWorkflow(name?: string): WorkflowResult;
  function cancelWorkflow(name?: string, options?: { reason?: string }): WorkflowResult;
  function getWorkflow(): ActiveWorkflow | null;
  ```

**Read first:** `sdks/js/src/screen.ts` (the whole 22-line file) and `sdks/js/test/screen.test.ts` — this task is that pattern, extended. Also `sdks/js/src/api/product.ts:42-45` for how `setScreen` emits its `$screen` event, and `sdks/js/src/identity.ts:74-89` for uuid/id generation helpers.

**Contract (identical across Tasks 6–10):**

| Call | Precondition | Result | Side effect |
|---|---|---|---|
| `startWorkflow(name)` | not initialised / closed | `disabled` | none |
| | name empty after trim, or > 120 chars | `invalid_name` | none |
| | a workflow is active, `force` falsy | `already_active` | debug warn only |
| | a workflow is active, `force: true` | `ok` | emit `$workflow_cancel` for the old one with `reason: 'superseded'`, then start the new one |
| | none active | `ok` | set state, emit `$workflow_start` |
| `endWorkflow(name?)` | none active | `not_active` | none |
| | `name` given and ≠ active name | `name_mismatch` | none |
| | otherwise | `ok` | emit `$workflow_end` with `duration_ms`, clear state |
| `cancelWorkflow(name?, {reason})` | none active | `not_active` | none |
| | `name` given and ≠ active name | `name_mismatch` | none |
| | otherwise | `ok` | emit `$workflow_cancel` with `duration_ms` + `reason` (default `'user'`), clear state |

The lifecycle events are emitted through the SDK's own `track()` so they carry the normal session/screen/scope context. `$workflow_start` is emitted **after** the state is set, so it is itself stamped with the new workflow. `$workflow_end` / `$workflow_cancel` are emitted **before** the state is cleared, so they are stamped with the workflow they close.

- [ ] **Step 1: Write the failing tests**

Create `sdks/js/test/workflow.test.ts`, using the same mock-transport capture harness as `test/screen.test.ts`:

```ts
import { describe, it, expect, beforeEach } from 'vitest';

describe('workflows', () => {
  beforeEach(() => { /* re-init the SDK with the mock transport, as screen.test.ts does */ });

  it('start returns ok and emits $workflow_start stamped with the new workflow', () => {
    const r = startWorkflow('checkout');
    expect(r.status).toBe('ok');
    expect(r.workflowId).toBeTruthy();
    const item = lastItem();
    expect(item.name).toBe('$workflow_start');
    expect(item.workflow_name).toBe('checkout');
    expect(item.workflow_id).toBe(r.workflowId);
    expect(item.properties.workflow_name).toBe('checkout');
  });

  it('stamps subsequent track calls with the active workflow', () => {
    const r = startWorkflow('checkout');
    track('add_to_cart');
    expect(lastItem().workflow_id).toBe(r.workflowId);
    expect(lastItem().workflow_name).toBe('checkout');
  });

  it('omits the fields entirely when no workflow is active', () => {
    track('plain');
    expect('workflow_id' in lastItem()).toBe(false);
    expect('workflow_name' in lastItem()).toBe(false);
  });

  it('start while active returns already_active and changes nothing', () => {
    const first = startWorkflow('onboarding');
    const before = itemCount();
    const second = startWorkflow('checkout');
    expect(second.status).toBe('already_active');
    expect(itemCount()).toBe(before);
    expect(getWorkflow()!.workflowId).toBe(first.workflowId);
  });

  it('force cancels the old with reason superseded then starts the new', () => {
    const first = startWorkflow('onboarding');
    const second = startWorkflow('checkout', { force: true });
    expect(second.status).toBe('ok');
    const [cancel, start] = lastItems(2);
    expect(cancel.name).toBe('$workflow_cancel');
    expect(cancel.workflow_id).toBe(first.workflowId);
    expect(cancel.properties.reason).toBe('superseded');
    expect(start.name).toBe('$workflow_start');
    expect(start.workflow_id).toBe(second.workflowId);
  });

  it('end emits $workflow_end with duration_ms and clears state', () => {
    startWorkflow('checkout');
    expect(endWorkflow().status).toBe('ok');
    expect(lastItem().name).toBe('$workflow_end');
    expect(typeof lastItem().properties.duration_ms).toBe('number');
    expect(getWorkflow()).toBeNull();
  });

  it('end with a mismatched name is a no-op returning name_mismatch', () => {
    startWorkflow('checkout');
    const before = itemCount();
    expect(endWorkflow('onboarding').status).toBe('name_mismatch');
    expect(itemCount()).toBe(before);
    expect(getWorkflow()).not.toBeNull();
  });

  it('end with no active workflow returns not_active', () => {
    expect(endWorkflow().status).toBe('not_active');
  });

  it('cancel defaults reason to user and caps a long reason at 120 chars', () => {
    startWorkflow('checkout');
    cancelWorkflow(undefined, { reason: 'x'.repeat(300) });
    expect(lastItem().properties.reason).toHaveLength(120);
  });

  it('rejects an empty or over-long name without starting anything', () => {
    expect(startWorkflow('   ').status).toBe('invalid_name');
    expect(startWorkflow('n'.repeat(121)).status).toBe('invalid_name');
    expect(getWorkflow()).toBeNull();
  });

  it('trims the name', () => {
    startWorkflow('  checkout  ');
    expect(getWorkflow()!.name).toBe('checkout');
  });

  it('stamps captureException and trackTransaction too', () => {
    const r = startWorkflow('checkout');
    captureException(new Error('boom'));
    expect(lastItem().workflow_id).toBe(r.workflowId);
    trackTransaction({ name: 'load', op: 'navigation', durationMs: 12 });
    expect(lastItem().workflow_id).toBe(r.workflowId);
  });
});
```

Add the small `lastItem()` / `lastItems(n)` / `itemCount()` helpers over the captured envelopes, matching how `screen.test.ts` reads captured items.

- [ ] **Step 2: Run to verify they fail**

```bash
cd sdks/js && npx vitest run test/workflow.test.ts
```

Expected: FAIL — `startWorkflow is not exported`.

- [ ] **Step 3: Write `src/workflow.ts`**

```ts
import type { ActiveWorkflow } from './types.js';

export const WORKFLOW_NAME_MAX = 120;
export const WORKFLOW_REASON_MAX = 120;

let current: ActiveWorkflow | null = null;

export function getWorkflowState(): ActiveWorkflow | null {
  return current;
}

export function setWorkflowState(workflow: ActiveWorkflow | null): void {
  current = workflow;
}

export function resetWorkflow(): void {
  current = null;
}

/** Returns the trimmed name, or null when invalid. */
export function normalizeWorkflowName(name: unknown): string | null {
  if (typeof name !== 'string') return null;
  const trimmed = name.trim();
  if (trimmed.length === 0 || trimmed.length > WORKFLOW_NAME_MAX) return null;
  return trimmed;
}

export function normalizeReason(reason: unknown): string {
  if (typeof reason !== 'string' || reason.trim().length === 0) return 'user';
  return reason.trim().slice(0, WORKFLOW_REASON_MAX);
}
```

- [ ] **Step 4: Add the types**

In `sdks/js/src/types.ts`, add `WorkflowStatus`, `WorkflowResult`, `ActiveWorkflow` exactly as given in the Interfaces block above, and add to `ErrorItem`, `EventItem`, and `TransactionItem`:

```ts
  workflow_id?: string;
  workflow_name?: string;
```

Optional (not `| null`) so `JSON.stringify` omits them when unset — the Rust side omits absent fields, and the golden fixtures must stay byte-identical.

- [ ] **Step 5: Implement the three functions in `src/api/product.ts`**

```ts
export function startWorkflow(client: Client | null, name: string, options?: { force?: boolean }): WorkflowResult {
  if (!client) return { status: 'disabled' };
  const normalized = normalizeWorkflowName(name);
  if (!normalized) {
    client.logger.warn(`startWorkflow: invalid name ${String(name)}`);
    return { status: 'invalid_name' };
  }
  const active = getWorkflowState();
  if (active) {
    if (!options?.force) {
      client.logger.warn(`startWorkflow("${normalized}"): "${active.name}" is already active; pass force to replace it`);
      return { status: 'already_active' };
    }
    emitWorkflowEnd(client, active, '$workflow_cancel', 'superseded');
  }
  const workflow: ActiveWorkflow = {
    workflowId: newUuid(),
    name: normalized,
    startedAt: new Date().toISOString(),
  };
  setWorkflowState(workflow);
  track(client, '$workflow_start', { workflow_id: workflow.workflowId, workflow_name: workflow.name });
  return { status: 'ok', workflowId: workflow.workflowId };
}
```

`endWorkflow` / `cancelWorkflow` share one helper:

```ts
function closeWorkflow(client: Client | null, eventName: '$workflow_end' | '$workflow_cancel',
                       name?: string, reason?: string): WorkflowResult {
  if (!client) return { status: 'disabled' };
  const active = getWorkflowState();
  if (!active) return { status: 'not_active' };
  if (name !== undefined && normalizeWorkflowName(name) !== active.name) {
    client.logger.warn(`${eventName}: "${name}" does not match active workflow "${active.name}"`);
    return { status: 'name_mismatch' };
  }
  emitWorkflowEnd(client, active, eventName, reason);
  return { status: 'ok', workflowId: active.workflowId };
}

function emitWorkflowEnd(client: Client, active: ActiveWorkflow,
                         eventName: '$workflow_end' | '$workflow_cancel', reason?: string): void {
  const props: Record<string, unknown> = {
    workflow_id: active.workflowId,
    workflow_name: active.name,
    duration_ms: Math.max(0, Date.now() - Date.parse(active.startedAt)),
  };
  if (eventName === '$workflow_cancel') props.reason = normalizeReason(reason);
  track(client, eventName, props);        // emitted while state is still set, so it gets stamped
  resetWorkflow();
}
```

Reuse the existing uuid helper from `src/identity.ts` rather than adding a new one. Match the actual `track(...)` signature and the actual way `product.ts` reaches the client and logger — adapt the snippets, do not paste blindly.

- [ ] **Step 6: Stamp the four item-construction sites**

In `src/api/product.ts` where the event item is built, and in `buildTransactionItem`, and in `src/api/capture.ts` for both `captureException` and `captureMessage`, add — right beside the existing `screen` assignment:

```ts
  const wf = getWorkflowState();
  ...
  ...(wf ? { workflow_id: wf.workflowId, workflow_name: wf.name } : {}),
```

Spread-when-present, so the keys are absent (not `undefined`) when there is no workflow.

- [ ] **Step 7: Export and reset**

In `src/index.ts`, add the four thin wrappers next to `setScreen`/`getScreen`, add them to the `Sauron` facade object, and re-export `WorkflowStatus` / `WorkflowResult` / `ActiveWorkflow` from the type block. In `src/client.ts`, call `resetWorkflow()` inside `teardown()` next to the existing `resetScreen()`.

- [ ] **Step 8: Bump the version in both places**

`sdks/js/package.json` `"version": "1.3.0"` and `SDK_VERSION` in `sdks/js/src/utils.ts`, then update the version assertion in `sdks/js/test/envelope.test.ts`.

- [ ] **Step 9: Run the full suite**

```bash
cd sdks/js && npm test && npx tsc --noEmit
```

Expected: PASS, including the untouched golden envelope test — proving the new fields are absent when no workflow is used.

---
### Task 7: Flutter SDK (`sdks/flutter`)

**Files:**
- Create: `sdks/flutter/lib/src/workflow.dart` (`WorkflowStatus`, `WorkflowResult`, `ActiveWorkflow`, name/reason normalisation)
- Modify: `sdks/flutter/lib/src/client.dart` (private `_currentWorkflow` field, the three methods, stamping)
- Modify: `sdks/flutter/lib/src/envelope.dart` (`workflowId`/`workflowName` on `ErrorItem`, `EventItem`, `TransactionItem`; bump `kSauronSdkVersion`)
- Modify: `sdks/flutter/lib/src/sauron.dart` (static facade methods)
- Modify: `sdks/flutter/lib/sauron_flutter.dart` (both export show-clauses)
- Modify: `sdks/flutter/pubspec.yaml` (`1.3.0` → `1.4.0`)
- Modify: `sdks/flutter/test/envelope_test.dart` (version assertion)
- Test: `sdks/flutter/test/workflow_test.dart` (new)

**Interfaces:**
- Consumes: the wire contract from Task 2.
- Produces:
  ```dart
  enum WorkflowStatus { ok, alreadyActive, notActive, nameMismatch, invalidName, disabled }
  class WorkflowResult { final WorkflowStatus status; final String? workflowId; }
  class ActiveWorkflow { final String workflowId; final String name; final DateTime startedAt; }

  // on SauronClient and the Sauron static facade:
  WorkflowResult startWorkflow(String name, {bool force = false});
  WorkflowResult endWorkflow([String? name]);
  WorkflowResult cancelWorkflow([String? name, String? reason]);
  ActiveWorkflow? get workflow;
  ```
  Wire values are snake_case regardless of the Dart enum casing: `already_active`, `not_active`, `name_mismatch`, `invalid_name`.

**Read first:** `sdks/flutter/lib/src/client.dart` — `String? _currentScreen` (field, getter, and the change-guarded `setScreen` that emits `$screen`), the stamp sites in the error path and the track path, and the `_log` helper. Also `sdks/flutter/test/screen_test.dart` for the mock-http capture harness and `buildClient` helper. Use `generateUuidV4()` from `lib/src/util/uuid.dart`.

**Contract:**

| Call | Precondition | Result | Side effect |
|---|---|---|---|
| `startWorkflow(name)` | client closed / not enabled | `disabled` | none |
| | name empty after trim, or > 120 chars | `invalidName` | none |
| | active and `force == false` | `alreadyActive` | `_log` warning only |
| | active and `force == true` | `ok` | emit `$workflow_cancel` for the old with `reason: 'superseded'`, then start the new |
| | none active | `ok` | set field, emit `$workflow_start` |
| `endWorkflow([name])` | none active | `notActive` | none |
| | `name` given and ≠ active name | `nameMismatch` | none |
| | otherwise | `ok` | emit `$workflow_end` with `duration_ms`, clear field |
| `cancelWorkflow([name, reason])` | none active | `notActive` | none |
| | `name` given and ≠ active name | `nameMismatch` | none |
| | otherwise | `ok` | emit `$workflow_cancel` with `duration_ms` + `reason` (default `'user'`, capped at 120), clear field |

`$workflow_start` is emitted after the field is set; the close events are emitted before it is cleared — so all three carry the workflow's own stamp. Lifecycle events go through the client's own `track()`.

**Flutter-specific warning:** per the project's own notes, zone-related SDK bugs are invisible to both `flutter test` and logcat. Keep every new method's body inside a `try`/`catch` that routes to `_log` and returns a status — never let an exception escape into a zone handler.

- [ ] **Step 1: Write the failing tests**

Create `sdks/flutter/test/workflow_test.dart` using the harness from `test/screen_test.dart`:

```dart
void main() {
  test('start emits \$workflow_start stamped with the new workflow', () async {
    final client = buildClient();
    final r = client.startWorkflow('checkout');
    expect(r.status, WorkflowStatus.ok);
    await client.flush();
    final item = lastItemJson();
    expect(item['name'], '\$workflow_start');
    expect(item['workflow_name'], 'checkout');
    expect(item['workflow_id'], r.workflowId);
  });

  test('stamps subsequent track and captureException calls', () async { /* ... */ });

  test('omits both keys when no workflow is active', () async {
    final client = buildClient();
    client.track('plain');
    await client.flush();
    expect(lastItemJson().containsKey('workflow_id'), isFalse);
    expect(lastItemJson().containsKey('workflow_name'), isFalse);
  });

  test('start while active returns alreadyActive and emits nothing', () async { /* ... */ });

  test('force cancels with reason superseded then starts the new one', () async { /* ... */ });

  test('end emits \$workflow_end with duration_ms and clears the field', () async { /* ... */ });

  test('end with a mismatched name is a no-op returning nameMismatch', () async { /* ... */ });

  test('end with none active returns notActive', () async { /* ... */ });

  test('cancel defaults reason to user and caps a long reason at 120 chars', () async { /* ... */ });

  test('rejects empty and over-long names', () async { /* ... */ });

  test('after close(), startWorkflow returns disabled and does not throw', () async { /* ... */ });
}
```

Write real assertions in place of every `/* ... */`.

- [ ] **Step 2: Run to verify they fail**

```bash
cd sdks/flutter && flutter test test/workflow_test.dart
```

Expected: FAIL to compile — `startWorkflow` is not defined.

- [ ] **Step 3: Write `lib/src/workflow.dart`**

```dart
const int kWorkflowNameMax = 120;
const int kWorkflowReasonMax = 120;

enum WorkflowStatus { ok, alreadyActive, notActive, nameMismatch, invalidName, disabled }

class WorkflowResult {
  const WorkflowResult(this.status, [this.workflowId]);
  final WorkflowStatus status;
  final String? workflowId;
}

class ActiveWorkflow {
  ActiveWorkflow({required this.workflowId, required this.name, required this.startedAt});
  final String workflowId;
  final String name;
  final DateTime startedAt;
}

/// Returns the trimmed name, or null when invalid.
String? normalizeWorkflowName(String? name) {
  if (name == null) return null;
  final trimmed = name.trim();
  if (trimmed.isEmpty || trimmed.length > kWorkflowNameMax) return null;
  return trimmed;
}

String normalizeWorkflowReason(String? reason) {
  final trimmed = reason?.trim() ?? '';
  if (trimmed.isEmpty) return 'user';
  return trimmed.length > kWorkflowReasonMax ? trimmed.substring(0, kWorkflowReasonMax) : trimmed;
}
```

- [ ] **Step 4: Add the wire fields**

In `lib/src/envelope.dart`, add `final String? workflowId;` and `final String? workflowName;` to `ErrorItem`, `EventItem`, and `TransactionItem` (constructor params + fields), and in each `toJson()` emit them **only when non-null**, matching how `screen` is conditionally emitted:

```dart
    if (workflowId != null) 'workflow_id': workflowId,
    if (workflowName != null) 'workflow_name': workflowName,
```

- [ ] **Step 5: Implement the client methods**

In `lib/src/client.dart`, add `ActiveWorkflow? _currentWorkflow;` next to `_currentScreen`, a public `ActiveWorkflow? get workflow => _currentWorkflow;`, and the three methods following the contract table. Stamp `workflowId: _currentWorkflow?.workflowId` and `workflowName: _currentWorkflow?.name` at every item-construction site that already sets `screen` — plus the transaction site, which currently sets `sessionId` but no `screen`.

- [ ] **Step 6: Expose on the facade and exports**

Add `startWorkflow` / `endWorkflow` / `cancelWorkflow` / `workflow` to `lib/src/sauron.dart` beside `setScreen`, delegating to the active client and returning `WorkflowResult(WorkflowStatus.disabled)` when there is none. Add `WorkflowStatus`, `WorkflowResult`, `ActiveWorkflow` to the export show-clauses in `lib/sauron_flutter.dart`.

- [ ] **Step 7: Bump the version in both places**

`sdks/flutter/pubspec.yaml` → `1.4.0`, `kSauronSdkVersion` in `lib/src/envelope.dart` → `1.4.0`, and update the assertion in `test/envelope_test.dart`.

- [ ] **Step 8: Run the suite**

```bash
cd sdks/flutter && flutter analyze && flutter test
```

Expected: analyze clean, all tests PASS including the unchanged golden `envelope_test.dart` payload (only its version string changes).

---
### Task 8: Node SDK (`sdks/node`) — scope-bound

**Files:**
- Create: `sdks/node/src/workflow.ts` (types + normalisation helpers, no global state)
- Modify: `sdks/node/src/scope.ts` (a `workflow` field on `Scope`, cloned by `PushScope`/`withScope`, applied in `applyToEvent`)
- Modify: `sdks/node/src/client.ts` (the three methods; stamp `track`, `captureException`, `captureMessage`, `trackTransaction`)
- Modify: `sdks/node/src/types.ts` (`ErrorItem`, `EventItem`, `TransactionItem`, + the three workflow types)
- Modify: `sdks/node/src/index.ts` (delegating exports)
- Modify: `sdks/node/package.json` + `sdks/node/src/transport.ts` (`SDK_VERSION`) → `1.3.0`
- Modify: `sdks/node/test/envelope.test.ts`, `sdks/node/test/transport.test.ts` (version assertions)
- Test: `sdks/node/test/workflow.test.ts` (new)

**Interfaces:**
- Consumes: the wire contract from Task 2.
- Produces:
  ```ts
  export type WorkflowStatus =
    | 'ok' | 'already_active' | 'not_active'
    | 'name_mismatch' | 'invalid_name' | 'disabled';
  export interface WorkflowResult { status: WorkflowStatus; workflowId?: string }
  export interface ActiveWorkflow { workflowId: string; name: string; startedAt: string }

  function startWorkflow(name: string, options?: { force?: boolean }): WorkflowResult;
  function endWorkflow(name?: string): WorkflowResult;
  function cancelWorkflow(name?: string, options?: { reason?: string }): WorkflowResult;
  function getWorkflow(): ActiveWorkflow | null;
  ```

**CRITICAL — do not use a module global.** `sdks/node` deliberately isolates per-request state in `AsyncLocalStorage` (`src/scope.ts`: `globalScope`, `getCurrentScope()`, `withScope`). The active workflow is a field on `Scope`, read through `getCurrentScope()`. A module-level `let currentWorkflow` would leak one HTTP request's workflow into another's telemetry. This is why this SDK never implemented `setScreen`.

**Read first:** `sdks/node/src/scope.ts` in full — how existing fields (`user`, `tags`, `contexts`) are declared, how a scope is cloned on push, and how `applyToEvent` merges them onto an outgoing item. Add `workflow` the same way. Also `sdks/node/src/client.ts` lines around the `screen: null` assignments — those are your stamp sites.

**Contract:** identical to the JS SDK.

| Call | Precondition | Result | Side effect |
|---|---|---|---|
| `startWorkflow(name)` | no active client | `disabled` | none |
| | name empty after trim, or > 120 chars | `invalid_name` | none |
| | current scope has a workflow, `force` falsy | `already_active` | debug log only |
| | current scope has a workflow, `force: true` | `ok` | emit `$workflow_cancel` with `reason: 'superseded'`, then start the new |
| | none | `ok` | set `scope.workflow`, emit `$workflow_start` |
| `endWorkflow(name?)` | no workflow on the current scope | `not_active` | none |
| | `name` given and ≠ active name | `name_mismatch` | none |
| | otherwise | `ok` | emit `$workflow_end` with `duration_ms`, clear `scope.workflow` |
| `cancelWorkflow(name?, {reason})` | no workflow on the current scope | `not_active` | none |
| | `name` given and ≠ active name | `name_mismatch` | none |
| | otherwise | `ok` | emit `$workflow_cancel` with `duration_ms` + `reason` (default `'user'`, capped at 120), clear it |

- [ ] **Step 1: Write the failing tests**

Create `sdks/node/test/workflow.test.ts`, using the fake-transport setup from `sdks/node/test/index.test.ts`:

```ts
it('start emits $workflow_start stamped with the new workflow', () => { /* ... */ });
it('stamps track, captureException, captureMessage and trackTransaction', () => { /* ... */ });
it('omits both keys when no workflow is active', () => { /* ... */ });
it('start while active returns already_active and emits nothing', () => { /* ... */ });
it('force cancels with reason superseded then starts the new one', () => { /* ... */ });
it('end emits $workflow_end with duration_ms and clears the scope field', () => { /* ... */ });
it('end with a mismatched name returns name_mismatch and is a no-op', () => { /* ... */ });
it('end with none active returns not_active', () => { /* ... */ });
it('cancel defaults reason to user and caps a long reason at 120 chars', () => { /* ... */ });
it('rejects empty and over-long names', () => { /* ... */ });

it('does NOT leak a workflow across concurrent async contexts', async () => {
  const seen: Record<string, string | undefined> = {};
  await Promise.all([
    withScope(async () => {
      startWorkflow('a');
      await new Promise((r) => setTimeout(r, 10));
      track('from_a');
      seen.a = lastItem().workflow_name;
    }),
    withScope(async () => {
      startWorkflow('b');
      track('from_b');
      seen.b = lastItem().workflow_name;
    }),
  ]);
  expect(seen.a).toBe('a');
  expect(seen.b).toBe('b');
});

it('a workflow started inside withScope does not survive it', async () => {
  await withScope(async () => { startWorkflow('inner'); });
  expect(getWorkflow()).toBeNull();
});
```

Write real assertions in place of every `/* ... */`. The last two tests are the point of this task — do not skip them.

- [ ] **Step 2: Run to verify they fail**

```bash
cd sdks/node && npx vitest run test/workflow.test.ts
```

Expected: FAIL — `startWorkflow is not exported`.

- [ ] **Step 3: Write `src/workflow.ts`**

Types and pure helpers only — no state:

```ts
export const WORKFLOW_NAME_MAX = 120;
export const WORKFLOW_REASON_MAX = 120;

export function normalizeWorkflowName(name: unknown): string | null {
  if (typeof name !== 'string') return null;
  const trimmed = name.trim();
  if (trimmed.length === 0 || trimmed.length > WORKFLOW_NAME_MAX) return null;
  return trimmed;
}

export function normalizeReason(reason: unknown): string {
  if (typeof reason !== 'string' || reason.trim().length === 0) return 'user';
  return reason.trim().slice(0, WORKFLOW_REASON_MAX);
}
```

- [ ] **Step 4: Add the scope field**

In `src/scope.ts`, add `workflow: ActiveWorkflow | null` to the `Scope` class (initialised `null`), include it in whatever clone/copy the scope-push path performs, and in `applyToEvent` set the two wire fields when it is present:

```ts
  if (this.workflow) {
    item.workflow_id = this.workflow.workflowId;
    item.workflow_name = this.workflow.name;
  }
```

Leaving them unset otherwise — do not assign `undefined`, and do not assign `null` (the Rust side omits absent fields and the golden fixtures assert exact JSON).

- [ ] **Step 5: Add the types**

In `src/types.ts`, add `WorkflowStatus`, `WorkflowResult`, `ActiveWorkflow`, and `workflow_id?: string; workflow_name?: string;` to `ErrorItem`, `EventItem`, `TransactionItem`. `index.ts` already does `export type * from './types.js'`, so they become public automatically.

- [ ] **Step 6: Implement the client methods**

Add `startWorkflow` / `endWorkflow` / `cancelWorkflow` / `getWorkflow` to `SauronClient`, operating on `getCurrentScope().workflow`. The close path emits the lifecycle event **before** clearing the field so the event is stamped. Emit via the client's own `track()` so scope/tags apply. Wrap each body in try/catch routed to the debug log; never throw.

Then add the thin delegating functions to `src/index.ts` next to the other `activeClient?.x(...)` wrappers, returning `{ status: 'disabled' }` when there is no active client.

- [ ] **Step 7: Bump the version in both places**

`sdks/node/package.json` → `1.3.0`, `SDK_VERSION` in `src/transport.ts` → `1.3.0`, and update the assertions in `test/envelope.test.ts` and `test/transport.test.ts`.

- [ ] **Step 8: Run the suite**

```bash
cd sdks/node && npm test && npx tsc --noEmit
```

Expected: PASS, including the four unchanged golden fixtures.

---
### Task 9: Python SDK (`sdks/python`) — scope-bound

**Files:**
- Create: `sdks/python/sauron/_workflow.py` (dataclasses + normalisation helpers, no module state)
- Modify: `sdks/python/sauron/_scope.py` (`workflow` field on `Scope`, cloned on push, applied in `apply_to_event`)
- Modify: `sdks/python/sauron/_client.py` (the three methods; stamp `track`, `capture_exception`, `capture_message`, `track_transaction`)
- Modify: `sdks/python/sauron/__init__.py` (module-level delegating functions + `__all__`)
- Modify: `sdks/python/pyproject.toml` + `SDK_VERSION` in `sauron/_client.py` → `1.3.0`
- Modify: `sdks/python/tests/test_golden.py`, `tests/test_envelope.py` (version assertion; the golden payload itself must not change)
- Test: `sdks/python/tests/test_workflow.py` (new)

**Interfaces:**
- Consumes: the wire contract from Task 2.
- Produces:
  ```python
  class WorkflowStatus(str, Enum):
      OK = "ok"
      ALREADY_ACTIVE = "already_active"
      NOT_ACTIVE = "not_active"
      NAME_MISMATCH = "name_mismatch"
      INVALID_NAME = "invalid_name"
      DISABLED = "disabled"

  @dataclass(frozen=True)
  class WorkflowResult:
      status: WorkflowStatus
      workflow_id: Optional[str] = None

  @dataclass
  class ActiveWorkflow:
      workflow_id: str
      name: str
      started_at: datetime

  def start_workflow(name: str, *, force: bool = False) -> WorkflowResult: ...
  def end_workflow(name: Optional[str] = None) -> WorkflowResult: ...
  def cancel_workflow(name: Optional[str] = None, *, reason: Optional[str] = None) -> WorkflowResult: ...
  def get_workflow() -> Optional[ActiveWorkflow]: ...
  ```
  `WorkflowStatus` subclasses `str` so callers can compare against the literal wire strings.

**CRITICAL — do not use a module global.** `sdks/python` isolates per-request state in a `ContextVar` (`sauron/_scope.py`: `_global`, `_current`, `get_current_scope()`, `reset_scopes()`). The active workflow is a field on `Scope`. A module-level `_current_workflow` would leak across concurrent async requests. This is why this SDK never implemented `set_screen`.

**Read first:** `sauron/_scope.py` in full — how existing fields are declared, cloned, and merged in `apply_to_event`. Also the four item-literal blocks in `sauron/_client.py` where `"screen": None` / `"session_id": None` are written; those are your stamp sites. Python has no shared wire-type module — the item dicts *are* the schema, locked by `tests/test_golden.py`.

**Contract:**

| Call | Precondition | Result | Side effect |
|---|---|---|---|
| `start_workflow(name)` | no initialised client | `DISABLED` | none |
| | name empty after strip, or > 120 chars | `INVALID_NAME` | none |
| | current scope has a workflow, `force=False` | `ALREADY_ACTIVE` | debug log only |
| | current scope has a workflow, `force=True` | `OK` | emit `$workflow_cancel` with `reason="superseded"`, then start the new |
| | none | `OK` | set `scope.workflow`, emit `$workflow_start` |
| `end_workflow(name=None)` | no workflow on the current scope | `NOT_ACTIVE` | none |
| | `name` given and != active name | `NAME_MISMATCH` | none |
| | otherwise | `OK` | emit `$workflow_end` with `duration_ms`, clear it |
| `cancel_workflow(name=None, reason=None)` | no workflow on the current scope | `NOT_ACTIVE` | none |
| | `name` given and != active name | `NAME_MISMATCH` | none |
| | otherwise | `OK` | emit `$workflow_cancel` with `duration_ms` + `reason` (default `"user"`, capped at 120), clear it |

- [ ] **Step 1: Write the failing tests**

Create `sdks/python/tests/test_workflow.py` as a `unittest.TestCase` using the fake transport from `tests/_fake.py` and calling `reset_scopes()` in `setUp`:

```python
class WorkflowTests(unittest.TestCase):
    def setUp(self): ...  # init with the fake transport, reset_scopes()

    def test_start_emits_workflow_start_stamped(self): ...
    def test_stamps_track_capture_exception_capture_message_and_transaction(self): ...
    def test_keys_absent_when_no_workflow(self):
        # assert "workflow_id" not in item and "workflow_name" not in item
        ...
    def test_start_while_active_returns_already_active_and_emits_nothing(self): ...
    def test_force_cancels_with_superseded_then_starts_new(self): ...
    def test_end_emits_workflow_end_with_duration_and_clears(self): ...
    def test_end_with_mismatched_name_is_noop(self): ...
    def test_end_with_none_active_returns_not_active(self): ...
    def test_cancel_defaults_reason_to_user_and_caps_at_120(self): ...
    def test_rejects_empty_and_overlong_names(self): ...

    def test_workflow_does_not_leak_across_concurrent_tasks(self):
        # asyncio.gather two coroutines, each in its own scope, one awaiting a sleep
        # between start_workflow and track; assert each track carried its own name
        ...
```

Fill in every body with real assertions. The concurrency test is the point of this task.

- [ ] **Step 2: Run to verify they fail**

```bash
cd sdks/python && python -m pytest tests/test_workflow.py -q
```

Expected: FAIL — `ImportError: cannot import name 'start_workflow'`.

- [ ] **Step 3: Write `sauron/_workflow.py`**

```python
WORKFLOW_NAME_MAX = 120
WORKFLOW_REASON_MAX = 120


def normalize_workflow_name(name):
    """Return the stripped name, or None when invalid."""
    if not isinstance(name, str):
        return None
    trimmed = name.strip()
    if not trimmed or len(trimmed) > WORKFLOW_NAME_MAX:
        return None
    return trimmed


def normalize_reason(reason):
    if not isinstance(reason, str) or not reason.strip():
        return "user"
    return reason.strip()[:WORKFLOW_REASON_MAX]
```

Plus the `WorkflowStatus`, `WorkflowResult`, and `ActiveWorkflow` definitions from the Interfaces block.

- [ ] **Step 4: Add the scope field**

In `sauron/_scope.py`, add `self.workflow = None` to `Scope.__init__`, include it in the clone performed when a scope is pushed, and in `apply_to_event` set the keys only when present:

```python
        if self.workflow is not None:
            item["workflow_id"] = self.workflow.workflow_id
            item["workflow_name"] = self.workflow.name
```

Do **not** write `None` values — the keys must be absent, because `tests/test_golden.py` asserts the exact payload.

- [ ] **Step 5: Implement the client methods and module functions**

Add `start_workflow` / `end_workflow` / `cancel_workflow` / `get_workflow` to `Client` in `sauron/_client.py`, operating on `get_current_scope().workflow`, emitting lifecycle events through the client's own `track()` **before** clearing the field. Wrap each body in `try/except Exception` routed to `self._log`; never raise.

Add the delegating module-level functions to `sauron/__init__.py` beside `track`/`capture_exception`, returning `WorkflowResult(WorkflowStatus.DISABLED)` when `_client` is None, and add all four names plus the three types to `__all__`.

- [ ] **Step 6: Bump the version in both places**

`sdks/python/pyproject.toml` → `1.3.0` and `SDK_VERSION` in `sauron/_client.py` → `1.3.0`; update the version assertions in `tests/test_golden.py` and any transport test that checks it.

- [ ] **Step 7: Run the suite**

```bash
cd sdks/python && python -m pytest -q
```

Expected: PASS, including `tests/test_envelope.py`'s existing assertion that `session_id` and `screen` are None on a plain item, and the unchanged golden payload.

---
### Task 10: C# SDK (`sdks/csharp`) — scope-bound

**Files:**
- Create: `sdks/csharp/Sauron/Workflow.cs` (`WorkflowStatus`, `WorkflowResult`, `ActiveWorkflow`, normalisation helpers)
- Modify: `sdks/csharp/Sauron/Scope.cs` (`Workflow` property on `Scope`, cloned by `PushScope`, applied in `ApplyToEvent`)
- Modify: `sdks/csharp/Sauron/SauronClient.cs` (the three methods; stamp `Track`, `CaptureExceptionCore`, `CaptureMessage`, `TrackTransaction`)
- Modify: `sdks/csharp/Sauron/Envelope.cs` (`WorkflowId` / `WorkflowName` on `EventItem` + `ErrorItem`) and `Sauron/TransactionItem.cs`
- Modify: `sdks/csharp/Sauron/SauronSdk.cs` (static facade methods)
- Modify: `sdks/csharp/Sauron/Sauron.csproj` `<Version>` + `SauronSdkMeta.Version` in `Sauron/Envelope.cs` → `1.3.0`
- Modify: `sdks/csharp/Sauron.Tests/EnvelopeGoldenTests.cs`, `Sauron.Tests/TransportTests.cs` (version assertions)
- Test: `sdks/csharp/Sauron.Tests/WorkflowTests.cs` (new)

**Interfaces:**
- Consumes: the wire contract from Task 2.
- Produces:
  ```csharp
  public enum WorkflowStatus { Ok, AlreadyActive, NotActive, NameMismatch, InvalidName, Disabled }

  public sealed record WorkflowResult(WorkflowStatus Status, string? WorkflowId = null);
  public sealed record ActiveWorkflow(string WorkflowId, string Name, DateTimeOffset StartedAt);

  // on SauronClient and the static SauronSdk facade:
  WorkflowResult StartWorkflow(string name, bool force = false);
  WorkflowResult EndWorkflow(string? name = null);
  WorkflowResult CancelWorkflow(string? name = null, string? reason = null);
  ActiveWorkflow? GetWorkflow();
  ```

**CRITICAL — do not use a static field.** `sdks/csharp` isolates per-request state in `AsyncLocal<Scope?>` (`Sauron/Scope.cs`: `ScopeManager`, `Current`, `PushScope()`, `ResetForTests()`). The active workflow is a property on `Scope`. A `static ActiveWorkflow?` would leak across concurrent requests. This is why this SDK never implemented `SetScreen`.

**Read first:** `Sauron/Scope.cs` in full, and the `EventItem.Screen` / `ErrorItem.Screen` declarations in `Sauron/Envelope.cs` — note the `[JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]` idiom, which the two new properties must use so they are omitted rather than serialised as `null`. Note also that `Screen` is declared but never assigned anywhere; do not treat it as a working precedent for assignment. Existing tests use `[Collection("SauronScope")]` and call `ScopeManager.ResetForTests()` in the constructor — do the same.

**Contract:**

| Call | Precondition | Result | Side effect |
|---|---|---|---|
| `StartWorkflow(name)` | no initialised client | `Disabled` | none |
| | name empty after trim, or > 120 chars | `InvalidName` | none |
| | current scope has a workflow, `force == false` | `AlreadyActive` | debug log only |
| | current scope has a workflow, `force == true` | `Ok` | emit `$workflow_cancel` with `reason = "superseded"`, then start the new |
| | none | `Ok` | set `Scope.Workflow`, emit `$workflow_start` |
| `EndWorkflow(name)` | no workflow on the current scope | `NotActive` | none |
| | `name` given and != active name | `NameMismatch` | none |
| | otherwise | `Ok` | emit `$workflow_end` with `duration_ms`, clear it |
| `CancelWorkflow(name, reason)` | no workflow on the current scope | `NotActive` | none |
| | `name` given and != active name | `NameMismatch` | none |
| | otherwise | `Ok` | emit `$workflow_cancel` with `duration_ms` + `reason` (default `"user"`, capped at 120), clear it |

Wire strings stay snake_case (`already_active`, `not_active`, `name_mismatch`, `invalid_name`) even though the enum members are PascalCase — the enum is never serialised, so no converter is needed.

- [ ] **Step 1: Write the failing tests**

Create `sdks/csharp/Sauron.Tests/WorkflowTests.cs`, modelled on `Sauron.Tests/TransportTests.cs`'s fake-handler capture setup:

```csharp
[Collection("SauronScope")]
public class WorkflowTests
{
    public WorkflowTests() => ScopeManager.ResetForTests();

    [Fact] public void Start_EmitsWorkflowStart_StampedWithNewWorkflow() { }
    [Fact] public void Stamps_Track_CaptureException_CaptureMessage_And_Transaction() { }
    [Fact] public void Keys_AreOmittedFromJson_WhenNoWorkflowActive() { }
    [Fact] public void Start_WhileActive_ReturnsAlreadyActive_AndEmitsNothing() { }
    [Fact] public void Force_CancelsWithSuperseded_ThenStartsNew() { }
    [Fact] public void End_EmitsWorkflowEnd_WithDurationMs_AndClearsScope() { }
    [Fact] public void End_WithMismatchedName_IsNoOp_ReturnsNameMismatch() { }
    [Fact] public void End_WithNoneActive_ReturnsNotActive() { }
    [Fact] public void Cancel_DefaultsReasonToUser_AndCapsAt120() { }
    [Fact] public void Rejects_EmptyAndOverlongNames() { }
    [Fact] public async Task Workflow_DoesNotLeak_AcrossConcurrentAsyncFlows() { }
}
```

Fill in every body with real assertions. Assert JSON omission by serialising the item with the SDK's own `JsonSerializerOptions` from `Envelope.cs` and checking the property is absent — not by checking for a null value.

- [ ] **Step 2: Run to verify they fail**

```bash
cd sdks/csharp && dotnet test
```

Expected: FAIL to compile — `StartWorkflow` does not exist.

- [ ] **Step 3: Write `Sauron/Workflow.cs`**

```csharp
namespace Sauron;

public enum WorkflowStatus { Ok, AlreadyActive, NotActive, NameMismatch, InvalidName, Disabled }

public sealed record WorkflowResult(WorkflowStatus Status, string? WorkflowId = null);

public sealed record ActiveWorkflow(string WorkflowId, string Name, DateTimeOffset StartedAt);

internal static class WorkflowNames
{
    internal const int NameMax = 120;
    internal const int ReasonMax = 120;

    /// <summary>Returns the trimmed name, or null when invalid.</summary>
    internal static string? Normalize(string? name)
    {
        if (string.IsNullOrWhiteSpace(name)) return null;
        var trimmed = name.Trim();
        return trimmed.Length > NameMax ? null : trimmed;
    }

    internal static string NormalizeReason(string? reason)
    {
        if (string.IsNullOrWhiteSpace(reason)) return "user";
        var trimmed = reason.Trim();
        return trimmed.Length > ReasonMax ? trimmed[..ReasonMax] : trimmed;
    }
}
```

- [ ] **Step 4: Add the wire properties**

In `Sauron/Envelope.cs`, add to both `EventItem` and `ErrorItem` (and to `TransactionItem` in its own file), beside the existing `Screen` property and using the same attribute idiom:

```csharp
    [JsonPropertyName("workflow_id")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? WorkflowId { get; set; }

    [JsonPropertyName("workflow_name")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? WorkflowName { get; set; }
```

Match the actual naming-policy setup in that file — if snake_case is applied globally, drop the redundant `JsonPropertyName`. The `WhenWritingNull` condition is not optional: the golden fixture asserts exact JSON.

- [ ] **Step 5: Add the scope property**

In `Sauron/Scope.cs`, add `public ActiveWorkflow? Workflow { get; set; }` to `Scope`, copy it in whatever clone `PushScope` performs, and in `ApplyToEvent` assign the two item properties when it is non-null.

- [ ] **Step 6: Implement the client methods and static facade**

Add `StartWorkflow` / `EndWorkflow` / `CancelWorkflow` / `GetWorkflow` to `SauronClient`, operating on `ScopeManager.Current?.Workflow` (falling back to `Global`, matching how other scope reads resolve). Emit lifecycle events via the client's own `Track()` **before** clearing the property. Wrap each body in `try/catch (Exception ex)` routed to the private `Log`; never let it escape.

Then add matching static methods to `SauronSdk`, returning `new WorkflowResult(WorkflowStatus.Disabled)` when `_client` is null, taking `_gate` where the neighbouring methods do.

- [ ] **Step 7: Bump the version in both places**

`Sauron/Sauron.csproj` `<Version>1.3.0</Version>` and `SauronSdkMeta.Version` in `Sauron/Envelope.cs`, then update the assertions in `Sauron.Tests/EnvelopeGoldenTests.cs` and `Sauron.Tests/TransportTests.cs`.

- [ ] **Step 8: Run the suite**

```bash
cd sdks/csharp && dotnet build -warnaserror && dotnet test
```

Expected: PASS, including `TransportTests`'s existing assertion that `session_id` and `screen` serialise as JSON null, and the golden payload otherwise unchanged.

---
### Task 11: Dashboard — API client, types, and the Workflows list page

**Files:**
- Create: `dashboard/src/lib/api/workflows.ts`
- Create: `dashboard/src/lib/workflows.ts` (pure row-shaping helpers, so they are unit-testable)
- Create: `dashboard/src/lib/workflows.test.ts`
- Create: `dashboard/src/pages/WorkflowsList.svelte`
- Modify: `dashboard/src/lib/models/index.ts` (the four workflow types)
- Modify: `dashboard/src/routes.ts` (register `/workflows`)
- Modify: `dashboard/src/lib/components/layout/Sidebar.svelte` (Explore-group entry)
- Modify: `dashboard/src/lib/components/ui/Icon.svelte` (register the `workflow` icon)
- Modify: `dashboard/src/lib/api/scope.test.ts` (a case for the new URL)

**Interfaces:**
- Consumes: `GET /v1/apps/{app_id}/workflows` (Task 5).
- Produces:
  ```ts
  // dashboard/src/lib/models/index.ts
  export interface WorkflowRow {
    name: string; started: number; completed: number; cancelled: number;
    abandoned: number; active: number; unique_users: number;
    median_duration_ms: number | null; p95_duration_ms: number | null;
    last_seen: string;
  }
  export type WorkflowStatus = 'active' | 'completed' | 'cancelled' | 'abandoned';
  export interface WorkflowRun {
    workflow_id: string; session_id: string | null; distinct_id: string | null;
    status: WorkflowStatus; started_at: string; ended_at: string | null;
    duration_ms: number | null; events_count: number; errors_count: number;
  }
  export interface WorkflowSpan {
    workflow_id: string; name: string; status: WorkflowStatus;
    started_at: string; ended_at: string | null;
  }
  export interface WorkflowDetail { /* see Task 12 */ }

  // dashboard/src/lib/api/workflows.ts
  export function listWorkflows(appId: string, opts?: ListWorkflowsParams): Promise<WorkflowRow[]>;
  export function getWorkflow(appId: string, name: string, opts?: { since_days?: number }): Promise<WorkflowDetail>;
  export function listWorkflowRuns(appId: string, name: string, opts?: ListWorkflowRunsParams): Promise<WorkflowRun[]>;
  export function listSessionWorkflows(appId: string, sessionId: string): Promise<WorkflowSpan[]>;

  // dashboard/src/lib/workflows.ts
  export function completionRate(row: WorkflowRow): number;         // 0..1, 0 when started === 0
  export function statusTone(status: WorkflowStatus): 'success' | 'neutral' | 'warning' | 'error';
  export function formatDuration(ms: number | null): string;        // '—' when null
  ```

**Read first:** `dashboard/src/lib/api/screens.ts` (the `URLSearchParams` client idiom) and `dashboard/src/pages/ScreensList.svelte` end to end — that page's `loading`/`refreshing`/`error` `$state` + `load()` + `refresh()` + `EmptyState` shape is what this page copies. Note the repeated comment in those files about touching `sessionStore.scopeKey` inside the `$effect`.

**Gotchas:**
- The axios interceptor in `dashboard/src/lib/api/client.ts` auto-injects `environment_id` for any URL matching `/^\/v1\/apps\/[^/]+(?:\/.*)?$/`. `/v1/apps/{id}/workflows` matches, which is what we want — do **not** add it to `APP_CONFIG_SUBPATHS`.
- Workflow names go in the path, so they must be `encodeURIComponent`'d in the client and `decodeURIComponent`'d from the route param.
- `npm run check` enforces `noUnusedLocals` / `noUnusedParameters` — an unused import fails the build.
- There is no jsdom and no `@testing-library/svelte`; component tests are not possible. That is why `completionRate` / `statusTone` / `formatDuration` live in a `.ts` module.

- [ ] **Step 1: Write the failing tests**

Create `dashboard/src/lib/workflows.test.ts`:

```ts
import { describe, it, expect } from 'vitest';
import { completionRate, statusTone, formatDuration } from './workflows';

describe('completionRate', () => {
  it('is completed over started', () => {
    expect(completionRate({ started: 4, completed: 3 } as never)).toBeCloseTo(0.75);
  });
  it('is 0 rather than NaN when nothing started', () => {
    expect(completionRate({ started: 0, completed: 0 } as never)).toBe(0);
  });
});

describe('statusTone', () => {
  it('maps every status to a Badge tone', () => {
    expect(statusTone('completed')).toBe('success');
    expect(statusTone('active')).toBe('neutral');
    expect(statusTone('cancelled')).toBe('warning');
    expect(statusTone('abandoned')).toBe('error');
  });
});

describe('formatDuration', () => {
  it('renders an em dash for null', () => { expect(formatDuration(null)).toBe('—'); });
  it('renders sub-second in ms', () => { expect(formatDuration(850)).toBe('850ms'); });
  it('renders seconds with one decimal', () => { expect(formatDuration(2500)).toBe('2.5s'); });
  it('renders minutes and seconds', () => { expect(formatDuration(95000)).toBe('1m 35s'); });
});
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd dashboard && npx vitest run src/lib/workflows.test.ts
```

Expected: FAIL — cannot resolve `./workflows`.

- [ ] **Step 3: Write `src/lib/workflows.ts`**

```ts
import type { WorkflowRow, WorkflowStatus } from './models/index.js';

export function completionRate(row: WorkflowRow): number {
  return row.started === 0 ? 0 : row.completed / row.started;
}

export function statusTone(status: WorkflowStatus): 'success' | 'neutral' | 'warning' | 'error' {
  switch (status) {
    case 'completed': return 'success';
    case 'active': return 'neutral';
    case 'cancelled': return 'warning';
    case 'abandoned': return 'error';
  }
}

export function formatDuration(ms: number | null): string {
  if (ms === null) return '—';
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const totalSeconds = Math.round(ms / 1000);
  return `${Math.floor(totalSeconds / 60)}m ${totalSeconds % 60}s`;
}
```

Match the project's import-extension convention — if sibling modules import without `.js`, do the same.

- [ ] **Step 4: Run to verify they pass**

```bash
cd dashboard && npx vitest run src/lib/workflows.test.ts
```

Expected: PASS.

- [ ] **Step 5: Add the model types**

Add `WorkflowRow`, `WorkflowStatus`, `WorkflowRun`, `WorkflowSpan` to `dashboard/src/lib/models/index.ts`, near the existing `ScreenRow` / `Session` declarations. (`WorkflowDetail` arrives in Task 12.)

- [ ] **Step 6: Write `src/lib/api/workflows.ts`**

Follow `screens.ts`'s hand-built `URLSearchParams` idiom:

```ts
import { api } from './client';
import type { WorkflowRow, WorkflowRun, WorkflowSpan } from '../models/index';

export interface ListWorkflowsParams {
  since_days?: number;
  search?: string;
  limit?: number;
  offset?: number;
}

export async function listWorkflows(appId: string, opts: ListWorkflowsParams = {}): Promise<WorkflowRow[]> {
  const p = new URLSearchParams();
  if (opts.since_days !== undefined) p.set('since_days', String(opts.since_days));
  if (opts.search) p.set('search', opts.search);
  if (opts.limit !== undefined) p.set('limit', String(opts.limit));
  if (opts.offset !== undefined) p.set('offset', String(opts.offset));
  const { data } = await api.get<WorkflowRow[]>(`/v1/apps/${appId}/workflows?${p.toString()}`);
  return data;
}
```

Add `listWorkflowRuns` (path `/v1/apps/${appId}/workflows/${encodeURIComponent(name)}/runs`, params `since_days`, `status`, `limit`, `offset`) and `listSessionWorkflows` (path `/v1/apps/${appId}/sessions/${encodeURIComponent(sessionId)}/workflows`). `getWorkflow` lands in Task 12.

- [ ] **Step 7: Add a scope test case**

In `dashboard/src/lib/api/scope.test.ts`, add a case asserting `/v1/apps/abc/workflows` **does** receive an injected `environment_id` (it must not be treated as an app-config subpath).

- [ ] **Step 8: Register the icon and the nav entry**

In `dashboard/src/lib/components/ui/Icon.svelte`, add `import Workflow from '@lucide/svelte/icons/workflow';` alongside the other icon imports and `'workflow': Workflow` to `iconRegistry`. `IconName` derives from the registry keys, so nothing else needs updating.

In `dashboard/src/lib/components/layout/Sidebar.svelte`, add to the Explore group, next to the Screens entry:

```ts
{ href: '#/workflows', label: 'Workflows', icon: 'workflow', match: (p) => p.startsWith('/workflows') },
```

- [ ] **Step 9: Write `src/pages/WorkflowsList.svelte`**

Structure, mirroring `ScreensList.svelte`:

- `<AppShell requireApp>` wrapper, `<h1 class="page-title">Workflows</h1>`.
- `$state`: `rows: WorkflowRow[]`, `loading`, `refreshing`, `error`, `sinceDays = 30`, `search = ''`, `offset = 0`; `limit` fixed at 50.
- An `$effect` that reads `sessionStore.currentAppId`, touches `sessionStore.scopeKey`, and calls `load()`. Debounce `search` by 300ms into an `appliedSearch` before it feeds `load()`, matching the Issues page.
- A `StatTiles` row of totals summed across rows: Started, Completed, Completion rate, Abandoned.
- A `Card padding="none"` containing a `DataTable` with columns: Workflow, Started, Completed, Cancelled, Abandoned, Completion rate, Median, p95, Users, Last seen. Each `<tr class="clickable">` navigates to `#/workflows/{encodeURIComponent(row.name)}`.
- `SearchInput` + `DateRange` above the table, `Pagination` below.
- `EmptyState` with `icon="workflow"` for both the error branch and the no-rows branch. The empty copy must make clear this is opt-in — e.g. title "No workflows yet", description "Call startWorkflow() in your app to group events into named flows."
- `RefreshButton` wired to `refresh()`.

Render `completionRate(row)` as a percentage and durations through `formatDuration`.

- [ ] **Step 10: Register the route**

In `dashboard/src/routes.ts`, import the page next to the other Explore imports and add `'/workflows': guarded(WorkflowsList),` in the Explore block.

- [ ] **Step 11: Verify**

```bash
cd dashboard && npm run check && npm test
```

Expected: both clean.

Then start the dev server with the preview tooling, navigate to `#/workflows`, and confirm with a snapshot that the page renders — the empty state with an app that has no workflow data, and populated rows after seeding some. Check the console and network panels for errors, and confirm the request carried `environment_id`.

---
### Task 12: Dashboard — Workflow detail page

**Files:**
- Create: `dashboard/src/pages/WorkflowDetail.svelte`
- Modify: `dashboard/src/lib/models/index.ts` (`WorkflowDetail`, `WorkflowIssueRow`)
- Modify: `dashboard/src/lib/api/workflows.ts` (add `getWorkflow`)
- Modify: `dashboard/src/lib/workflows.ts` + `workflows.test.ts` (add `outcomeSteps`)
- Modify: `dashboard/src/routes.ts` (register `/workflows/:name`)

**Interfaces:**
- Consumes: `GET /v1/apps/{app_id}/workflows/{name}` and `.../runs` (Task 5); `listWorkflowRuns`, `formatDuration`, `statusTone` (Task 11).
- Produces:
  ```ts
  export interface WorkflowIssueRow { issue_id: string; title: string; count: number }
  export interface WorkflowDetail {
    name: string;
    started: number; completed: number; cancelled: number;
    abandoned: number; active: number; unique_users: number;
    median_duration_ms: number | null; p95_duration_ms: number | null;
    duration_buckets: { bucket: string; count: number }[];
    top_events: { name: string; count: number }[];
    top_issues: WorkflowIssueRow[];
  }
  export function getWorkflow(appId: string, name: string, opts?: { since_days?: number }): Promise<WorkflowDetail>;
  export function outcomeSteps(d: WorkflowDetail): { label: string; count: number }[];
  ```

**Read first:** `dashboard/src/pages/ScreenDetail.svelte` (route-param decoding via `$props()` + `$derived`, the `StatTile` row, the load/error shape) and the props of `dashboard/src/lib/components/FunnelChart.svelte`, `DurationHistogram.svelte`, and `BarList.svelte` — there is no charting library, so these hand-rolled components are the only option. Read each one's prop signature before using it and shape the data to fit rather than changing the component.

- [ ] **Step 1: Write the failing test**

Add to `dashboard/src/lib/workflows.test.ts`:

```ts
import { outcomeSteps } from './workflows';

describe('outcomeSteps', () => {
  it('renders started as the first step then each terminal outcome', () => {
    const steps = outcomeSteps({
      started: 10, completed: 6, cancelled: 2, abandoned: 1, active: 1,
    } as never);
    expect(steps).toEqual([
      { label: 'Started', count: 10 },
      { label: 'Completed', count: 6 },
      { label: 'Cancelled', count: 2 },
      { label: 'Abandoned', count: 1 },
      { label: 'Still active', count: 1 },
    ]);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

```bash
cd dashboard && npx vitest run src/lib/workflows.test.ts
```

Expected: FAIL — `outcomeSteps` is not exported.

- [ ] **Step 3: Implement `outcomeSteps`**

```ts
export function outcomeSteps(d: WorkflowDetail): { label: string; count: number }[] {
  return [
    { label: 'Started', count: d.started },
    { label: 'Completed', count: d.completed },
    { label: 'Cancelled', count: d.cancelled },
    { label: 'Abandoned', count: d.abandoned },
    { label: 'Still active', count: d.active },
  ];
}
```

- [ ] **Step 4: Run to verify it passes**

```bash
cd dashboard && npx vitest run src/lib/workflows.test.ts
```

Expected: PASS.

- [ ] **Step 5: Add the types and the API function**

Add `WorkflowIssueRow` and `WorkflowDetail` to `dashboard/src/lib/models/index.ts`, and to `dashboard/src/lib/api/workflows.ts`:

```ts
export async function getWorkflow(
  appId: string, name: string, opts: { since_days?: number } = {},
): Promise<WorkflowDetail> {
  const p = new URLSearchParams();
  if (opts.since_days !== undefined) p.set('since_days', String(opts.since_days));
  const { data } = await api.get<WorkflowDetail>(
    `/v1/apps/${appId}/workflows/${encodeURIComponent(name)}?${p.toString()}`,
  );
  return data;
}
```

- [ ] **Step 6: Write `src/pages/WorkflowDetail.svelte`**

- `interface Props { params?: { name?: string } }` + `$props()`, then `const name = $derived(decodeURIComponent(params?.name ?? ''))`.
- `$state`: `detail: WorkflowDetail | null`, `runs: WorkflowRun[]`, `runStatus: WorkflowStatus | 'all' = 'all'`, `runOffset = 0`, plus `loading`/`refreshing`/`error`/`sinceDays`.
- One `$effect` reading `sessionStore.currentAppId`, `name`, `sinceDays`, and touching `sessionStore.scopeKey`, loading detail and runs in parallel via `Promise.all`.
- Layout:
  1. `<h1 class="page-title">{name}</h1>` with a back link to `#/workflows` and a `DateRange`.
  2. `StatTiles`: Started, Completion rate, Median duration, p95 duration, Unique users, Abandoned.
  3. `<Card title="Outcomes">` → `FunnelChart` fed by `outcomeSteps(detail)`.
  4. `<Card title="Duration">` → `DurationHistogram` fed by `detail.duration_buckets`.
  5. `<Card title="Events in this workflow">` → `BarList` fed by `detail.top_events`.
  6. `<Card title="Issues in this workflow" padding="none">` → `DataTable` of `top_issues`, each row linking to the existing issue detail route (copy the href form used by the Issues page).
  7. `<Card title="Recent runs" padding="none">` → a native `<select>` (styled globally; there is no `Select` component) bound to `runStatus` filtering the runs request, then a `DataTable` with columns Status (`Badge` toned via `statusTone`), Started, Duration (`formatDuration`), Events, Errors, User, Session. Rows with a `session_id` are `clickable` and navigate to `#/sessions/{encodeURIComponent(run.session_id)}`; rows without one (server SDKs) render the session cell as `—` and are not clickable. Add `Pagination` bound to `runOffset`.
- `EmptyState` for the error branch and for a workflow name with no data.

- [ ] **Step 7: Register the route**

In `dashboard/src/routes.ts`, add `'/workflows/:name': guarded(WorkflowDetail),` immediately after the `'/workflows'` entry. Order matters in `svelte-spa-router` — the more specific pattern must not be shadowed; verify by navigating to both.

- [ ] **Step 8: Verify**

```bash
cd dashboard && npm run check && npm test
```

Expected: clean.

Then drive it in the preview: navigate to `#/workflows`, click a row, confirm the detail page renders all seven blocks with real seeded data, change the runs status filter and confirm the network request carries `status=`, and click through a run to the session timeline. Check console + network for errors, and screenshot the finished page.

---
### Task 13: Dashboard — `workflow` filter chip on Issues and Events

**Files:**
- Modify: `dashboard/src/lib/components/filters/filters.ts` (`ISSUE_FIELDS`, `EVENT_FIELDS`)
- Modify: `dashboard/src/lib/components/filters/filters.test.ts`

**Interfaces:**
- Consumes: the server-side `workflow` filter field (Task 5).
- Produces: a `workflow` entry in both `ISSUE_FIELDS` and `EVENT_FIELDS`, flowing through the existing `encodeFilters` / `parseFilters` codec with no new mechanism.

**Read first:** `dashboard/src/lib/components/filters/filters.ts` in full — the `FieldDef` shape, `Op` union, `encodeFilters`, `parseFilters`, and the existing field registries. Also skim `FilterBar.svelte` to confirm a `type: 'string'` field needs no new UI. This task adds **data**, not code paths; if you find yourself editing `FilterBar.svelte`, stop and re-read.

- [ ] **Step 1: Write the failing tests**

Add to `dashboard/src/lib/components/filters/filters.test.ts`:

```ts
it('round-trips a workflow filter on issues', () => {
  const encoded = encodeFilters([{ field: 'workflow', op: 'eq', value: 'checkout' }]);
  expect(encoded).toEqual(['workflow:eq:checkout']);
  expect(parseFilters(encoded, ISSUE_FIELDS)).toEqual([
    { field: 'workflow', op: 'eq', value: 'checkout' },
  ]);
});

it('round-trips a workflow filter on events, including a name needing escaping', () => {
  const encoded = encodeFilters([{ field: 'workflow', op: 'contains', value: 'check out/50%' }]);
  expect(parseFilters(encoded, EVENT_FIELDS)).toEqual([
    { field: 'workflow', op: 'contains', value: 'check out/50%' },
  ]);
});

it('rejects an operator the workflow field does not support', () => {
  expect(parseFilters(['workflow:gt:checkout'], ISSUE_FIELDS)).toEqual([]);
});
```

If `FieldDef` does not currently carry a per-field operator allow-list, drop the third test rather than inventing one — check the type first.

- [ ] **Step 2: Run to verify they fail**

```bash
cd dashboard && npx vitest run src/lib/components/filters/filters.test.ts
```

Expected: FAIL — `parseFilters` drops the unknown `workflow` field, returning `[]`.

- [ ] **Step 3: Register the field**

Add to both `ISSUE_FIELDS` and `EVENT_FIELDS`, matching the exact object shape of the neighbouring entries:

```ts
{ field: 'workflow', label: 'Workflow', type: 'string' },
```

- [ ] **Step 4: Run to verify they pass**

```bash
cd dashboard && npm test && npm run check
```

Expected: PASS and clean.

- [ ] **Step 5: Verify end to end in the preview**

Navigate to `#/issues`, add a `Workflow` chip via the filter bar, and confirm: the URL gains `?filter=workflow:eq:checkout`, the network request carries the same `filter` param, the result set narrows, and a page reload rehydrates the chip from the URL. Repeat on `#/events`. Screenshot the filtered Issues page.

---

### Task 14: Dashboard — workflow lane on the session timeline

**Files:**
- Modify: `dashboard/src/lib/models/index.ts` (add the `workflow` arm to `TimelineItem`)
- Modify: `dashboard/src/pages/SessionDetail.svelte` (fetch spans, merge into the timeline)
- Modify: `dashboard/src/lib/components/Timeline.svelte` (four dispatch helpers + badges + styles)

**Interfaces:**
- Consumes: `listSessionWorkflows` (Task 11), `statusTone` and `formatDuration` (Task 11).
- Produces: `TimelineItem` gains
  ```ts
  | { kind: 'workflow'; at: string; edge: 'start' | 'end'; workflow: WorkflowSpan }
  ```
  A span becomes **two** timeline items — one at `started_at` with `edge: 'start'`, and, when `ended_at` is set, one at `ended_at` with `edge: 'end'` — so the existing chronological merge places them correctly around the items they contain, with no changes to the sort.

**Read first:** `dashboard/src/lib/components/Timeline.svelte` in full. Four `switch (item.kind)` helpers must each gain a `workflow` arm — `icon()`, `tone()`, `title()`, `payload()` — plus `screenOf()`, plus the inline per-variant badge block, plus a `.kind-workflow` / `.node.workflow` style pair. Missing any one of them produces a silently blank timeline node. Also read `SessionDetail.svelte`'s existing merge of events + errors + transactions into `TimelineItem[]`.

- [ ] **Step 1: Extend the union and the fetch**

Add the `workflow` arm to `TimelineItem` in `dashboard/src/lib/models/index.ts`.

In `SessionDetail.svelte`, call `listSessionWorkflows(appId, sessionId)` alongside the existing detail fetch (in the same `Promise.all`), then expand each span into its one or two items and merge them into the array the timeline receives, keeping the existing chronological sort:

```ts
const workflowItems = spans.flatMap((workflow) => {
  const items: TimelineItem[] = [{ kind: 'workflow', at: workflow.started_at, edge: 'start', workflow }];
  if (workflow.ended_at) {
    items.push({ kind: 'workflow', at: workflow.ended_at, edge: 'end', workflow });
  }
  return items;
});
```

A failure to load spans must not blank the timeline — if the spans request rejects, log it and render the timeline without the lane.

- [ ] **Step 2: Add the five dispatch arms in `Timeline.svelte`**

```ts
// icon()
case 'workflow': return item.edge === 'start' ? 'workflow' : 'check';
// tone()
case 'workflow':
  if (item.edge === 'start') return 'workflow';
  return item.workflow.status === 'completed' ? 'success' : 'warning';
// title()
case 'workflow':
  return item.edge === 'start'
    ? `Workflow started: ${item.workflow.name}`
    : `Workflow ${item.workflow.status}: ${item.workflow.name}`;
// payload()
case 'workflow': return item.workflow;
// screenOf()
case 'workflow': return null;
```

Use icon names that exist in the registry (`workflow` was added in Task 11; verify `check` exists or substitute one that does). If `tone()`'s return type is a fixed union, add `'workflow'` to it.

- [ ] **Step 3: Add the badge and styles**

Extend the inline per-variant badge chain in the markup with a `workflow` arm rendering a `Badge` toned via the workflow's status, and, on the `end` edge, the duration via `formatDuration`. Add `.kind-workflow` and `.node.workflow` rules beside the existing `.kind-error` / `.node.event` rules, styled so the two edges read as a bracket around the items between them — a distinct accent colour and a heavier rail segment, not a new layout mechanism.

- [ ] **Step 4: Verify**

```bash
cd dashboard && npm run check && npm test
```

Expected: clean. `noUnusedLocals` will catch a helper arm you added an import for but never used.

- [ ] **Step 5: Verify visually in the preview**

Seed a session containing `$workflow_start`, a few events and one error, then `$workflow_end`. Open `#/sessions/{id}` and confirm: both workflow nodes appear in the right chronological positions, the contained items sit between them, the end node shows the duration and a `completed` badge, and expanding a workflow node shows the span JSON. Then check a session with an abandoned workflow (start, no end) renders just the start node without breaking the timeline. Screenshot both.

---
### Task 15: Documentation, parity matrix, and final end-to-end verification

**Files:**
- Modify: `wiki/Capabilities.md` (workflow row; fix the stale `v0.3.0` claims at the top and in the versioning section)
- Modify: `wiki/Ingest-Wire-Contract.md` (the two stamped fields + the three reserved event names)
- Modify: `wiki/Browser-SDK.md`, `wiki/Flutter-SDK.md`, `wiki/Node-SDK.md`, `wiki/Python-SDK.md`, `wiki/CSharp-SDK.md` (API reference entries + one usage example each)
- Create: `wiki/Workflows.md` (the concept page)
- Modify: `sdks/PUBLISHING.md` (version table → js/node/python/csharp `1.3.0`, flutter `1.4.0`)

**Interfaces:**
- Consumes: everything from Tasks 1–14. Nothing consumes this task.

- [ ] **Step 1: Write `wiki/Workflows.md`**

Cover, in this order: what a workflow is (a named, explicitly-bounded span inside one session); that it is entirely optional; the three calls and the full status enum with the meaning of each value; the `force` semantics and the `superseded` cancel reason; the name guard on end/cancel; that only one can be active at a time; how abandonment is derived (30 minutes of inactivity, reported not stored); the three reserved event names and their properties; the two stamped fields; and the fact that server SDKs bind the workflow to their request scope and therefore carry no session id. Link to the per-SDK pages and to the wire contract page.

- [ ] **Step 2: Add the parity row**

In `wiki/Capabilities.md`, add a `workflows` row to the capability matrix marking all five SDKs supported, with a footnote noting that server SDKs are scope-bound and session-less. Correct the two stale `v0.3.0` references to the real current versions.

- [ ] **Step 3: Document the wire contract**

In `wiki/Ingest-Wire-Contract.md`, add `workflow_id` and `workflow_name` to the field tables for the error, event, and transaction items — stating explicitly that both are optional and **omitted** (not null) when unused — and document the three reserved `$workflow_*` event names with their property lists.

- [ ] **Step 4: Add the per-SDK reference entries**

For each of the five SDK pages, add the four functions to the API reference in that page's existing table/heading style, with a short usage example in that language:

```ts
const wf = Sauron.startWorkflow('checkout');
if (wf.status !== 'ok') { /* already in a flow */ }
// ... user goes through the flow, events are attributed automatically ...
Sauron.endWorkflow('checkout');
```

Use idiomatic equivalents for Dart, Python, and C#. Include the `force` case and the status check in at least the Browser and Flutter examples.

- [ ] **Step 5: Update the publishing table**

In `sdks/PUBLISHING.md`, set the version table to js `1.3.0`, node `1.3.0`, python `1.3.0`, csharp `1.3.0`, flutter `1.4.0`. Verify the "bump in both places" table already lists the correct manifest↔constant pairs for each SDK; if any pair is wrong, fix it.

- [ ] **Step 6: Run every suite**

```bash
cd backend   && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
cd sdks/js   && npm test && npx tsc --noEmit
cd sdks/node && npm test && npx tsc --noEmit
cd sdks/python  && python -m pytest -q
cd sdks/flutter && flutter analyze && flutter test
cd sdks/csharp  && dotnet build -warnaserror && dotnet test
cd dashboard    && npm run check && npm test
```

Expected: every command green. Do not proceed to Step 7 with any failure.

- [ ] **Step 7: End-to-end verification against a live stack**

Run the real path, not just the test suites. Bring up Postgres, Redis, the ingest API, and the pipeline worker, then use the example webapp (or a short script against the JS SDK) to:

1. `startWorkflow('checkout')`, fire three `track` calls and one `captureException`, then `endWorkflow()`.
2. Start a second workflow and leave it open.
3. Start a third and replace it with `force: true`.

Then verify, in order:

- `SELECT name, status, cancel_reason, events_count, errors_count, started_at, ended_at FROM workflows;` shows three rows: one `completed` with `events_count = 5` (3 tracks + start + end) and `errors_count = 1`, one `active`, one `cancelled` with `cancel_reason = 'superseded'` plus its replacement.
- `SELECT name, workflow_name FROM analytics_events WHERE workflow_id IS NOT NULL;` shows every event stamped, including the lifecycle events themselves.
- `GET /v1/apps/{app}/workflows` returns the rollup with the right counts and a completion rate of 1/3.
- The Workflows list page renders those rows; the detail page shows the outcome funnel, the contained events (with the `$workflow_*` events excluded from the top-events list), and the error under top issues.
- An Issues query filtered by `workflow:eq:checkout` returns the seeded error's issue.
- The session timeline for the completed run shows the bracketing start/end nodes.
- Finally, run a `track` call with **no** workflow active and confirm the outgoing envelope JSON contains neither `workflow_id` nor `workflow_name` — the optionality guarantee.

Capture the SQL output and a screenshot of the list and detail pages as the verification record.

- [ ] **Step 8: Report honestly**

Write a short summary of what was verified and what was not. If any step above was skipped or any suite left failing, say so explicitly rather than reporting the feature as done.

---

## Notes for the executing agent

- **Do not commit anything.** No `git commit`, no `git branch`, no `git checkout -b`. Leave everything in the working tree. There is other uncommitted work in this repo (including migration `000029`) — do not stage, stash, or clean it.
- **Run the migration before anything else**, and re-run `sauron-migrate` on any environment you test against. This repo has a known failure mode where new binaries meet an old schema and scatter 500s.
- The three server SDKs have no CI job. Their suites will not be run for you — run them locally at the end of Tasks 8, 9, 10 and again in Task 15.
- If a snippet in this plan disagrees with the code you find, the code wins on style and the spec wins on semantics. Note the divergence in your task report.

