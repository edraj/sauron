# Uptime Monitoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add project-scoped uptime monitoring — a dedicated `sauron-monitor` binary actively probes user-defined HTTP(S)/TCP endpoints on a schedule, records checks, opens/resolves incidents on state change, fires webhooks, and surfaces uptime %, latency, and incidents in the dashboard.

**Architecture:** A new `sauron-monitor` library crate holds all pure decision logic (status matching, HTTP evaluation, the up/down state machine, SSRF classification, webhook payloads). A new `sauron-monitor` binary runs a scheduler loop that atomically claims due monitors from Postgres (`FOR UPDATE SKIP LOCKED`), probes them concurrently, persists results/incidents, and dispatches webhooks. `sauron-api` gains project-scoped CRUD + read endpoints; the Svelte dashboard gains an Uptime list + monitor detail. State lives entirely in Postgres — no Redis, no SDK changes, no ingest wire-contract changes.

**Tech Stack:** Rust (edition 2021), axum 0.8, diesel + diesel-async (deadpool, `postgres_backend`), PostgreSQL, `reqwest` (rustls) for probing, `tokio`; Svelte 5 (runes) + TypeScript + axios dashboard; Docker Compose.

## Global Constraints

Copied verbatim from the spec and the repo's existing conventions — every task implicitly includes these:

- **Rust:** `edition = "2021"`, `rust-version = "1.82"`, `license = "AGPL-3.0-only"` (inherited via `*.workspace = true`).
- **DB access:** diesel-async over `AsyncPgConnection`; typed diesel for CRUD, `diesel::sql_query(...).bind::<SqlType, _>(...)` for aggregates (mirror `repo::create_saved_funnel`). All timestamps `chrono::Utc::now()`.
- **Migrations:** `backend/migrations/YYYY-MM-DD-NNNNNN_name/{up,down}.sql`; this feature uses **`2026-07-14-000009_monitors`** (next after `000008`). `schema.rs` is hand-maintained to match.
- **RBAC:** permission strings live in `sauron-auth::rbac::perm`; `perm::ALL` array length must equal the const count; preset roles are re-synced at API startup by `ensure_preset_roles`.
- **⚠️ No DB/handler integration-test harness exists.** Only **pure unit tests** run in `cargo test`. Everything touching the DB, HTTP handlers, the prober's I/O, or the UI is verified **end-to-end via `docker compose up --build`** in the final task. Do not invent a DB test harness.
- **Compose host ports:** API `10000:8080`, ingest `10001:8081`, dashboard `10002:80`. New `sauron-monitor` service has **no published port** (no HTTP surface).
- **Docker build:** the backend `Dockerfile` selects the binary via a `BIN` build arg (`args: BIN: sauron-<name>`); a new bin needs only a new compose service, no Dockerfile change.
- **Commits:** the user controls when to commit (stop-auto-commit preference). The `Commit` steps below mark logical commit points; when executing, follow the user's direction on whether to actually commit or batch.
- **Anti-flap defaults:** `failure_threshold = 2`, `recovery_threshold = 1`. **Retention:** raw checks kept `MONITOR_CHECK_RETENTION_DAYS` days (default 30). **SSRF:** loopback/private/link-local targets blocked by default (`MONITOR_SSRF_ALLOW_PRIVATE=false`).

---

## File Structure

**New backend crate — `backend/crates/sauron-monitor-core/`** (pure logic + probe I/O):
- `Cargo.toml`
- `src/lib.rs` — module wiring + re-exports
- `src/status.rs` — `status_matches`, `evaluate_http`
- `src/state.rs` — `Status`, `MonitorState`, `ProbeResult`, `TransitionKind`, `Outcome`, `apply`, `status_str`
- `src/ssrf.rs` — `is_blocked_ip`, `guard_target`
- `src/webhook.rs` — `WebhookPayload`
- `src/probe.rs` — `Kind`, `ProbeSpec`, `probe` (dispatches HTTP/TCP; I/O)

**New backend bin — `backend/bins/sauron-monitor/`**:
- `Cargo.toml`
- `src/main.rs` — scheduler loop, persistence, webhook dispatch, retention prune

**Modified backend:**
- `backend/migrations/2026-07-14-000009_monitors/{up,down}.sql` — create
- `backend/crates/sauron-db/src/schema.rs` — add 3 `table!` blocks
- `backend/crates/sauron-db/src/models.rs` — add monitor row + insert structs
- `backend/crates/sauron-db/src/repo.rs` — add monitor repo functions
- `backend/crates/sauron-auth/src/rbac.rs` — add `monitor:read`/`monitor:write`
- `backend/crates/sauron-core/src/config.rs` — add `monitor_*` fields
- `backend/bins/sauron-api/src/routes/monitors.rs` — create
- `backend/bins/sauron-api/src/routes/mod.rs` — add `pub mod monitors;`
- `backend/bins/sauron-api/src/main.rs` — wire routes

**Modified dashboard:**
- `dashboard/src/lib/api/monitors.ts` — create
- `dashboard/src/lib/models/index.ts` — add monitor types
- `dashboard/src/pages/Monitors.svelte` — create (list)
- `dashboard/src/pages/MonitorDetail.svelte` — create (detail)
- `dashboard/src/lib/components/ui/StatusPill.svelte` — create
- `dashboard/src/routes.ts` — add routes
- `dashboard/src/lib/components/layout/Sidebar.svelte` — add nav group

**Modified ops:**
- `docker-compose.yml` — add `sauron-monitor` service
- `.env.example` — add `MONITOR_*` defaults

---

## Task 1: Migration + Diesel schema

**Files:**
- Create: `backend/migrations/2026-07-14-000009_monitors/up.sql`
- Create: `backend/migrations/2026-07-14-000009_monitors/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs` (append 3 `table!` blocks + `allow_tables_to_appear_in_same_query!`)

**Interfaces:**
- Produces: tables `monitors`, `monitor_checks`, `monitor_incidents`; diesel modules `monitors`, `monitor_checks`, `monitor_incidents` in `schema.rs`.

- [ ] **Step 1: Write `up.sql`**

```sql
CREATE TABLE monitors (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id              UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name                    TEXT NOT NULL,
    kind                    TEXT NOT NULL CHECK (kind IN ('http', 'tcp')),
    target                  TEXT NOT NULL,
    method                  TEXT NOT NULL DEFAULT 'GET',
    config                  JSONB NOT NULL DEFAULT '{}'::jsonb,
    interval_seconds        INT  NOT NULL DEFAULT 60,
    timeout_ms              INT  NOT NULL DEFAULT 10000,
    failure_threshold       INT  NOT NULL DEFAULT 2,
    recovery_threshold      INT  NOT NULL DEFAULT 1,
    webhook_url             TEXT,
    enabled                 BOOL NOT NULL DEFAULT TRUE,
    status                  TEXT NOT NULL DEFAULT 'unknown'
                              CHECK (status IN ('unknown', 'up', 'down', 'paused')),
    consecutive_failures    INT  NOT NULL DEFAULT 0,
    consecutive_successes   INT  NOT NULL DEFAULT 0,
    last_checked_at         TIMESTAMPTZ,
    next_check_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_status_changed_at  TIMESTAMPTZ,
    created_by              UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX monitors_due_idx ON monitors (next_check_at) WHERE enabled;
CREATE INDEX monitors_project_idx ON monitors (project_id);

CREATE TABLE monitor_checks (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    monitor_id        UUID NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    checked_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    up                BOOL NOT NULL,
    status_code       INT,
    response_time_ms  INT,
    error             TEXT
);
CREATE INDEX monitor_checks_monitor_time_idx ON monitor_checks (monitor_id, checked_at DESC);

CREATE TABLE monitor_incidents (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    monitor_id   UUID NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at  TIMESTAMPTZ,
    cause        TEXT NOT NULL,
    last_error   TEXT
);
CREATE UNIQUE INDEX monitor_incidents_one_open_idx ON monitor_incidents (monitor_id) WHERE resolved_at IS NULL;
CREATE INDEX monitor_incidents_monitor_idx ON monitor_incidents (monitor_id, started_at DESC);
```

- [ ] **Step 2: Write `down.sql`**

```sql
DROP TABLE IF EXISTS monitor_checks;
DROP TABLE IF EXISTS monitor_incidents;
DROP TABLE IF EXISTS monitors;
```

- [ ] **Step 3: Append `table!` blocks to `schema.rs`**

Add after the existing blocks (keep the file's alphabetical-ish grouping; exact contents):

```rust
diesel::table! {
    monitors (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        kind -> Text,
        target -> Text,
        method -> Text,
        config -> Jsonb,
        interval_seconds -> Int4,
        timeout_ms -> Int4,
        failure_threshold -> Int4,
        recovery_threshold -> Int4,
        webhook_url -> Nullable<Text>,
        enabled -> Bool,
        status -> Text,
        consecutive_failures -> Int4,
        consecutive_successes -> Int4,
        last_checked_at -> Nullable<Timestamptz>,
        next_check_at -> Timestamptz,
        last_status_changed_at -> Nullable<Timestamptz>,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    monitor_checks (id) {
        id -> Uuid,
        monitor_id -> Uuid,
        checked_at -> Timestamptz,
        up -> Bool,
        status_code -> Nullable<Int4>,
        response_time_ms -> Nullable<Int4>,
        error -> Nullable<Text>,
    }
}

diesel::table! {
    monitor_incidents (id) {
        id -> Uuid,
        monitor_id -> Uuid,
        started_at -> Timestamptz,
        resolved_at -> Nullable<Timestamptz>,
        cause -> Text,
        last_error -> Nullable<Text>,
    }
}
```

Then add these tables to the existing `diesel::allow_tables_to_appear_in_same_query!(...)` macro list at the bottom of `schema.rs` (append `monitors, monitor_checks, monitor_incidents` to the existing entries), and add `diesel::joinable!(monitors -> projects (project_id));`, `diesel::joinable!(monitor_checks -> monitors (monitor_id));`, `diesel::joinable!(monitor_incidents -> monitors (monitor_id));` alongside the existing `joinable!` lines.

- [ ] **Step 4: Verify the db crate still compiles**

Run: `cd backend && cargo build -p sauron-db`
Expected: compiles clean (no behavioral test yet — DB behavior is verified in Task 16's compose e2e).

- [ ] **Step 5: Commit**

```bash
git add backend/migrations/2026-07-14-000009_monitors backend/crates/sauron-db/src/schema.rs
git commit -m "feat(monitors): migration + diesel schema for monitors/checks/incidents"
```

---

## Task 2: RBAC permissions (`monitor:read` / `monitor:write`)

**Files:**
- Modify: `backend/crates/sauron-auth/src/rbac.rs`

**Interfaces:**
- Produces: `perm::MONITOR_READ` (`"monitor:read"`), `perm::MONITOR_WRITE` (`"monitor:write"`); both present in `perm::ALL` (now length 19); Viewer has read, Developer/Admin/Owner have read+write.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `rbac.rs`:

```rust
#[test]
fn monitor_perms_are_registered_and_seeded() {
    // Both perms exist in the canonical list.
    assert!(perm::ALL.contains(&perm::MONITOR_READ));
    assert!(perm::ALL.contains(&perm::MONITOR_WRITE));
    // Owner (=ALL) has both.
    assert!(OWNER.permissions.contains(&perm::MONITOR_WRITE));
    // Viewer reads but cannot write.
    assert!(VIEWER.permissions.contains(&perm::MONITOR_READ));
    assert!(!VIEWER.permissions.contains(&perm::MONITOR_WRITE));
    // Developer can write.
    assert!(DEVELOPER.permissions.contains(&perm::MONITOR_WRITE));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test -p sauron-auth monitor_perms_are_registered_and_seeded`
Expected: FAIL to compile — `perm::MONITOR_READ` not found.

- [ ] **Step 3: Add the constants and grow `ALL`**

In `mod perm`, add after `FUNNEL_WRITE`:

```rust
    pub const MONITOR_READ: &str = "monitor:read";
    pub const MONITOR_WRITE: &str = "monitor:write";
```

Change `pub const ALL: [&str; 17]` to `pub const ALL: [&str; 19]` and add `MONITOR_READ, MONITOR_WRITE,` into the array (place them after `FUNNEL_WRITE,`).

- [ ] **Step 4: Add to preset roles**

- `ADMIN.permissions`: add `perm::MONITOR_READ,` and `perm::MONITOR_WRITE,`.
- `DEVELOPER.permissions`: add `perm::MONITOR_READ,` and `perm::MONITOR_WRITE,`.
- `VIEWER.permissions`: add `perm::MONITOR_READ,`.
- `OWNER` already uses `&perm::ALL` — no change.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd backend && cargo test -p sauron-auth`
Expected: PASS (new test + all existing rbac tests).

- [ ] **Step 6: Commit**

```bash
git add backend/crates/sauron-auth/src/rbac.rs
git commit -m "feat(monitors): add monitor:read/monitor:write RBAC perms + preset wiring"
```

---

## Task 3: `sauron-monitor` crate — status matching + HTTP evaluation

**Files:**
- Create: `backend/crates/sauron-monitor-core/Cargo.toml`
- Create: `backend/crates/sauron-monitor-core/src/lib.rs`
- Create: `backend/crates/sauron-monitor-core/src/status.rs`
- Modify: `backend/Cargo.toml` (add `reqwest` to `[workspace.dependencies]` and `sauron-monitor` internal crate path)

**Interfaces:**
- Produces: `status_matches(spec: &str, code: u16) -> bool`; `evaluate_http(status_code: u16, body: &str, expected: &str, assertion: Option<&str>) -> (bool, Option<String>)`.

- [ ] **Step 1: Add workspace dependency entries**

In `backend/Cargo.toml` under `# --- internal crates ---` add:

```toml
sauron-monitor-core = { path = "crates/sauron-monitor-core" }
```

Under `# --- async runtime / http ---` add (rustls avoids a system OpenSSL dep in the Docker build):

```toml
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "gzip"] }
```

- [ ] **Step 2: Create the crate manifest**

`backend/crates/sauron-monitor-core/Cargo.toml`:

```toml
[package]
name = "sauron-monitor-core"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true }
reqwest = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 3: Create `src/lib.rs`**

```rust
//! Uptime-monitor decision logic (pure) plus probe execution (I/O).
//!
//! The pure modules (`status`, `state`, `ssrf`, `webhook`) are unit-tested
//! without a network or database. `probe` performs the actual HTTP/TCP I/O.

pub mod probe;
pub mod ssrf;
pub mod state;
pub mod status;
pub mod webhook;

pub use probe::{probe, Kind, ProbeSpec};
pub use state::{apply, status_str, MonitorState, Outcome, ProbeResult, Status, TransitionKind};
pub use status::{evaluate_http, status_matches};
```

(Modules `state`, `ssrf`, `webhook`, `probe` are created in Tasks 4–7; until then, comment out the not-yet-created `pub mod` / `pub use` lines, or create empty stubs. Simplest: create all four files as empty modules now with `// placeholder` so the crate compiles at each step.)

- [ ] **Step 4: Write the failing test**

`backend/crates/sauron-monitor-core/src/status.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_and_csv_matching() {
        assert!(status_matches("200-399", 200));
        assert!(status_matches("200-399", 301));
        assert!(status_matches("200-399", 399));
        assert!(!status_matches("200-399", 400));
        assert!(!status_matches("200-399", 500));
        assert!(status_matches("200,204", 204));
        assert!(!status_matches("200,204", 201));
        assert!(status_matches("200-299,301", 301));
    }

    #[test]
    fn evaluate_http_status_and_assertion() {
        // status ok, no assertion -> up
        assert_eq!(evaluate_http(200, "hello", "200-399", None), (true, None));
        // status mismatch -> down with "HTTP 503"
        assert_eq!(
            evaluate_http(503, "boom", "200-399", None),
            (false, Some("HTTP 503".to_string()))
        );
        // status ok but assertion missing -> down
        assert_eq!(
            evaluate_http(200, "hello world", "200-399", Some("OK")),
            (false, Some("assertion 'OK' not found".to_string()))
        );
        // status ok and assertion present -> up
        assert_eq!(evaluate_http(200, "all OK here", "200-399", Some("OK")), (true, None));
        // empty assertion is ignored
        assert_eq!(evaluate_http(200, "x", "200-399", Some("")), (true, None));
    }
}
```

- [ ] **Step 5: Run test to verify it fails**

Run: `cd backend && cargo test -p sauron-monitor-core status`
Expected: FAIL to compile — `status_matches` / `evaluate_http` not found.

- [ ] **Step 6: Implement `status.rs`**

Prepend above the test module:

```rust
//! Expected-status parsing and HTTP result evaluation (pure).

/// True if `code` satisfies the `expected` spec: comma-separated parts, each a
/// single code (`204`) or an inclusive range (`200-399`). Unparseable parts are
/// skipped.
pub fn status_matches(expected: &str, code: u16) -> bool {
    for part in expected.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = part.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (lo.trim().parse::<u16>(), hi.trim().parse::<u16>()) {
                if code >= lo && code <= hi {
                    return true;
                }
            }
        } else if let Ok(v) = part.parse::<u16>() {
            if code == v {
                return true;
            }
        }
    }
    false
}

/// Evaluate an HTTP response. Returns `(up, error_reason)`.
pub fn evaluate_http(
    status_code: u16,
    body: &str,
    expected: &str,
    assertion: Option<&str>,
) -> (bool, Option<String>) {
    if !status_matches(expected, status_code) {
        return (false, Some(format!("HTTP {status_code}")));
    }
    if let Some(a) = assertion {
        if !a.is_empty() && !body.contains(a) {
            return (false, Some(format!("assertion '{a}' not found")));
        }
    }
    (true, None)
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cd backend && cargo test -p sauron-monitor-core status`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add backend/Cargo.toml backend/crates/sauron-monitor
git commit -m "feat(monitors): sauron-monitor crate + status/evaluate logic (TDD)"
```

---

## Task 4: State machine (`state.rs`)

**Files:**
- Create/replace: `backend/crates/sauron-monitor-core/src/state.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `enum Status { Unknown, Up, Down, Paused }`
  - `struct ProbeResult { up: bool, status_code: Option<i32>, response_time_ms: Option<i32>, error: Option<String> }`
  - `struct MonitorState { status: Status, consecutive_failures: i32, consecutive_successes: i32, failure_threshold: i32, recovery_threshold: i32 }`
  - `enum TransitionKind { None, WentDown, WentUp }`
  - `struct Outcome { new_status: Status, consecutive_failures: i32, consecutive_successes: i32, transition: TransitionKind }`
  - `fn apply(state: &MonitorState, result: &ProbeResult) -> Outcome`
  - `fn status_str(s: Status) -> &'static str`

- [ ] **Step 1: Write the failing test**

`backend/crates/sauron-monitor-core/src/state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn st(status: Status, cf: i32, cs: i32) -> MonitorState {
        MonitorState { status, consecutive_failures: cf, consecutive_successes: cs,
            failure_threshold: 2, recovery_threshold: 1 }
    }
    fn up() -> ProbeResult { ProbeResult { up: true, status_code: Some(200), response_time_ms: Some(5), error: None } }
    fn down() -> ProbeResult { ProbeResult { up: false, status_code: None, response_time_ms: None, error: Some("timeout".into()) } }

    #[test]
    fn single_failure_does_not_trip_when_threshold_is_two() {
        let o = apply(&st(Status::Up, 0, 5), &down());
        assert_eq!(o.new_status, Status::Up);
        assert_eq!(o.consecutive_failures, 1);
        assert_eq!(o.transition, TransitionKind::None);
    }

    #[test]
    fn second_consecutive_failure_goes_down() {
        let o = apply(&st(Status::Up, 1, 0), &down());
        assert_eq!(o.new_status, Status::Down);
        assert_eq!(o.transition, TransitionKind::WentDown);
    }

    #[test]
    fn recovery_after_one_success() {
        let o = apply(&st(Status::Down, 4, 0), &up());
        assert_eq!(o.new_status, Status::Up);
        assert_eq!(o.transition, TransitionKind::WentUp);
        assert_eq!(o.consecutive_successes, 1);
        assert_eq!(o.consecutive_failures, 0);
    }

    #[test]
    fn unknown_first_success_is_silent() {
        let o = apply(&st(Status::Unknown, 0, 0), &up());
        assert_eq!(o.new_status, Status::Up);
        assert_eq!(o.transition, TransitionKind::None);
    }

    #[test]
    fn unknown_then_two_failures_goes_down() {
        let o1 = apply(&st(Status::Unknown, 0, 0), &down());
        assert_eq!(o1.new_status, Status::Unknown);
        assert_eq!(o1.transition, TransitionKind::None);
        let o2 = apply(&st(Status::Unknown, o1.consecutive_failures, 0), &down());
        assert_eq!(o2.new_status, Status::Down);
        assert_eq!(o2.transition, TransitionKind::WentDown);
    }

    #[test]
    fn flap_suppressed_down_then_up_stays_up() {
        // one failure, then success: never left Up, no incident
        let o1 = apply(&st(Status::Up, 0, 3), &down());
        assert_eq!(o1.new_status, Status::Up);
        let o2 = apply(&st(Status::Up, o1.consecutive_failures, 0), &up());
        assert_eq!(o2.new_status, Status::Up);
        assert_eq!(o2.transition, TransitionKind::None);
    }

    #[test]
    fn status_strings() {
        assert_eq!(status_str(Status::Unknown), "unknown");
        assert_eq!(status_str(Status::Up), "up");
        assert_eq!(status_str(Status::Down), "down");
        assert_eq!(status_str(Status::Paused), "paused");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test -p sauron-monitor-core state`
Expected: FAIL to compile — types/`apply` not found.

- [ ] **Step 3: Implement `state.rs`**

Prepend above the test module:

```rust
//! The up/down state machine (pure). Given the current persisted counters and a
//! fresh probe result, decide the new status and whether a transition fires.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Unknown,
    Up,
    Down,
    Paused,
}

pub fn status_str(s: Status) -> &'static str {
    match s {
        Status::Unknown => "unknown",
        Status::Up => "up",
        Status::Down => "down",
        Status::Paused => "paused",
    }
}

/// The result of one probe (network outcome already evaluated to up/down).
#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub up: bool,
    pub status_code: Option<i32>,
    pub response_time_ms: Option<i32>,
    pub error: Option<String>,
}

/// The monitor's persisted state the decision needs.
#[derive(Clone, Debug)]
pub struct MonitorState {
    pub status: Status,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub failure_threshold: i32,
    pub recovery_threshold: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransitionKind {
    None,
    WentDown,
    WentUp,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub new_status: Status,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub transition: TransitionKind,
}

/// Apply one probe result to the monitor's state.
///
/// - A failure increments the failure counter and resets successes; once it
///   reaches `failure_threshold` and we are not already down, we go **down**
///   (fires from `Up` *and* `Unknown` — a service that starts down should alert).
/// - A success increments the success counter and resets failures; from `Down`
///   once it reaches `recovery_threshold` we go **up** (fires `WentUp`). From
///   `Unknown`, the first qualifying success sets `Up` **silently** (no false
///   "recovered" alert for something that was never known-down).
pub fn apply(state: &MonitorState, result: &ProbeResult) -> Outcome {
    let mut cf = state.consecutive_failures;
    let mut cs = state.consecutive_successes;
    if result.up {
        cs += 1;
        cf = 0;
    } else {
        cf += 1;
        cs = 0;
    }

    let mut new_status = state.status;
    let mut transition = TransitionKind::None;

    if result.up {
        if cs >= state.recovery_threshold {
            match state.status {
                Status::Down => {
                    new_status = Status::Up;
                    transition = TransitionKind::WentUp;
                }
                Status::Unknown => {
                    new_status = Status::Up; // silent bootstrap
                }
                _ => {}
            }
        }
    } else if state.status != Status::Down && cf >= state.failure_threshold {
        new_status = Status::Down;
        transition = TransitionKind::WentDown;
    }

    Outcome {
        new_status,
        consecutive_failures: cf,
        consecutive_successes: cs,
        transition,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test -p sauron-monitor-core state`
Expected: PASS (all 7 tests).

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-monitor-core/src/state.rs
git commit -m "feat(monitors): up/down state machine with anti-flap thresholds (TDD)"
```

---

## Task 5: SSRF classifier (`ssrf.rs`)

**Files:**
- Create/replace: `backend/crates/sauron-monitor-core/src/ssrf.rs`

**Interfaces:**
- Produces:
  - `fn is_blocked_ip(ip: std::net::IpAddr) -> bool`
  - `async fn guard_target(host: &str, allow_private: bool) -> Result<(), String>` (resolves the host and rejects if any resolved IP is blocked)

- [ ] **Step 1: Write the failing test**

`backend/crates/sauron-monitor-core/src/ssrf.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr { s.parse().unwrap() }

    #[test]
    fn blocks_private_and_local_v4() {
        assert!(is_blocked_ip(ip("127.0.0.1")));
        assert!(is_blocked_ip(ip("10.1.2.3")));
        assert!(is_blocked_ip(ip("192.168.0.5")));
        assert!(is_blocked_ip(ip("172.16.9.9")));
        assert!(is_blocked_ip(ip("169.254.169.254"))); // cloud metadata
        assert!(is_blocked_ip(ip("0.0.0.0")));
        assert!(is_blocked_ip(ip("100.64.0.1"))); // CGNAT
    }

    #[test]
    fn allows_public_v4() {
        assert!(!is_blocked_ip(ip("8.8.8.8")));
        assert!(!is_blocked_ip(ip("1.1.1.1")));
        assert!(!is_blocked_ip(ip("93.184.216.34")));
    }

    #[test]
    fn blocks_local_v6_allows_public_v6() {
        assert!(is_blocked_ip(ip("::1")));
        assert!(is_blocked_ip(ip("fc00::1")));   // unique local
        assert!(is_blocked_ip(ip("fe80::1")));   // link local
        assert!(is_blocked_ip(ip("::ffff:127.0.0.1"))); // v4-mapped loopback
        assert!(!is_blocked_ip(ip("2606:4700:4700::1111")));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test -p sauron-monitor-core ssrf`
Expected: FAIL to compile — `is_blocked_ip` not found.

- [ ] **Step 3: Implement `ssrf.rs`**

Prepend above the test module:

```rust
//! SSRF guard: reject probing loopback / private / link-local / metadata
//! targets by default. The classifier is pure and unit-tested; `guard_target`
//! resolves DNS and checks every resolved address (defends against rebinding).

use std::net::IpAddr;

/// True if the address is one we refuse to probe unless explicitly allowed.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || o[0] == 0
                // 100.64.0.0/10 carrier-grade NAT
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique local
                || (seg[0] & 0xfe00) == 0xfc00
                // fe80::/10 link local
                || (seg[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Resolve `host` and fail if any resolved address is blocked (unless
/// `allow_private`). `host` is a bare hostname or IP literal (no port).
pub async fn guard_target(host: &str, allow_private: bool) -> Result<(), String> {
    if allow_private {
        return Ok(());
    }
    // `lookup_host` needs a port; :0 is fine, we only use the IPs.
    let addrs = tokio::net::lookup_host((host, 0u16))
        .await
        .map_err(|e| format!("DNS resolution failed: {e}"))?;
    let mut saw_any = false;
    for addr in addrs {
        saw_any = true;
        if is_blocked_ip(addr.ip()) {
            return Err(format!("target {} resolves to a blocked address", host));
        }
    }
    if !saw_any {
        return Err(format!("target {host} did not resolve"));
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test -p sauron-monitor-core ssrf`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-monitor-core/src/ssrf.rs
git commit -m "feat(monitors): SSRF address classifier + DNS guard (TDD)"
```

---

## Task 6: Webhook payload (`webhook.rs`)

**Files:**
- Create/replace: `backend/crates/sauron-monitor-core/src/webhook.rs`

**Interfaces:**
- Produces: `struct WebhookPayload` (Serialize) with fields `monitor_id, name, project_id, status, previous_status, at, incident_id, cause, target`.

- [ ] **Step 1: Write the failing test**

`backend/crates/sauron-monitor-core/src/webhook.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use uuid::Uuid;

    #[test]
    fn serializes_expected_shape() {
        let p = WebhookPayload {
            monitor_id: Uuid::nil(),
            name: "api",
            project_id: Uuid::nil(),
            status: "down",
            previous_status: "up",
            at: chrono::Utc.timestamp_opt(0, 0).unwrap(),
            incident_id: Some(Uuid::nil()),
            cause: Some("HTTP 503"),
            target: "https://example.com",
        };
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["status"], "down");
        assert_eq!(v["previous_status"], "up");
        assert_eq!(v["cause"], "HTTP 503");
        assert_eq!(v["target"], "https://example.com");
        assert_eq!(v["name"], "api");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd backend && cargo test -p sauron-monitor-core webhook`
Expected: FAIL to compile — `WebhookPayload` not found.

- [ ] **Step 3: Implement `webhook.rs`**

Prepend above the test module:

```rust
//! The JSON body POSTed to a monitor's `webhook_url` on each state change.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct WebhookPayload<'a> {
    pub monitor_id: Uuid,
    pub name: &'a str,
    pub project_id: Uuid,
    pub status: &'a str,
    pub previous_status: &'a str,
    pub at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incident_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<&'a str>,
    pub target: &'a str,
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test -p sauron-monitor-core webhook`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-monitor-core/src/webhook.rs
git commit -m "feat(monitors): webhook payload type (TDD)"
```

---

## Task 7: Probe execution (`probe.rs`, I/O)

**Files:**
- Create/replace: `backend/crates/sauron-monitor-core/src/probe.rs`

**Interfaces:**
- Consumes: `status::evaluate_http`, `ssrf::guard_target`, `state::ProbeResult`.
- Produces:
  - `enum Kind { Http, Tcp }`
  - `struct ProbeSpec { kind, target, method, headers: Vec<(String,String)>, body: Option<String>, expected_status: String, body_assertion: Option<String>, follow_redirects: bool, timeout: std::time::Duration }`
  - `async fn probe(spec: &ProbeSpec, client: &reqwest::Client, allow_private: bool) -> ProbeResult`

> This task is I/O; it has no unit test (per the no-network/no-DB harness constraint). It is compiled here and exercised behaviorally in Task 16's compose e2e. Keep the pure decision (`evaluate_http`) in Task 3 so the untested surface is just the network call.

- [ ] **Step 1: Implement `probe.rs`**

```rust
//! Probe execution. HTTP via reqwest, TCP via a raw connect. Each returns a
//! `ProbeResult`; a failed probe is data (down + reason), never an error.

use std::time::{Duration, Instant};

use crate::ssrf::guard_target;
use crate::state::ProbeResult;
use crate::status::evaluate_http;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Http,
    Tcp,
}

#[derive(Clone, Debug)]
pub struct ProbeSpec {
    pub kind: Kind,
    /// URL for HTTP; `host:port` for TCP.
    pub target: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub expected_status: String,
    pub body_assertion: Option<String>,
    pub follow_redirects: bool,
    pub timeout: Duration,
}

fn down(reason: impl Into<String>, ms: Option<i32>) -> ProbeResult {
    ProbeResult { up: false, status_code: None, response_time_ms: ms, error: Some(reason.into()) }
}

/// Extract the bare host from an HTTP URL or a `host:port` string (for the SSRF guard).
fn host_of(spec: &ProbeSpec) -> Option<String> {
    match spec.kind {
        Kind::Http => reqwest::Url::parse(&spec.target).ok().and_then(|u| u.host_str().map(|s| s.to_string())),
        Kind::Tcp => spec.target.rsplit_once(':').map(|(h, _)| h.to_string()),
    }
}

pub async fn probe(spec: &ProbeSpec, client: &reqwest::Client, allow_private: bool) -> ProbeResult {
    // SSRF guard first.
    if let Some(host) = host_of(spec) {
        if let Err(e) = guard_target(&host, allow_private).await {
            return down(e, None);
        }
    } else {
        return down("invalid target", None);
    }

    match spec.kind {
        Kind::Http => probe_http(spec, client).await,
        Kind::Tcp => probe_tcp(spec).await,
    }
}

async fn probe_http(spec: &ProbeSpec, client: &reqwest::Client) -> ProbeResult {
    let method = reqwest::Method::from_bytes(spec.method.as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let mut req = client.request(method, &spec.target).timeout(spec.timeout);
    for (k, v) in &spec.headers {
        req = req.header(k, v);
    }
    if let Some(b) = &spec.body {
        req = req.body(b.clone());
    }

    let start = Instant::now();
    match req.send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            let ms = start.elapsed().as_millis() as i32;
            let (up, err) = evaluate_http(code, &body, &spec.expected_status, spec.body_assertion.as_deref());
            ProbeResult { up, status_code: Some(code as i32), response_time_ms: Some(ms), error: err }
        }
        Err(e) => {
            let ms = start.elapsed().as_millis() as i32;
            let reason = if e.is_timeout() { "connection timeout".to_string() } else { format!("request failed: {e}") };
            ProbeResult { up: false, status_code: None, response_time_ms: Some(ms), error: Some(reason) }
        }
    }
}

async fn probe_tcp(spec: &ProbeSpec) -> ProbeResult {
    let start = Instant::now();
    match tokio::time::timeout(spec.timeout, tokio::net::TcpStream::connect(&spec.target)).await {
        Ok(Ok(_stream)) => {
            let ms = start.elapsed().as_millis() as i32;
            ProbeResult { up: true, status_code: None, response_time_ms: Some(ms), error: None }
        }
        Ok(Err(e)) => down(format!("TCP connect failed: {e}"), Some(start.elapsed().as_millis() as i32)),
        Err(_) => down("connection timeout", Some(start.elapsed().as_millis() as i32)),
    }
}
```

Note the `follow_redirects` field is honored by how the caller builds the shared `reqwest::Client` (redirect policy is a client-level setting); the bin builds one client with redirects enabled and, for monitors that disable them, builds requests is not enough — for the MVP we keep a single client with `redirect::Policy::limited(10)` and treat `follow_redirects=false` as "expected_status should include 3xx." Document this limitation in the spec's out-of-scope if stricter per-monitor redirect control is needed later.

- [ ] **Step 2: Verify the crate compiles**

Run: `cd backend && cargo build -p sauron-monitor-core && cargo test -p sauron-monitor-core`
Expected: builds; all pure tests (status/state/ssrf/webhook) still pass.

- [ ] **Step 3: Commit**

```bash
git add backend/crates/sauron-monitor-core/src/probe.rs backend/crates/sauron-monitor-core/src/lib.rs
git commit -m "feat(monitors): HTTP/TCP probe execution behind ProbeSpec"
```

---

## Task 8: Config fields

**Files:**
- Modify: `backend/crates/sauron-core/src/config.rs`

**Interfaces:**
- Produces: `Config` fields `monitor_tick_ms: u64`, `monitor_batch: i64`, `monitor_max_concurrency: usize`, `monitor_check_retention_days: i64`, `monitor_min_interval_secs: i64`, `monitor_ssrf_allow_private: bool`.

- [ ] **Step 1: Add fields to the struct**

In `pub struct Config`, add:

```rust
    pub monitor_tick_ms: u64,
    pub monitor_batch: i64,
    pub monitor_max_concurrency: usize,
    pub monitor_check_retention_days: i64,
    pub monitor_min_interval_secs: i64,
    pub monitor_ssrf_allow_private: bool,
```

- [ ] **Step 2: Populate them in `from_env`**

`parse` only handles `FromStr` types; add a small bool parse inline. In the `Ok(Self { ... })` block add:

```rust
            monitor_tick_ms: parse("MONITOR_TICK_MS", 1000),
            monitor_batch: parse("MONITOR_BATCH", 100),
            monitor_max_concurrency: parse("MONITOR_MAX_CONCURRENCY", 50),
            monitor_check_retention_days: parse("MONITOR_CHECK_RETENTION_DAYS", 30),
            monitor_min_interval_secs: parse("MONITOR_MIN_INTERVAL_SECS", 30),
            monitor_ssrf_allow_private: var("MONITOR_SSRF_ALLOW_PRIVATE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
```

- [ ] **Step 3: Verify it compiles**

Run: `cd backend && cargo build -p sauron-core`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add backend/crates/sauron-core/src/config.rs
git commit -m "feat(monitors): MONITOR_* config fields"
```

---

## Task 9: DB models + repo functions

**Files:**
- Modify: `backend/crates/sauron-db/src/models.rs`
- Modify: `backend/crates/sauron-db/src/repo.rs`

**Interfaces:**
- Produces (models): `Monitor` (Queryable/Selectable/Serialize), `NewMonitor` (Insertable), `MonitorCheckRow`, `MonitorIncidentRow`.
- Produces (repo), all `async fn(conn: &mut AsyncPgConnection, ...)`:
  - `list_monitors_for_project(project_id) -> QueryResult<Vec<MonitorListRow>>`
  - `create_monitor(NewMonitor) -> QueryResult<Monitor>`
  - `get_monitor(id) -> QueryResult<Option<Monitor>>`
  - `monitor_project(id) -> QueryResult<Option<Uuid>>`
  - `update_monitor(id, patch) -> QueryResult<Option<Monitor>>`
  - `delete_monitor(id) -> QueryResult<usize>`
  - `claim_due_monitors(batch: i64) -> QueryResult<Vec<Monitor>>`
  - `record_check_and_state(...)`
  - `open_incident(monitor_id, cause, last_error) -> QueryResult<Uuid>`
  - `resolve_incident(monitor_id) -> QueryResult<()>`
  - `uptime_pct(monitor_id, since_hours: i64) -> QueryResult<Option<f64>>`
  - `latency_series(monitor_id, since_hours: i64) -> QueryResult<Vec<CheckPoint>>`
  - `list_incidents(monitor_id, limit: i64) -> QueryResult<Vec<MonitorIncidentRow>>`
  - `prune_checks(older_than_days: i64) -> QueryResult<usize>`

- [ ] **Step 1: Add model structs**

Append to `models.rs` (mirror the existing `Queryable`/`Insertable` style; `config` maps to `serde_json::Value`):

```rust
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = monitors)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Monitor {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub method: String,
    pub config: serde_json::Value,
    pub interval_seconds: i32,
    pub timeout_ms: i32,
    pub failure_threshold: i32,
    pub recovery_threshold: i32,
    pub webhook_url: Option<String>,
    pub enabled: bool,
    pub status: String,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub next_check_at: DateTime<Utc>,
    pub last_status_changed_at: Option<DateTime<Utc>>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = monitors)]
pub struct NewMonitor<'a> {
    pub project_id: Uuid,
    pub name: &'a str,
    pub kind: &'a str,
    pub target: &'a str,
    pub method: &'a str,
    pub config: &'a serde_json::Value,
    pub interval_seconds: i32,
    pub timeout_ms: i32,
    pub failure_threshold: i32,
    pub recovery_threshold: i32,
    pub webhook_url: Option<&'a str>,
    pub created_by: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = monitor_incidents)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct MonitorIncidentRow {
    pub id: Uuid,
    pub monitor_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub cause: String,
    pub last_error: Option<String>,
}
```

- [ ] **Step 2: Add repo query helper structs**

Add near the other `QueryableByName` aggregate rows in `repo.rs` (mirror `SavedFunnelRow`'s `#[diesel(sql_type = ...)]` style; imports `diesel::sql_types::{BigInt, Double, Nullable, Text, Timestamptz, Uuid as SqlUuid, Bool, Integer}`):

```rust
#[derive(QueryableByName, Serialize)]
pub struct MonitorListRow {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Text)]
    pub kind: String,
    #[diesel(sql_type = Text)]
    pub target: String,
    #[diesel(sql_type = Text)]
    pub status: String,
    #[diesel(sql_type = Bool)]
    pub enabled: bool,
    #[diesel(sql_type = Nullable<Integer>)]
    pub last_response_time_ms: Option<i32>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    pub last_checked_at: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<Double>)]
    pub uptime_24h: Option<f64>,
}

#[derive(QueryableByName, Serialize)]
pub struct CheckPoint {
    #[diesel(sql_type = Timestamptz)]
    pub checked_at: DateTime<Utc>,
    #[diesel(sql_type = Bool)]
    pub up: bool,
    #[diesel(sql_type = Nullable<Integer>)]
    pub response_time_ms: Option<i32>,
    #[diesel(sql_type = Nullable<Integer>)]
    pub status_code: Option<i32>,
    #[diesel(sql_type = Nullable<Text>)]
    pub error: Option<String>,
}
```

- [ ] **Step 3: Add typed CRUD repo functions**

Add a `// Monitors` section to `repo.rs`:

```rust
pub async fn create_monitor(
    conn: &mut AsyncPgConnection,
    m: NewMonitor<'_>,
) -> QueryResult<Monitor> {
    diesel::insert_into(monitors::table)
        .values(m)
        .returning(Monitor::as_returning())
        .get_result(conn)
        .await
}

pub async fn get_monitor(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Monitor>> {
    monitors::table.find(id).select(Monitor::as_select()).first(conn).await.optional()
}

pub async fn monitor_project(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Uuid>> {
    monitors::table.find(id).select(monitors::project_id).first(conn).await.optional()
}

pub async fn delete_monitor(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(monitors::table.find(id)).execute(conn).await
}

pub async fn list_incidents(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    limit: i64,
) -> QueryResult<Vec<MonitorIncidentRow>> {
    monitor_incidents::table
        .filter(monitor_incidents::monitor_id.eq(monitor_id))
        .select(MonitorIncidentRow::as_select())
        .order(monitor_incidents::started_at.desc())
        .limit(limit)
        .load(conn)
        .await
}
```

- [ ] **Step 4: Add the list-with-uptime query (raw SQL)**

```rust
pub async fn list_monitors_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<MonitorListRow>> {
    diesel::sql_query(
        "SELECT m.id, m.name, m.kind, m.target, m.status, m.enabled, \
                lc.response_time_ms AS last_response_time_ms, m.last_checked_at, \
                up.pct AS uptime_24h \
         FROM monitors m \
         LEFT JOIN LATERAL ( \
             SELECT response_time_ms FROM monitor_checks c \
             WHERE c.monitor_id = m.id ORDER BY c.checked_at DESC LIMIT 1 \
         ) lc ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT 100.0 * avg(CASE WHEN c.up THEN 1 ELSE 0 END) AS pct \
             FROM monitor_checks c \
             WHERE c.monitor_id = m.id AND c.checked_at >= now() - interval '24 hours' \
         ) up ON TRUE \
         WHERE m.project_id = $1 \
         ORDER BY m.created_at ASC",
    )
    .bind::<SqlUuid, _>(project_id)
    .get_results(conn)
    .await
}
```

- [ ] **Step 5: Add uptime %, latency series, and prune (raw SQL)**

```rust
#[derive(QueryableByName)]
struct PctRow { #[diesel(sql_type = Nullable<Double>)] pct: Option<f64> }

pub async fn uptime_pct(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    since_hours: i64,
) -> QueryResult<Option<f64>> {
    let row: PctRow = diesel::sql_query(
        "SELECT 100.0 * avg(CASE WHEN up THEN 1 ELSE 0 END) AS pct FROM monitor_checks \
         WHERE monitor_id = $1 AND checked_at >= now() - ($2 || ' hours')::interval",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(since_hours.to_string())
    .get_result(conn)
    .await?;
    Ok(row.pct)
}

pub async fn latency_series(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    since_hours: i64,
) -> QueryResult<Vec<CheckPoint>> {
    diesel::sql_query(
        "SELECT checked_at, up, response_time_ms, status_code, error FROM monitor_checks \
         WHERE monitor_id = $1 AND checked_at >= now() - ($2 || ' hours')::interval \
         ORDER BY checked_at ASC",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(since_hours.to_string())
    .get_results(conn)
    .await
}

pub async fn prune_checks(conn: &mut AsyncPgConnection, older_than_days: i64) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM monitor_checks WHERE checked_at < now() - ($1 || ' days')::interval",
    )
    .bind::<Text, _>(older_than_days.to_string())
    .execute(conn)
    .await
}
```

- [ ] **Step 6: Add the prober's claim + persistence functions**

```rust
/// Atomically claim due monitors and push their next_check_at forward so no
/// other prober picks the same rows. Returns the claimed rows to probe.
pub async fn claim_due_monitors(
    conn: &mut AsyncPgConnection,
    batch: i64,
) -> QueryResult<Vec<Monitor>> {
    diesel::sql_query(
        "UPDATE monitors SET next_check_at = now() + make_interval(secs => interval_seconds), \
                last_checked_at = now() \
         WHERE id IN ( \
             SELECT id FROM monitors \
             WHERE enabled AND status <> 'paused' AND next_check_at <= now() \
             ORDER BY next_check_at FOR UPDATE SKIP LOCKED LIMIT $1 \
         ) RETURNING *",
    )
    .bind::<BigInt, _>(batch)
    .get_results(conn)
    .await
}

/// Persist one probe result: insert the check row and update the monitor's
/// counters + status. `new_status` is the state machine's decision.
pub async fn record_check_and_state(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    up: bool,
    status_code: Option<i32>,
    response_time_ms: Option<i32>,
    error: Option<&str>,
    new_status: &str,
    consecutive_failures: i32,
    consecutive_successes: i32,
    status_changed: bool,
) -> QueryResult<()> {
    diesel::sql_query(
        "INSERT INTO monitor_checks (monitor_id, up, status_code, response_time_ms, error) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Bool, _>(up)
    .bind::<Nullable<Integer>, _>(status_code)
    .bind::<Nullable<Integer>, _>(response_time_ms)
    .bind::<Nullable<Text>, _>(error)
    .execute(conn)
    .await?;

    diesel::sql_query(
        "UPDATE monitors SET status = $2, consecutive_failures = $3, consecutive_successes = $4, \
                updated_at = now(), \
                last_status_changed_at = CASE WHEN $5 THEN now() ELSE last_status_changed_at END \
         WHERE id = $1",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(new_status)
    .bind::<Integer, _>(consecutive_failures)
    .bind::<Integer, _>(consecutive_successes)
    .bind::<Bool, _>(status_changed)
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(QueryableByName)]
struct IdRow { #[diesel(sql_type = SqlUuid)] id: Uuid }

pub async fn open_incident(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    cause: &str,
    last_error: Option<&str>,
) -> QueryResult<Uuid> {
    // ON CONFLICT on the partial unique index: if an incident is already open,
    // keep it and just refresh last_error.
    let row: IdRow = diesel::sql_query(
        "INSERT INTO monitor_incidents (monitor_id, cause, last_error) VALUES ($1, $2, $3) \
         ON CONFLICT (monitor_id) WHERE resolved_at IS NULL \
         DO UPDATE SET last_error = EXCLUDED.last_error RETURNING id",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(cause)
    .bind::<Nullable<Text>, _>(last_error)
    .get_result(conn)
    .await?;
    Ok(row.id)
}

pub async fn resolve_incident(conn: &mut AsyncPgConnection, monitor_id: Uuid) -> QueryResult<()> {
    diesel::sql_query(
        "UPDATE monitor_incidents SET resolved_at = now() \
         WHERE monitor_id = $1 AND resolved_at IS NULL",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .execute(conn)
    .await?;
    Ok(())
}
```

- [ ] **Step 7: Add `update_monitor`**

Use a partial-update via `AsChangeset` is awkward with optional fields; use a raw `UPDATE` with `COALESCE` so only provided fields change:

```rust
#[allow(clippy::too_many_arguments)]
pub async fn update_monitor(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: Option<&str>,
    enabled: Option<bool>,
    status: Option<&str>,
    interval_seconds: Option<i32>,
    webhook_url: Option<Option<&str>>, // outer None = leave; inner None = set NULL
) -> QueryResult<Option<Monitor>> {
    // webhook: encode "leave" as a sentinel by splitting into two binds.
    let (set_webhook, webhook_val) = match webhook_url {
        None => (false, None),
        Some(v) => (true, v),
    };
    diesel::sql_query(
        "UPDATE monitors SET \
            name = COALESCE($2, name), \
            enabled = COALESCE($3, enabled), \
            status = COALESCE($4, status), \
            interval_seconds = COALESCE($5, interval_seconds), \
            webhook_url = CASE WHEN $6 THEN $7 ELSE webhook_url END, \
            next_check_at = CASE WHEN $4 = 'unknown' THEN now() ELSE next_check_at END, \
            updated_at = now() \
         WHERE id = $1 RETURNING *",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Nullable<Text>, _>(name)
    .bind::<Nullable<Bool>, _>(enabled)
    .bind::<Nullable<Text>, _>(status)
    .bind::<Nullable<Integer>, _>(interval_seconds)
    .bind::<Bool, _>(set_webhook)
    .bind::<Nullable<Text>, _>(webhook_val)
    .get_result(conn)
    .await
    .optional()
}
```

- [ ] **Step 8: Verify compilation**

Run: `cd backend && cargo build -p sauron-db`
Expected: compiles. (Behavioral correctness verified in Task 16.)

- [ ] **Step 9: Commit**

```bash
git add backend/crates/sauron-db/src/models.rs backend/crates/sauron-db/src/repo.rs
git commit -m "feat(monitors): db models + repo (CRUD, claim, persist, uptime, prune)"
```

---

## Task 10: The `sauron-monitor` binary (scheduler loop)

**Files:**
- Create: `backend/bins/sauron-monitor/Cargo.toml`
- Create: `backend/bins/sauron-monitor/src/main.rs`

**Interfaces:**
- Consumes: `sauron_monitor_core::{probe, ProbeSpec, Kind, apply, status_str, MonitorState, Status, TransitionKind, WebhookPayload}`, `sauron_db::repo::*`, `sauron_core::Config`.

- [ ] **Step 1: Create the manifest**

`backend/bins/sauron-monitor/Cargo.toml`:

```toml
[package]
name = "sauron-monitor"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[[bin]]
name = "sauron-monitor"
path = "src/main.rs"

[dependencies]
sauron-core = { workspace = true }
sauron-db = { workspace = true }
sauron-monitor-core = { workspace = true }
sauron-telemetry = { workspace = true }
tokio = { workspace = true }
reqwest = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
```

> Naming note: the `Dockerfile` runs `cargo build --release -p ${BIN}` **and** `cp target/release/${BIN}`, so `BIN` must equal both the **package name** and the **produced binary name**. This binary package is therefore named `sauron-monitor` (package + binary), and the reusable logic lives in the separately-named **`sauron-monitor-core`** crate (Tasks 3–7) to avoid a Cargo package-name clash. `BIN=sauron-monitor` in `docker-compose.yml` and `target/release/sauron-monitor` both resolve correctly.

- [ ] **Step 2: Write `main.rs` — setup + client + Config**

```rust
//! `sauron-monitor` — the active uptime prober.
//!
//! A scheduler loop claims due monitors (FOR UPDATE SKIP LOCKED), probes them
//! concurrently, applies the state machine, persists checks/incidents, and
//! fires webhooks. State lives entirely in Postgres; no Redis.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tracing::{info, warn};

use sauron_core::Config;
use sauron_db::models::Monitor;
use sauron_db::{repo, PgPool};
use sauron_monitor_core::{
    apply, probe, status_str, Kind, MonitorState, ProbeSpec, ProbeResult, Status, TransitionKind,
    WebhookPayload,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-monitor");
    let cfg = Arc::new(Config::from_env()?);
    let pool = sauron_db::build_pool(&cfg.database_url, 8)?;

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("Sauron-Monitor/1.0")
        .build()?;

    info!(tick_ms = cfg.monitor_tick_ms, "sauron-monitor started");

    let mut last_prune = chrono::Utc::now();
    loop {
        if let Err(e) = tick(&pool, &http, &cfg).await {
            warn!(error = %e, "monitor tick failed; backing off");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        // Prune old checks roughly hourly.
        if (chrono::Utc::now() - last_prune).num_minutes() >= 60 {
            if let Ok(mut conn) = sauron_db::conn(&pool).await {
                match repo::prune_checks(&mut conn, cfg.monitor_check_retention_days).await {
                    Ok(n) if n > 0 => info!(pruned = n, "pruned old monitor checks"),
                    _ => {}
                }
            }
            last_prune = chrono::Utc::now();
        }
        tokio::time::sleep(Duration::from_millis(cfg.monitor_tick_ms)).await;
    }
}
```

- [ ] **Step 3: Write the `tick` + per-monitor processing**

```rust
async fn tick(pool: &PgPool, http: &reqwest::Client, cfg: &Config) -> anyhow::Result<()> {
    let mut conn = sauron_db::conn(pool).await?;
    let due = repo::claim_due_monitors(&mut conn, cfg.monitor_batch).await?;
    drop(conn); // release the pooled connection while probing
    if due.is_empty() {
        return Ok(());
    }
    let sem = Arc::new(Semaphore::new(cfg.monitor_max_concurrency));
    let mut handles = Vec::new();
    for m in due {
        let pool = pool.clone();
        let http = http.clone();
        let sem = sem.clone();
        let allow_private = cfg.monitor_ssrf_allow_private;
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            if let Err(e) = process_monitor(&pool, &http, &m, allow_private).await {
                warn!(monitor = %m.id, error = %e, "monitor processing failed");
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    Ok(())
}

fn spec_of(m: &Monitor) -> ProbeSpec {
    let cfg = &m.config;
    let headers = cfg.get("headers").and_then(|h| h.as_object()).map(|o| {
        o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect()
    }).unwrap_or_default();
    ProbeSpec {
        kind: if m.kind == "tcp" { Kind::Tcp } else { Kind::Http },
        target: m.target.clone(),
        method: m.method.clone(),
        headers,
        body: cfg.get("body").and_then(|b| b.as_str()).map(|s| s.to_string()),
        expected_status: cfg.get("expected_status").and_then(|s| s.as_str()).unwrap_or("200-399").to_string(),
        body_assertion: cfg.get("body_assertion").and_then(|s| s.as_str()).map(|s| s.to_string()),
        follow_redirects: cfg.get("follow_redirects").and_then(|b| b.as_bool()).unwrap_or(true),
        timeout: Duration::from_millis(m.timeout_ms.max(1) as u64),
    }
}

async fn process_monitor(
    pool: &PgPool,
    http: &reqwest::Client,
    m: &Monitor,
    allow_private: bool,
) -> anyhow::Result<()> {
    let spec = spec_of(m);
    let result: ProbeResult = probe(&spec, http, allow_private).await;

    let cur = match m.status.as_str() {
        "up" => Status::Up,
        "down" => Status::Down,
        "paused" => Status::Paused,
        _ => Status::Unknown,
    };
    let state = MonitorState {
        status: cur,
        consecutive_failures: m.consecutive_failures,
        consecutive_successes: m.consecutive_successes,
        failure_threshold: m.failure_threshold.max(1),
        recovery_threshold: m.recovery_threshold.max(1),
    };
    let outcome = apply(&state, &result);
    let changed = outcome.transition != TransitionKind::None;

    let mut conn = sauron_db::conn(pool).await?;
    repo::record_check_and_state(
        &mut conn,
        m.id,
        result.up,
        result.status_code,
        result.response_time_ms,
        result.error.as_deref(),
        status_str(outcome.new_status),
        outcome.consecutive_failures,
        outcome.consecutive_successes,
        changed,
    )
    .await?;

    let mut incident_id = None;
    match outcome.transition {
        TransitionKind::WentDown => {
            let cause = result.error.clone().unwrap_or_else(|| "check failed".into());
            incident_id = Some(repo::open_incident(&mut conn, m.id, &cause, result.error.as_deref()).await?);
        }
        TransitionKind::WentUp => {
            repo::resolve_incident(&mut conn, m.id).await?;
        }
        TransitionKind::None => {}
    }
    drop(conn);

    if changed {
        if let Some(url) = &m.webhook_url {
            fire_webhook(http, url, m, status_str(cur), status_str(outcome.new_status), incident_id, result.error.as_deref()).await;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn fire_webhook(
    http: &reqwest::Client,
    url: &str,
    m: &Monitor,
    previous: &str,
    status: &str,
    incident_id: Option<uuid::Uuid>,
    cause: Option<&str>,
) {
    let payload = WebhookPayload {
        monitor_id: m.id,
        name: &m.name,
        project_id: m.project_id,
        status,
        previous_status: previous,
        at: chrono::Utc::now(),
        incident_id,
        cause,
        target: &m.target,
    };
    for attempt in 0..3 {
        let res = http.post(url).timeout(Duration::from_secs(5)).json(&payload).send().await;
        match res {
            Ok(r) if r.status().is_success() => return,
            Ok(r) => warn!(status = %r.status(), "webhook non-2xx"),
            Err(e) => warn!(error = %e, "webhook post failed"),
        }
        tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
    }
}
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cd backend && cargo build`
Expected: the whole workspace (including `sauron-monitor` bin) compiles. Fix any import/type mismatches surfaced here.

- [ ] **Step 5: Commit**

```bash
git add backend/bins/sauron-monitor
git commit -m "feat(monitors): sauron-monitor scheduler binary (claim/probe/persist/webhook/prune)"
```

---

## Task 11: API routes (`routes/monitors.rs` + wiring)

**Files:**
- Create: `backend/bins/sauron-api/src/routes/monitors.rs`
- Modify: `backend/bins/sauron-api/src/routes/mod.rs` (add `pub mod monitors;`)
- Modify: `backend/bins/sauron-api/src/main.rs` (register routes)

**Interfaces:**
- Consumes: `authorize_project`, `perm::{MONITOR_READ, MONITOR_WRITE}`, `repo::*`, `cfg.monitor_min_interval_secs`.
- Produces: handlers `list`, `create`, `detail`, `update`, `delete`, `checks`, `incidents`.

- [ ] **Step 1: Add the module**

In `routes/mod.rs`, add `pub mod monitors;` in the alphabetical list (after `journeys;`).

- [ ] **Step 2: Write `routes/monitors.rs`**

```rust
//! Project-scoped uptime monitors: CRUD + read (checks, incidents).

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{authorize_project, perm, AuthUser};
use sauron_db::models::Monitor;
use sauron_db::repo;

use super::db;
use crate::error::ApiError;
use crate::AppState;

const KINDS: [&str; 2] = ["http", "tcp"];

#[derive(Deserialize)]
pub struct RangeQuery {
    pub hours: Option<i64>,
}

#[derive(Deserialize)]
pub struct CreateMonitorReq {
    pub name: String,
    pub kind: String,
    pub target: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default)]
    pub interval_seconds: Option<i32>,
    #[serde(default)]
    pub timeout_ms: Option<i32>,
    #[serde(default)]
    pub failure_threshold: Option<i32>,
    #[serde(default)]
    pub recovery_threshold: Option<i32>,
    #[serde(default)]
    pub webhook_url: Option<String>,
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_project(&mut conn, auth.user_id, project_id, perm::MONITOR_READ).await?;
    let rows = repo::list_monitors_for_project(&mut conn, project_id).await?;
    Ok(Json(json!(rows)))
}

pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<CreateMonitorReq>,
) -> Result<Json<Monitor>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("monitor name is required".into()));
    }
    if !KINDS.contains(&req.kind.as_str()) {
        return Err(ApiError::BadRequest("kind must be 'http' or 'tcp'".into()));
    }
    if req.target.trim().is_empty() {
        return Err(ApiError::BadRequest("target is required".into()));
    }
    let min = state.cfg.monitor_min_interval_secs as i32;
    let interval = req.interval_seconds.unwrap_or(60).max(min);

    let mut conn = db(&state).await?;
    authorize_project(&mut conn, auth.user_id, project_id, perm::MONITOR_WRITE).await?;

    let config = req.config.unwrap_or_else(|| json!({}));
    let new = sauron_db::models::NewMonitor {
        project_id,
        name: req.name.trim(),
        kind: &req.kind,
        target: req.target.trim(),
        method: req.method.as_deref().unwrap_or("GET"),
        config: &config,
        interval_seconds: interval,
        timeout_ms: req.timeout_ms.unwrap_or(10000).clamp(500, 120_000),
        failure_threshold: req.failure_threshold.unwrap_or(2).max(1),
        recovery_threshold: req.recovery_threshold.unwrap_or(1).max(1),
        webhook_url: req.webhook_url.as_deref().filter(|s| !s.is_empty()),
        created_by: Some(auth.user_id),
    };
    let m = repo::create_monitor(&mut conn, new).await?;
    Ok(Json(m))
}

async fn load_authorized(
    state: &AppState,
    user_id: Uuid,
    monitor_id: Uuid,
    perm: &str,
) -> Result<(sauron_db::PgConn, Monitor), ApiError> {
    let mut conn = db(state).await?;
    let project_id = repo::monitor_project(&mut conn, monitor_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_project(&mut conn, user_id, project_id, perm).await?;
    let m = repo::get_monitor(&mut conn, monitor_id).await?.ok_or(ApiError::NotFound)?;
    Ok((conn, m))
}

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (mut conn, m) = load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_READ).await?;
    let uptime_24h = repo::uptime_pct(&mut conn, monitor_id, 24).await?;
    let uptime_7d = repo::uptime_pct(&mut conn, monitor_id, 24 * 7).await?;
    let uptime_30d = repo::uptime_pct(&mut conn, monitor_id, 24 * 30).await?;
    let incidents = repo::list_incidents(&mut conn, monitor_id, 20).await?;
    Ok(Json(json!({
        "monitor": m,
        "uptime": { "h24": uptime_24h, "d7": uptime_7d, "d30": uptime_30d },
        "incidents": incidents,
    })))
}

#[derive(Deserialize)]
pub struct UpdateMonitorReq {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub interval_seconds: Option<i32>,
    pub webhook_url: Option<Option<String>>,
}

pub async fn update(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
    Json(req): Json<UpdateMonitorReq>,
) -> Result<Json<Monitor>, ApiError> {
    let _ = load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_WRITE).await?;
    let mut conn = db(&state).await?;
    // Pausing/enabling flips status too.
    let status = req.enabled.map(|e| if e { "unknown" } else { "paused" });
    let min = state.cfg.monitor_min_interval_secs as i32;
    let interval = req.interval_seconds.map(|i| i.max(min));
    let webhook = req.webhook_url.map(|w| w.as_deref().filter(|s| !s.is_empty()).map(|s| s.to_string()));
    let webhook_ref = webhook.as_ref().map(|w| w.as_deref());
    let m = repo::update_monitor(
        &mut conn,
        monitor_id,
        req.name.as_deref(),
        req.enabled,
        status,
        interval,
        webhook_ref,
    )
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(Json(m))
}

pub async fn delete(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let _ = load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_WRITE).await?;
    let mut conn = db(&state).await?;
    repo::delete_monitor(&mut conn, monitor_id).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn checks(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<Value>, ApiError> {
    let (mut conn, _m) = load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_READ).await?;
    let hours = q.hours.unwrap_or(24).clamp(1, 24 * 90);
    let series = repo::latency_series(&mut conn, monitor_id, hours).await?;
    Ok(Json(json!(series)))
}

pub async fn incidents(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (mut conn, _m) = load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_READ).await?;
    let rows = repo::list_incidents(&mut conn, monitor_id, 50).await?;
    Ok(Json(json!(rows)))
}
```

> `PgConn` is the pooled-connection alias used by `super::db`. If `load_authorized`'s returned connection type does not name-resolve, inline the two `db(&state)` calls in each handler instead of returning the connection (simpler and matches how other handlers each grab their own connection). Prefer the inline form if the alias is not `pub`.

- [ ] **Step 3: Register routes in `main.rs`**

After the funnels/journeys routes block, add:

```rust
        // --- uptime monitors (project-scoped) ---
        .route(
            "/v1/projects/{project_id}/monitors",
            get(routes::monitors::list).post(routes::monitors::create),
        )
        .route(
            "/v1/monitors/{monitor_id}",
            get(routes::monitors::detail)
                .patch(routes::monitors::update)
                .delete(routes::monitors::delete),
        )
        .route(
            "/v1/monitors/{monitor_id}/checks",
            get(routes::monitors::checks),
        )
        .route(
            "/v1/monitors/{monitor_id}/incidents",
            get(routes::monitors::incidents),
        )
```

- [ ] **Step 4: Verify the api builds**

Run: `cd backend && cargo build -p sauron-api`
Expected: compiles.

- [ ] **Step 5: Commit**

```bash
git add backend/bins/sauron-api/src/routes/monitors.rs backend/bins/sauron-api/src/routes/mod.rs backend/bins/sauron-api/src/main.rs
git commit -m "feat(monitors): project-scoped monitor CRUD + read API"
```

---

## Task 12: Dashboard API client + models

**Files:**
- Create: `dashboard/src/lib/api/monitors.ts`
- Modify: `dashboard/src/lib/models/index.ts` (add types)

**Interfaces:**
- Produces TS: `Monitor`, `MonitorListItem`, `MonitorDetail`, `MonitorCheck`, `MonitorIncident`; api fns `listMonitors`, `createMonitor`, `getMonitor`, `updateMonitor`, `deleteMonitor`, `getMonitorChecks`, `getMonitorIncidents`.

- [ ] **Step 1: Add model types**

Append to `dashboard/src/lib/models/index.ts`:

```typescript
export type MonitorStatus = 'unknown' | 'up' | 'down' | 'paused';

export interface MonitorListItem {
  id: string;
  name: string;
  kind: 'http' | 'tcp';
  target: string;
  status: MonitorStatus;
  enabled: boolean;
  last_response_time_ms: number | null;
  last_checked_at: string | null;
  uptime_24h: number | null;
}

export interface Monitor {
  id: string;
  project_id: string;
  name: string;
  kind: 'http' | 'tcp';
  target: string;
  method: string;
  config: Record<string, unknown>;
  interval_seconds: number;
  timeout_ms: number;
  failure_threshold: number;
  recovery_threshold: number;
  webhook_url: string | null;
  enabled: boolean;
  status: MonitorStatus;
  last_checked_at: string | null;
  next_check_at: string;
  created_at: string;
}

export interface MonitorIncident {
  id: string;
  monitor_id: string;
  started_at: string;
  resolved_at: string | null;
  cause: string;
  last_error: string | null;
}

export interface MonitorDetail {
  monitor: Monitor;
  uptime: { h24: number | null; d7: number | null; d30: number | null };
  incidents: MonitorIncident[];
}

export interface MonitorCheck {
  checked_at: string;
  up: boolean;
  response_time_ms: number | null;
  status_code: number | null;
  error: string | null;
}
```

- [ ] **Step 2: Write the api client**

`dashboard/src/lib/api/monitors.ts` (mirror `funnels.ts`; `api` from `./client`):

```typescript
import { api } from './client';
import type {
  Monitor,
  MonitorCheck,
  MonitorDetail,
  MonitorListItem,
} from '../models';

export async function listMonitors(projectId: string): Promise<MonitorListItem[]> {
  const { data } = await api.get<MonitorListItem[]>(`/v1/projects/${projectId}/monitors`);
  return data;
}

export interface CreateMonitorBody {
  name: string;
  kind: 'http' | 'tcp';
  target: string;
  method?: string;
  config?: Record<string, unknown>;
  interval_seconds?: number;
  timeout_ms?: number;
  webhook_url?: string;
}

export async function createMonitor(projectId: string, body: CreateMonitorBody): Promise<Monitor> {
  const { data } = await api.post<Monitor>(`/v1/projects/${projectId}/monitors`, body);
  return data;
}

export async function getMonitor(id: string): Promise<MonitorDetail> {
  const { data } = await api.get<MonitorDetail>(`/v1/monitors/${id}`);
  return data;
}

export interface UpdateMonitorBody {
  name?: string;
  enabled?: boolean;
  interval_seconds?: number;
  webhook_url?: string | null;
}

export async function updateMonitor(id: string, body: UpdateMonitorBody): Promise<Monitor> {
  const { data } = await api.patch<Monitor>(`/v1/monitors/${id}`, body);
  return data;
}

export async function deleteMonitor(id: string): Promise<void> {
  await api.delete(`/v1/monitors/${id}`);
}

export async function getMonitorChecks(id: string, hours = 24): Promise<MonitorCheck[]> {
  const { data } = await api.get<MonitorCheck[]>(`/v1/monitors/${id}/checks`, { params: { hours } });
  return data;
}
```

- [ ] **Step 3: Verify typecheck**

Run: `cd dashboard && npm run typecheck`
Expected: 0 errors (types resolve; unused-import errors mean a page task will consume them).

- [ ] **Step 4: Commit**

```bash
git add dashboard/src/lib/api/monitors.ts dashboard/src/lib/models/index.ts
git commit -m "feat(monitors): dashboard api client + types"
```

---

## Task 13: Dashboard — status pill, Uptime list page, route, sidebar

**Files:**
- Create: `dashboard/src/lib/components/ui/StatusPill.svelte`
- Create: `dashboard/src/pages/Monitors.svelte`
- Modify: `dashboard/src/routes.ts`
- Modify: `dashboard/src/lib/components/layout/Sidebar.svelte`

**Interfaces:**
- Consumes: `listMonitors`, `createMonitor`, `sessionStore.currentProjectId`, `sessionStore.can`.

- [ ] **Step 1: Create `StatusPill.svelte`**

```svelte
<script lang="ts">
  import type { MonitorStatus } from '../../models';
  let { status }: { status: MonitorStatus } = $props();
  const label: Record<MonitorStatus, string> = {
    up: 'Up', down: 'Down', paused: 'Paused', unknown: 'Pending',
  };
</script>

<span class="pill {status}">{label[status]}</span>

<style>
  .pill { display: inline-flex; align-items: center; padding: 2px 9px; border-radius: 999px;
    font-size: 12px; font-weight: 600; border: 1px solid var(--border); }
  .up { color: #16794a; background: color-mix(in srgb, #16794a 12%, transparent); border-color: color-mix(in srgb, #16794a 40%, transparent); }
  .down { color: #b42318; background: color-mix(in srgb, #b42318 12%, transparent); border-color: color-mix(in srgb, #b42318 40%, transparent); }
  .paused { color: var(--text-muted); background: var(--surface-2); }
  .unknown { color: var(--text-faint); background: var(--surface-2); }
</style>
```

- [ ] **Step 2: Create `Monitors.svelte`** (list + create form)

```svelte
<script lang="ts">
  import { push } from 'svelte-spa-router';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listMonitors, createMonitor } from '../lib/api/monitors';
  import type { MonitorListItem } from '../lib/models';
  import StatusPill from '../lib/components/ui/StatusPill.svelte';

  let monitors = $state<MonitorListItem[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let showForm = $state(false);

  // create form
  let name = $state('');
  let kind = $state<'http' | 'tcp'>('http');
  let target = $state('');
  let method = $state('GET');
  let interval = $state(60);
  let webhook = $state('');
  let saving = $state(false);

  const projectId = $derived(sessionStore.currentProjectId);
  const canWrite = $derived(sessionStore.can('monitor:write', { project: projectId }));

  async function load() {
    if (!projectId) { loading = false; return; }
    loading = true; error = null;
    try { monitors = await listMonitors(projectId); }
    catch (e) { error = (e as Error).message; }
    finally { loading = false; }
  }

  async function submit() {
    if (!projectId || !name || !target) return;
    saving = true;
    try {
      await createMonitor(projectId, {
        name, kind, target, method: kind === 'http' ? method : undefined,
        interval_seconds: interval, webhook_url: webhook || undefined,
      });
      showForm = false; name = ''; target = ''; webhook = '';
      await load();
    } catch (e) { error = (e as Error).message; }
    finally { saving = false; }
  }

  $effect(() => { void projectId; load(); });
</script>

<div class="page">
  <header>
    <h1>Uptime</h1>
    {#if canWrite}
      <button onclick={() => (showForm = !showForm)}>{showForm ? 'Cancel' : 'New monitor'}</button>
    {/if}
  </header>

  {#if showForm}
    <div class="form">
      <input placeholder="Name" bind:value={name} />
      <select bind:value={kind}>
        <option value="http">HTTP(S)</option>
        <option value="tcp">TCP</option>
      </select>
      {#if kind === 'http'}
        <input placeholder="https://example.com/health" bind:value={target} />
        <select bind:value={method}>
          <option>GET</option><option>POST</option><option>HEAD</option>
        </select>
      {:else}
        <input placeholder="host:port (e.g. db.example.com:5432)" bind:value={target} />
      {/if}
      <input type="number" min="30" bind:value={interval} /> <span>sec</span>
      <input placeholder="Webhook URL (optional)" bind:value={webhook} />
      <button disabled={saving} onclick={submit}>{saving ? 'Saving…' : 'Create'}</button>
    </div>
  {/if}

  {#if error}<p class="err">{error}</p>{/if}
  {#if loading}
    <p>Loading…</p>
  {:else if monitors.length === 0}
    <p class="empty">No monitors yet.</p>
  {:else}
    <table>
      <thead><tr><th>Name</th><th>Target</th><th>Status</th><th>Uptime 24h</th><th>Latency</th><th>Checked</th></tr></thead>
      <tbody>
        {#each monitors as m (m.id)}
          <tr class="row" onclick={() => push(`/monitors/${m.id}`)}>
            <td>{m.name} <span class="kind">{m.kind}</span></td>
            <td class="mono">{m.target}</td>
            <td><StatusPill status={m.status} /></td>
            <td>{m.uptime_24h == null ? '—' : `${m.uptime_24h.toFixed(1)}%`}</td>
            <td>{m.last_response_time_ms == null ? '—' : `${m.last_response_time_ms} ms`}</td>
            <td>{m.last_checked_at ? new Date(m.last_checked_at).toLocaleTimeString() : '—'}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .page { padding: 20px; }
  header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px; }
  .form { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin-bottom: 16px;
    padding: 12px; border: 1px solid var(--border); border-radius: var(--radius); }
  input, select { padding: 6px 8px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--surface); color: var(--text); }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: 9px 10px; border-bottom: 1px solid var(--border); font-size: 13.5px; }
  .row { cursor: pointer; }
  .row:hover { background: var(--surface-2); }
  .mono { font-family: ui-monospace, monospace; font-size: 12.5px; color: var(--text-muted); }
  .kind { font-size: 11px; color: var(--text-faint); text-transform: uppercase; margin-left: 6px; }
  .err { color: #b42318; }
  .empty { color: var(--text-faint); }
</style>
```

- [ ] **Step 3: Register the routes**

In `routes.ts`, add the imports and route entries:

```typescript
import Monitors from './pages/Monitors.svelte';
import MonitorDetail from './pages/MonitorDetail.svelte';
```

Add under a new `// Uptime` group in the `routes` object:

```typescript
  '/monitors': guarded(Monitors as Component<never>),
  '/monitors/:id': guarded(MonitorDetail as Component<never>),
```

- [ ] **Step 4: Add the sidebar group**

In `Sidebar.svelte`, insert a new group after `Monitor` (or into it). Add to the `groups` array:

```typescript
    {
      label: 'Uptime',
      items: [
        { href: '#/monitors', label: 'Monitors', icon: 'activity', match: (p) => p.startsWith('/monitors'),
          show: () => sessionStore.can('monitor:read') },
      ],
    },
```

> Verify `'activity'` is a valid `IconName` in `Icon.svelte`; if not, pick an existing icon (e.g. `'zap'` or `'clock'`). Do not invent an icon name.

- [ ] **Step 5: Verify typecheck + build**

Run: `cd dashboard && npm run typecheck && npm run build`
Expected: 0 type errors (a missing `MonitorDetail.svelte` import will error — create the stub in Task 14 first if building this in isolation; when executing in order, do Step 5's build at the end of Task 14).

- [ ] **Step 6: Commit**

```bash
git add dashboard/src/pages/Monitors.svelte dashboard/src/lib/components/ui/StatusPill.svelte dashboard/src/routes.ts dashboard/src/lib/components/layout/Sidebar.svelte
git commit -m "feat(monitors): Uptime list page + status pill + nav"
```

---

## Task 14: Dashboard — monitor detail page

**Files:**
- Create: `dashboard/src/pages/MonitorDetail.svelte`

**Interfaces:**
- Consumes: `getMonitor`, `getMonitorChecks`, `updateMonitor`, `deleteMonitor`.

- [ ] **Step 1: Create `MonitorDetail.svelte`**

```svelte
<script lang="ts">
  import { push } from 'svelte-spa-router';
  import { getMonitor, getMonitorChecks, updateMonitor, deleteMonitor } from '../lib/api/monitors';
  import type { MonitorDetail, MonitorCheck } from '../lib/models';
  import { sessionStore } from '../lib/stores/session.svelte';
  import StatusPill from '../lib/components/ui/StatusPill.svelte';

  let { params }: { params: { id: string } } = $props();

  let detail = $state<MonitorDetail | null>(null);
  let checks = $state<MonitorCheck[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  const canWrite = $derived(
    sessionStore.can('monitor:write', { project: detail?.monitor.project_id }),
  );

  async function load() {
    loading = true; error = null;
    try {
      detail = await getMonitor(params.id);
      checks = await getMonitorChecks(params.id, 24);
    } catch (e) { error = (e as Error).message; }
    finally { loading = false; }
  }

  async function togglePause() {
    if (!detail) return;
    const nextEnabled = detail.monitor.status === 'paused';
    await updateMonitor(params.id, { enabled: nextEnabled });
    await load();
  }

  async function remove() {
    if (!detail) return;
    if (!confirm(`Delete monitor "${detail.monitor.name}"?`)) return;
    await deleteMonitor(params.id);
    push('/monitors');
  }

  const fmtPct = (v: number | null | undefined) => (v == null ? '—' : `${v.toFixed(2)}%`);

  $effect(() => { void params.id; load(); });
</script>

{#if loading}
  <p class="p">Loading…</p>
{:else if error}
  <p class="p err">{error}</p>
{:else if detail}
  <div class="p">
    <a href="#/monitors" class="back">← Uptime</a>
    <header>
      <div>
        <h1>{detail.monitor.name} <StatusPill status={detail.monitor.status} /></h1>
        <p class="mono">{detail.monitor.kind.toUpperCase()} · {detail.monitor.target}</p>
      </div>
      {#if canWrite}
        <div class="actions">
          <button onclick={togglePause}>{detail.monitor.status === 'paused' ? 'Resume' : 'Pause'}</button>
          <button class="danger" onclick={remove}>Delete</button>
        </div>
      {/if}
    </header>

    <div class="tiles">
      <div class="tile"><span>Uptime 24h</span><strong>{fmtPct(detail.uptime.h24)}</strong></div>
      <div class="tile"><span>Uptime 7d</span><strong>{fmtPct(detail.uptime.d7)}</strong></div>
      <div class="tile"><span>Uptime 30d</span><strong>{fmtPct(detail.uptime.d30)}</strong></div>
      <div class="tile"><span>Interval</span><strong>{detail.monitor.interval_seconds}s</strong></div>
    </div>

    <h2>Recent checks</h2>
    <table>
      <thead><tr><th>Time</th><th>Result</th><th>Code</th><th>Latency</th><th>Error</th></tr></thead>
      <tbody>
        {#each checks.slice().reverse().slice(0, 50) as c (c.checked_at)}
          <tr>
            <td>{new Date(c.checked_at).toLocaleString()}</td>
            <td class={c.up ? 'ok' : 'bad'}>{c.up ? 'up' : 'down'}</td>
            <td>{c.status_code ?? '—'}</td>
            <td>{c.response_time_ms == null ? '—' : `${c.response_time_ms} ms`}</td>
            <td class="mono">{c.error ?? ''}</td>
          </tr>
        {/each}
      </tbody>
    </table>

    <h2>Incidents</h2>
    {#if detail.incidents.length === 0}
      <p class="empty">No incidents recorded.</p>
    {:else}
      <table>
        <thead><tr><th>Started</th><th>Resolved</th><th>Cause</th></tr></thead>
        <tbody>
          {#each detail.incidents as i (i.id)}
            <tr>
              <td>{new Date(i.started_at).toLocaleString()}</td>
              <td>{i.resolved_at ? new Date(i.resolved_at).toLocaleString() : 'ongoing'}</td>
              <td>{i.cause}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
{/if}

<style>
  .p { padding: 20px; }
  .back { color: var(--text-muted); font-size: 13px; }
  header { display: flex; justify-content: space-between; align-items: flex-start; margin: 8px 0 18px; }
  .mono { font-family: ui-monospace, monospace; color: var(--text-muted); font-size: 13px; }
  .actions { display: flex; gap: 8px; }
  .danger { color: #b42318; }
  .tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 12px; margin-bottom: 24px; }
  .tile { border: 1px solid var(--border); border-radius: var(--radius); padding: 12px 14px; display: flex; flex-direction: column; gap: 4px; }
  .tile span { font-size: 12px; color: var(--text-faint); }
  .tile strong { font-size: 20px; }
  table { width: 100%; border-collapse: collapse; margin-bottom: 24px; }
  th, td { text-align: left; padding: 8px 10px; border-bottom: 1px solid var(--border); font-size: 13px; }
  .ok { color: #16794a; } .bad { color: #b42318; }
  .err { color: #b42318; } .empty { color: var(--text-faint); }
</style>
```

- [ ] **Step 2: Verify typecheck + build**

Run: `cd dashboard && npm run typecheck && npm run build`
Expected: 0 errors; production build succeeds.

- [ ] **Step 3: Commit**

```bash
git add dashboard/src/pages/MonitorDetail.svelte
git commit -m "feat(monitors): monitor detail page (uptime tiles, checks, incidents)"
```

---

## Task 15: docker-compose service + env

**Files:**
- Modify: `docker-compose.yml`
- Modify: `.env.example`

- [ ] **Step 1: Add the `sauron-monitor` service**

In `docker-compose.yml`, add after the `ingest` service (no published port):

```yaml
  monitor:
    build:
      context: ./backend
      args:
        BIN: sauron-monitor
    environment:
      DATABASE_URL: postgres://${POSTGRES_USER:-sauron}:${POSTGRES_PASSWORD:-sauron}@postgres:5432/${POSTGRES_DB:-sauron}
      MONITOR_TICK_MS: ${MONITOR_TICK_MS:-1000}
      MONITOR_BATCH: ${MONITOR_BATCH:-100}
      MONITOR_MAX_CONCURRENCY: ${MONITOR_MAX_CONCURRENCY:-50}
      MONITOR_CHECK_RETENTION_DAYS: ${MONITOR_CHECK_RETENTION_DAYS:-30}
      MONITOR_MIN_INTERVAL_SECS: ${MONITOR_MIN_INTERVAL_SECS:-30}
      MONITOR_SSRF_ALLOW_PRIVATE: ${MONITOR_SSRF_ALLOW_PRIVATE:-false}
      RUST_LOG: ${RUST_LOG:-info,sauron=debug}
    depends_on:
      migrate:
        condition: service_completed_successfully
      postgres:
        condition: service_healthy
```

> The API also validates `monitor_min_interval_secs`; add `MONITOR_MIN_INTERVAL_SECS` to the `api` service's `environment` block too (same default) so create/patch clamping matches the prober.

- [ ] **Step 2: Add env defaults to `.env.example`**

Append:

```bash
# --- uptime monitor (sauron-monitor) ---
MONITOR_TICK_MS=1000
MONITOR_BATCH=100
MONITOR_MAX_CONCURRENCY=50
MONITOR_CHECK_RETENTION_DAYS=30
MONITOR_MIN_INTERVAL_SECS=30
# Set true only for internal self-monitoring of private addresses.
MONITOR_SSRF_ALLOW_PRIVATE=false
```

- [ ] **Step 3: Commit**

```bash
git add docker-compose.yml .env.example
git commit -m "feat(monitors): sauron-monitor compose service + MONITOR_* env"
```

---

## Task 16: End-to-end verification (docker compose)

This is the behavioral gate for every DB/handler/prober/UI change (there is no in-process integration harness).

- [ ] **Step 1: Build and start the full stack**

Run: `cp .env.example .env` (edit `JWT_SECRET`), then `docker compose up --build -d`
Expected: `postgres`, `redis`, `migrate` (exits 0), `ingest`, `api`, `monitor`, `dashboard` all start; migration `2026-07-14-000009_monitors` applied. Check `docker compose logs monitor` shows `sauron-monitor started`.

- [ ] **Step 2: Get an access token + a project id**

Register/login via the API (mirror the existing e2e used for prior features):

```bash
API=http://localhost:10000
TOKEN=$(curl -s $API/v1/auth/register -H 'content-type: application/json' \
  -d '{"email":"mon@test.dev","password":"password123","name":"Mon"}' | jq -r .access_token)
ORG=$(curl -s $API/v1/orgs -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d '{"name":"Mon Org"}' | jq -r .id)
PROJECT=$(curl -s $API/v1/orgs/$ORG/projects -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d '{"name":"Mon Project"}' | jq -r .id)
```

- [ ] **Step 3: Create an up monitor (the API health) and a down monitor**

```bash
# UP: probe the api container's health over the compose network is not reachable from
# the monitor via localhost; use a public fast endpoint or the api service DNS name.
# Simplest deterministic "up": TCP to postgres is blocked by SSRF (private). Use a public host:
curl -s $API/v1/projects/$PROJECT/monitors -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"name":"Example","kind":"http","target":"https://example.com","interval_seconds":30}'
# DOWN: unroutable port on a public host → connect refused/timeout
curl -s $API/v1/projects/$PROJECT/monitors -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"name":"Down","kind":"tcp","target":"example.com:9","interval_seconds":30,"failure_threshold":2}'
```

> To exercise SSRF blocking, also create `{"kind":"http","target":"http://169.254.169.254/"}` and confirm its checks record `error` like "resolves to a blocked address" and it never goes `up`.

- [ ] **Step 4: Wait ~2 intervals, then assert transitions**

Run:
```bash
sleep 75
curl -s $API/v1/projects/$PROJECT/monitors -H "authorization: Bearer $TOKEN" | jq
```
Expected: the Example monitor shows `"status":"up"` with `uptime_24h` near 100 and a numeric `last_response_time_ms`; the Down monitor shows `"status":"down"` after 2 failed checks.

- [ ] **Step 5: Assert incident + checks on the detail endpoint**

```bash
DOWN_ID=... # id from step 3
curl -s $API/v1/monitors/$DOWN_ID -H "authorization: Bearer $TOKEN" | jq '.incidents, .uptime'
curl -s $API/v1/monitors/$DOWN_ID/checks -H "authorization: Bearer $TOKEN" | jq 'length'
```
Expected: one open incident (`resolved_at: null`) with a `cause`; the checks array grows over time.

- [ ] **Step 6: Verify webhook fires**

Create a monitor whose `webhook_url` points at a request-capture endpoint (e.g. a temporary `https://webhook.site/...` URL or a tiny local echo). Force a transition (create a down monitor with a webhook) and confirm a POST with `status`, `previous_status`, `cause` arrives.

- [ ] **Step 7: Verify the dashboard**

Open `http://localhost:10002` → log in → the **Uptime** nav item appears → the Monitors list shows both monitors with status pills → clicking one opens the detail with uptime tiles, the recent-checks table, and (for the down one) the incident. Confirm **Pause** flips status to `paused` and probing stops (no new checks), **Resume** re-arms it.

- [ ] **Step 8: Tear down**

Run: `docker compose down` (keep volumes) — or `docker compose down -v` to reset.

- [ ] **Step 9: Final commit (if any e2e fixups were needed)**

```bash
git add -A
git commit -m "fix(monitors): e2e verification fixups"
```

---

## Self-Review

**Spec coverage** (each spec section → task):
- Data model (3 tables, RBAC) → Tasks 1, 2, 9. ✓
- Prober crate (status/state/ssrf/webhook/probe) → Tasks 3–7. ✓
- Scheduler bin (claim, concurrency, persist, webhook, prune) → Tasks 9 (repo) + 10 (bin). ✓
- API endpoints (project-scoped list/create; standalone detail/update/delete/checks/incidents) → Task 11. ✓
- Dashboard (Uptime list, detail, status pill, nav, api/models) → Tasks 12–14. ✓
- Config + deployment (Config fields, compose service, env) → Tasks 8, 15. ✓
- Error handling & security (loop resilience, restart safety, SSRF) → Task 5 (SSRF) + Task 10 (`tick` back-off, per-probe `spawn`) + claim-reschedule (Task 9). ✓
- Testing strategy (pure unit tests + compose e2e) → Tasks 3–6 unit tests; Task 16 e2e. ✓
- Out-of-scope items → not implemented (correct). ✓

**Placeholder scan:** No "TBD/TODO/handle appropriately". The callouts (`PgConn` alias fallback in Task 11; `activity` icon check in Task 13; lib/bin package naming in Task 10) are explicit conditional instructions with a concrete fallback, not deferrals.

**Type consistency:** `ProbeResult`, `MonitorState`, `Outcome`, `TransitionKind`, `Status`, `status_str`, `apply`, `probe`, `ProbeSpec`, `Kind`, `WebhookPayload` are defined in Tasks 3–7 and consumed with matching signatures in Task 10. Repo function names in Task 9 match their call sites in Tasks 10–11 (`claim_due_monitors`, `record_check_and_state`, `open_incident`, `resolve_incident`, `uptime_pct`, `latency_series`, `list_incidents`, `list_monitors_for_project`, `create_monitor`, `get_monitor`, `monitor_project`, `update_monitor`, `delete_monitor`, `prune_checks`). TS types in Task 12 match the JSON shapes returned by Task 11 handlers.

**Known risk flagged for the implementer:** the `Dockerfile` uses `-p ${BIN}` + `cp target/release/${BIN}`, so the binary *package* must be named `sauron-monitor`; the reusable logic therefore lives in the `sauron-monitor-core` crate (Tasks 3–7) and the bin (Task 10) depends on it. `BIN=sauron-monitor` in compose is unchanged.
