# S0 Email Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the Sauron backend a deployment-level SMTP relay, a reusable HTML/plain-text email template engine, and a durable `mail_outbox` queue drained by a supervised background task inside `sauron-api`, so later slices can send a product email without touching a socket on the request path.

**Architecture:** A new leaf crate `backend/crates/sauron-mail` owns message composition (templates, escaping, `{{var}}` substitution) and transmission (SSRF-checked, deadline-bounded SMTP over `lettre`). A new `mail_outbox` Postgres table plus repository functions in `sauron-db` hold rendered messages durably; `backend/bins/sauron-api/src/mail.rs` renders and INSERTs at request time and drains off it, supervised by a new `backend/bins/sauron-api/src/tasks.rs`. `sauron-alerts` drops its own `lettre` dependency and routes its email channel through `sauron-mail`, with byte-identical output.

**Tech Stack:** Rust 1.82 (workspace MSRV), `lettre` 0.11.22 (rustls, `builder`, `smtp-transport`, `tokio1-rustls-tls`), `diesel` 2.3 + `diesel-async` 0.9 (Postgres), `tokio` (rt-multi-thread, sync, time), `thiserror`, `tracing`, `axum` 0.8.

## Global Constraints

- **NEVER run `git commit`, `git add`, or create a branch.** The repository owner commits manually.
- **Never use `conn.transaction(...)`.** The MSRV (1.82) blocks it. Multi-statement atomicity is one data-modifying CTE via `diesel::sql_query` with `.bind()`.
- **`backend/crates/sauron-db/src/schema.rs` is HAND-MAINTAINED.** The diesel CLI must NEVER run. A new table means hand-editing three places: a `diesel::table!` block, a `diesel::joinable!` line if it has an FK, and the name in `allow_tables_to_appear_in_same_query!`.
- **Migrations** are `backend/migrations/YYYY-MM-DD-0000NN_slug/{up,down}.sql`, BOTH files required, `up.sql` opening with a prose comment explaining WHY. A migration runs in ONE transaction; `CONCURRENTLY` is unavailable; an index build on a partitioned parent locks every child.
- **S0 consumes migration `000034` and only `000034`.** Directory name: `2026-08-01-000034_mail_outbox`. Diesel orders migrations lexicographically by the full `YYYY-MM-DD-0000NN` string, so the date prefix is the **landing** date and must never decrease as NN increases. Downstream numbers are already allocated: S2=000035, S1=000036, S3=000037, S4=000038–000040, S5=000041–000043. Do not take any of them.
- **Enum-like columns are TEXT + CHECK**, never custom SQL types. The one deliberate exception in this slice is `mail_outbox.kind`, which has no CHECK — the reason is written into `up.sql`.
- **All SQL lives in `backend/crates/sauron-db/src/repo.rs`** as free `pub async fn name(conn: &mut AsyncPgConnection, ...) -> QueryResult<T>`. Handlers never build queries inline.
- **Insertable-only structs must NOT gain a `Queryable` derive** — `Queryable` decodes positionally and would silently bind fields to the wrong columns.
- **Never hold a pooled `PgConn` across network I/O.** The API pool is 16 connections for the whole process. `drop(conn)` first.
- **`Config::from_env` never bails on a new field.** Every new setting is a recorded `Result` reached through a `require_*()` accessor, because `Config` is shared by every binary and bailing there once took down `sauron-ingest` and `sauron-tier`.
- **No task's initialization may `?` out of `main()`.** `sauron-api.service` is `Restart=on-failure` with no StartLimit override; a `?` on a missing table burns systemd's five-starts-in-ten-seconds budget and leaves the unit `failed` with no HTTP surface to diagnose from.
- **`schema.rs` claims are deltas, never absolute counts.** S0 is **+1 table**. Never assert a total — four later slices add tables to the same file.
- **Comments explain the failure mode that motivated the code, not what the code does.** Match that register.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` are hard gates.
- **Dashboard: nothing.** S0 adds no page, no route, no Sidebar entry, no `models/*.ts`, and no permission. `perm::ALL` stays `[&str; 27]`.

### Commands used verbatim throughout this plan

```
# Rust build/check (workspace)
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets

# Rust unit tests, no database
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p <crate> <testname>

# Rust tests needing a real Postgres (harness returns None and SKIPS when unset)
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p <crate> --test <file> <testname> -- --nocapture

# Apply migrations to the live dev database
cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate

# Formatting + lint gates
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```

`DUCKDB_LIB_DIR` is required on every cargo invocation in this repository: DuckDB is linked unbundled, so `sauron-tier` cannot link without it. Shorten it in your shell with `export DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu` once per session; the steps below spell it out so a step copied in isolation still works.

## File Structure

### Created

| Path | Responsibility |
|---|---|
| `backend/migrations/2026-08-01-000034_mail_outbox/up.sql` | Creates `mail_outbox` + four indexes. Prose header carries the no-`org_id`, no-`kind`-CHECK, credential-at-rest and `expires_at` decisions. |
| `backend/migrations/2026-08-01-000034_mail_outbox/down.sql` | `DROP TABLE IF EXISTS mail_outbox;` — drops only what this `up.sql` created. |
| `backend/crates/sauron-mail/Cargo.toml` | Leaf crate manifest. Depends on `sauron-core`, `sauron-monitor-core`, `lettre`, `tokio`, `tracing`, `thiserror`. **Not** `sauron-db`. |
| `backend/crates/sauron-mail/src/lib.rs` | Crate doc (composition vs. queueing split; the outbox is the programme's async side-effect primitive), module declarations, re-exports. |
| `backend/crates/sauron-mail/src/text.rs` | `pub fn substitute`, `pub fn html_escape` — **moved** verbatim out of `sauron-alerts/src/render.rs`, with their tests. |
| `backend/crates/sauron-mail/src/kind.rs` | `MailKind` + `as_str()` + `dedup_window()`. The authority for the `kind` column's value set. |
| `backend/crates/sauron-mail/src/template.rs` | `MailContent`, `Cta`, `Branding`, `RenderedMail`, `TemplateError`, `render()`, `LAYOUT_HTML` and its three sub-templates. |
| `backend/crates/sauron-mail/src/smtp.rs` | `SmtpParams`, `MailBody`, `OutgoingMail`, `MailError`, `SmtpClient`, one-shot `send()`, `is_transient()`, `normalize_recipient()`, the dev sink. |
| `backend/crates/sauron-core/tests/config_keys_documented.rs` | Asserts every `var("KEY")` / `parse("KEY"` literal in `config.rs` appears in `.env.example`. |
| `backend/crates/sauron-db/tests/mail_outbox.rs` | Integration tests for the ten `mail_outbox` repo functions against a real Postgres. |
| `backend/bins/sauron-api/src/tasks.rs` | `TaskHealth`, `supervise()`, the process-global task registry `/health` reads. |
| `backend/bins/sauron-api/src/mail.rs` | `MailSender` (render → enqueue → nudge → drain) and the free `hygiene()` sweep. |
| `backend/bins/sauron-api/tests/http_mail_outbox.rs` | Boots the real binary in three SMTP configurations and asserts `/health` stays 200 and lists `mail_hygiene` unconditionally. |

### Modified

| Path | Change |
|---|---|
| `backend/Cargo.toml` | `sauron-mail = { path = "crates/sauron-mail" }` in the `# --- internal crates ---` block. |
| `backend/crates/sauron-alerts/Cargo.toml` | Drop `lettre`, add `sauron-mail`. |
| `backend/bins/sauron-api/Cargo.toml` | Add `sauron-mail`. |
| `backend/crates/sauron-core/src/config.rs` | `SmtpSettings`, `SmtpTls`, `build_smtp()`, private `smtp`/`dashboard_url` results, `pub dev_mode`, `mail_drain_tick_secs`, `mail_outbox_retention_days`, `require_smtp()`, `require_dashboard_url()`, hand-written `Debug` for `Config`. |
| `backend/crates/sauron-db/src/schema.rs` | `mail_outbox` `table!` block, `joinable!(mail_outbox -> users (user_id))`, `mail_outbox,` in `allow_tables_to_appear_in_same_query!`. |
| `backend/crates/sauron-db/src/models.rs` | `MailOutbox` (no `Serialize`, no derived `Debug`) + hand-written redacting `Debug`; `NewMailOutbox<'a>` (`Insertable` only). |
| `backend/crates/sauron-db/src/repo.rs` | Ten `mail_outbox` functions + one private `QueryableByName` row struct. |
| `backend/crates/sauron-alerts/src/render.rs` | `pub use sauron_mail::text::substitute;` + `use sauron_mail::text::html_escape;`; local definitions and their tests removed. |
| `backend/crates/sauron-alerts/src/deliver.rs` | `deliver_email` shrinks to building `SmtpParams` + `OutgoingMail` and calling `sauron_mail::send`. All `lettre` imports removed. |
| `backend/crates/sauron-alerts/src/engine.rs` | The four-substring retry predicate becomes `sauron_mail::is_transient(&e)`. |
| `backend/bins/sauron-api/src/main.rs` | `mod mail; mod tasks;`, `AppState.mail`, `Config` behind an `Arc` earlier, `/health` JSON body, two supervised tasks. |
| `.env.example` | New `# --- transactional email (sauron-api) ---` block; `DASHBOARD_URL` in `# --- CORS / URLs ---`; sixteen previously-undocumented keys added so the new config-key assertion can be switched on. |
| `docker-compose.yml` | `api:` service gains `DASHBOARD_URL` and every `SMTP_*` / `MAIL_*` as a `${VAR:-}` passthrough. **No fallback on `DASHBOARD_URL`.** |
| `README.md` | `### Transactional email` table after `### Alerting & notifications`; `DASHBOARD_URL` row in `### Dashboard API`. |
| `packaging/rpm/config/api.env` | Commented `DASHBOARD_URL` + `SMTP_*` block; a pointer for `SMTP_PASSWORD` to `/etc/sauron/secret.env`. |
| `packaging/rpm/SETUP.md` | `SMTP_PASSWORD` in the secret.env section; a new **§11 "Upgrading"** with the stop/migrate/start gate and a per-migration table later slices append to. |
| `packaging/rpm/sauron.spec` | `Release` bumped `1` → `2` so the build has a new NEVR and `dnf upgrade` will install it; `%changelog` entry naming `mail_outbox` and instructing `sauron-migrate` after upgrading. |

### Reconciliations applied while writing this plan

These are places the slice design and the programme design were ambiguous. The plan picks one and says why; a reviewer should check the choice, not re-derive it.

1. **`MAIL_OUTBOX_RETENTION_DAYS` is an env var.** Programme P4 says "retention values are compile-time constants", but slice §10 lists it in the variable table with a default of `30`, and the slice's own "thirteen new variables" arithmetic only reaches thirteen if it is counted. P4's sentence is about the reapers P4 *newly assigns* (`password_reset_tokens` and friends). It stays an env var.
2. **`hygiene` is a free function, not a `MailSender` method.** Slice §6 lists `hygiene` as a method on `MailSender`, but §7 requires the hygiene task to run **unconditionally**, including on deployments where `AppState.mail` is `None` and no `MailSender` exists. A method on a type that is not constructed cannot run unconditionally. `pub async fn hygiene(pool: &PgPool, retention_days: i64)` is the only shape that satisfies the requirement the design nominates as its answer to credential-at-rest.
3. **The task-health registry is a module-global in `tasks.rs`, not a second `AppState` field.** The design says `AppState` gains exactly one field (`mail`). `/health` needs the registry; a `static` inside `tasks.rs` that `supervise()` writes and `snapshot()` reads keeps that promise and lets the `/health` handler take no state at all.
4. **`MailSender` carries `from_address` and `from_name`.** `SmtpParams` deliberately has no From fields, but `OutgoingMail` needs them and the drain builds `OutgoingMail`. They come from `SmtpSettings` at construction.
5. **The config-key assertion is a Rust test, not a `ci.yml` grep.** The programme table locates it in `.github/workflows/ci.yml`; the slice says "CI needs no new configuration". A `#[test]` in `sauron-core` runs under the `cargo test --workspace` CI already performs, needs no workflow edit, and — unlike a shell grep — is runnable locally with the same command an engineer already uses.

---

## Task 1: Migration 000034, schema and models for `mail_outbox`

**Files:**
- Create `backend/migrations/2026-08-01-000034_mail_outbox/up.sql`
- Create `backend/migrations/2026-08-01-000034_mail_outbox/down.sql`
- Modify `backend/crates/sauron-db/src/schema.rs` (append a `table!` block after the `alert_events` block at ~line 452-467; add one `joinable!` after line 501 (`diesel::joinable!(workflows -> app_environments (environment_id));`, the last one in the block); add one name to `allow_tables_to_appear_in_same_query!` at ~line 531)
- Modify `backend/crates/sauron-db/src/models.rs` (append after `NewAlertEvent`, before the `#[cfg(test)] mod tests` block at ~line 880)
- Test `backend/crates/sauron-db/tests/mail_outbox.rs` (created here, extended by Tasks 9 and 10)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - table `mail_outbox` with columns `id, kind, recipient, recipient_key, subject, body_text, body_html, status, attempts, max_attempts, next_attempt_at, expires_at, last_error, user_id, created_at, updated_at, sent_at` **in that declaration order**
  - `sauron_db::schema::mail_outbox` (diesel table)
  - `sauron_db::models::MailOutbox { id: Uuid, kind: String, recipient: String, recipient_key: String, subject: String, body_text: String, body_html: String, status: String, attempts: i32, max_attempts: i32, next_attempt_at: DateTime<Utc>, expires_at: DateTime<Utc>, last_error: Option<String>, user_id: Option<Uuid>, created_at: DateTime<Utc>, updated_at: DateTime<Utc>, sent_at: Option<DateTime<Utc>> }` deriving `Clone, Queryable, Selectable, QueryableByName` plus a hand-written `Debug`
  - `sauron_db::models::NewMailOutbox<'a> { kind: &'a str, recipient: &'a str, recipient_key: &'a str, subject: &'a str, body_text: &'a str, body_html: &'a str, user_id: Option<Uuid> }` deriving `Insertable` only

- [ ] **Step 1: Write the failing schema round-trip test.**
  Create `backend/crates/sauron-db/tests/mail_outbox.rs`:

```rust
//! The `mail_outbox` repository surface against a real Postgres.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see the module doc
//! on `tests/common/mod.rs`.

mod common;

use chrono::Utc;
use common::TestDb;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::models::MailOutbox;
use sauron_db::schema::mail_outbox;

/// Reading a row back through `schema.rs` proves the hand-maintained column
/// order matches the migration. `Queryable` decodes positionally, so a column
/// inserted in the wrong place in `schema.rs` binds `body_html` into `status`
/// and every later assertion in this file becomes meaningless.
#[tokio::test]
async fn schema_column_order_matches_the_migration() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO mail_outbox (kind, recipient, recipient_key, subject, body_text, body_html) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind::<Text, _>("password_reset")
    .bind::<Text, _>("Victim@Corp.Test")
    .bind::<Text, _>("victim@corp.test")
    .bind::<Text, _>("Reset your password")
    .bind::<Text, _>("plain body")
    .bind::<Text, _>("<p>html body</p>")
    .execute(&mut conn)
    .await
    .expect("insert");

    let row: MailOutbox = mail_outbox::table
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("select");

    assert_eq!(row.kind, "password_reset");
    assert_eq!(row.recipient, "Victim@Corp.Test");
    assert_eq!(row.recipient_key, "victim@corp.test");
    assert_eq!(row.subject, "Reset your password");
    assert_eq!(row.body_text, "plain body");
    assert_eq!(row.body_html, "<p>html body</p>");
    assert_eq!(row.status, "pending");
    assert_eq!(row.attempts, 0);
    assert_eq!(row.max_attempts, 8);
    assert!(row.last_error.is_none());
    assert!(row.user_id.is_none());
    assert!(row.sent_at.is_none());
    // The column DEFAULT is one hour. It is a backstop for a hand-written row,
    // never the policy — every enqueue passes its own.
    assert!(row.expires_at > Utc::now());

    drop(conn);
    db.cleanup().await;
}

/// A pending row's body is a live credential. One `warn!(row = ?r, ...)` in a
/// drain loop must not put a working reset URL in the journal.
#[test]
fn debug_redacts_the_body() {
    let row = MailOutbox {
        id: uuid::Uuid::nil(),
        kind: "password_reset".into(),
        recipient: "victim@corp.test".into(),
        recipient_key: "victim@corp.test".into(),
        subject: "Reset your password".into(),
        body_text: "https://sauron.test/#/reset-password?token=SECRETTOKEN".into(),
        body_html: "<a href=\"https://sauron.test/#/reset-password?token=SECRETTOKEN\">x</a>".into(),
        status: "pending".into(),
        attempts: 0,
        max_attempts: 8,
        next_attempt_at: Utc::now(),
        expires_at: Utc::now(),
        last_error: None,
        user_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        sent_at: None,
    };
    let printed = format!("{row:?}");
    assert!(printed.contains("<redacted>"), "got: {printed}");
    assert!(!printed.contains("SECRETTOKEN"), "got: {printed}");
    // The fields an operator actually needs must still be there.
    assert!(printed.contains("password_reset"));
    assert!(printed.contains("victim@corp.test"));
}

// Silence the unused-import warnings the later tasks' tests remove.
#[allow(dead_code)]
fn _unused(_: BigInt, _: SqlUuid) {}
```

  Delete the `_unused` shim and its imports at the end of Task 10 once every import is genuinely used; it exists only so this file compiles on its own with `-D warnings` before Tasks 9 and 10 land.

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test mail_outbox`
  Expected: compile error, `error[E0432]: unresolved import sauron_db::models::MailOutbox` and `unresolved import sauron_db::schema::mail_outbox`.

- [ ] **Step 3: Write `up.sql`.**
  Create `backend/migrations/2026-08-01-000034_mail_outbox/up.sql`:

```sql
-- A durable outbox for transactional email: the deployment sends a message to
-- ONE PERSON, off the request path, and can prove it happened.
--
-- Why a table at all. `sauron-alerts` already sends email, but only through a
-- per-org `notification_channels` row whose SMTP credentials the org's admin
-- owns. A password-reset link routed that way tells an arbitrary org admin that
-- one of their members asked for a reset, and strands entirely a user who
-- belongs to no org. This table is addressed to a person, so it deliberately
-- has NO org_id.
--
-- Why not a bare tokio::spawn. A spawned send dies with the process, and a lost
-- reset mail is unrecoverable for a user who has already spent their rate-limit
-- bucket. Why not Redis: `RedisStore` sets `response_timeout(None)`, so a
-- command against a dead Redis sits through reconnect for 9-19 seconds — on the
-- auth path.
--
-- `kind` deliberately has NO CHECK, deviating from the house TEXT+CHECK rule.
-- The value set keeps growing after this migration lands, and the slice that
-- adds the fifth kind must not also have to widen a CHECK on a table holding
-- live credentials. The authority is `sauron_mail::MailKind`, which also owns
-- each kind's dedup window — two things that must change together, so splitting
-- one of them into SQL guarantees drift. `status` DOES have a CHECK: this
-- migration owns every value it can take.
--
-- A pending row holds a live credential. Before this table, a read-only database
-- compromise — a backup, a replica, an SQL injection — could not take over an
-- account: password hashes are Argon2 and refresh tokens are stored hashed. A
-- `body_html` containing a working reset URL hands over accounts outright. The
-- bound on that exposure is min(delivery time, the row's own `expires_at`): the
-- body is blanked the moment the row reaches 'sent'/'sink', and a hygiene sweep
-- blanks ANY row's body once it is past `expires_at`, regardless of status.
-- Nothing recoverable is lost, because the claim query refuses an expired row
-- anyway — a body that survived that instant could never be delivered, only
-- stolen.
--
-- `expires_at` is what stops a stale message being delivered on revoked
-- authorization. EVERY enqueue sets it explicitly, from the lifetime of whatever
-- the body carries. The DEFAULT below is only a backstop for a row an operator
-- writes by hand: a reader who takes the one hour there for the real policy will
-- scrub 24-hour admin-initiated reset mail early, while its token is still live
-- and the row is still the only thing an operator can requeue.
--
-- `max_attempts` is a column, not a config knob, so an operator can bump one
-- stuck row. Combined with the fact that failing a row does NOT blank its body,
-- a failed row can be resurrected for as long as its body survives:
--   UPDATE mail_outbox SET status='pending', attempts=0, next_attempt_at=now(),
--          expires_at=now()+interval '10 minutes' WHERE id=...;
--
-- `recipient_key` is the parsed, lowercased envelope address. It exists so the
-- per-recipient cap cannot be walked around: `register` validates addresses with
-- `req.email.contains('@')` alone, and lettre's parser discards the unparsed
-- remainder, so `victim@corp.test`, `victim@corp.test ` and
-- `victim@corp.test <x>` are three `users.email` rows that deliver to one mailbox.

CREATE TABLE mail_outbox (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  kind            TEXT NOT NULL,
  recipient       TEXT NOT NULL,
  recipient_key   TEXT NOT NULL,
  subject         TEXT NOT NULL,
  body_text       TEXT NOT NULL,
  body_html       TEXT NOT NULL,
  status          TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending','sending','sent','failed','sink')),
  attempts        INT NOT NULL DEFAULT 0,
  max_attempts    INT NOT NULL DEFAULT 8,
  next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at      TIMESTAMPTZ NOT NULL DEFAULT now() + interval '1 hour',
  last_error      TEXT,
  user_id         UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  sent_at         TIMESTAMPTZ
);

-- The claim query's whole predicate, so a drain tick touches only due rows.
CREATE INDEX mail_outbox_due_idx     ON mail_outbox (next_attempt_at) WHERE status = 'pending';
-- Orphan recovery: rows a process was killed mid-send on. Nothing else ever
-- reclaims them, because the claim query only looks at 'pending'.
CREATE INDEX mail_outbox_stuck_idx   ON mail_outbox (updated_at)      WHERE status = 'sending';
-- The per-recipient suppression probe inside the enqueue INSERT.
CREATE INDEX mail_outbox_dedup_idx   ON mail_outbox (kind, recipient_key, created_at DESC);
-- The retention sweep's ORDER BY.
CREATE INDEX mail_outbox_created_idx ON mail_outbox (created_at);
```

- [ ] **Step 4: Write `down.sql`.**
  Create `backend/migrations/2026-08-01-000034_mail_outbox/down.sql`:

```sql
-- Drops only what this migration's up.sql created; DROP TABLE takes the four
-- indexes with it. (Migration 20's down.sql dropped two indexes it had not
-- created, silently destroying migration 4's. Do not repeat that here.)
DROP TABLE IF EXISTS mail_outbox;
```

- [ ] **Step 5: Hand-edit `schema.rs` — the `table!` block.**
  In `backend/crates/sauron-db/src/schema.rs`, immediately after the closing `}` of the `alert_events` `table!` block (currently ends around line 467, just before the first `diesel::joinable!` line), insert:

```rust
diesel::table! {
    mail_outbox (id) {
        id -> Uuid,
        kind -> Text,
        recipient -> Text,
        recipient_key -> Text,
        subject -> Text,
        body_text -> Text,
        body_html -> Text,
        status -> Text,
        attempts -> Int4,
        max_attempts -> Int4,
        next_attempt_at -> Timestamptz,
        expires_at -> Timestamptz,
        last_error -> Nullable<Text>,
        user_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        sent_at -> Nullable<Timestamptz>,
    }
}
```

- [ ] **Step 6: Hand-edit `schema.rs` — the joinable and the allow-list.**
  After the line `diesel::joinable!(workflows -> app_environments (environment_id));` add:

```rust
diesel::joinable!(mail_outbox -> users (user_id));
```

  and inside `diesel::allow_tables_to_appear_in_same_query!( ... );`, after the `workflows,` entry, add:

```rust
    mail_outbox,
```

- [ ] **Step 7: Add the models.**
  In `backend/crates/sauron-db/src/models.rs`, immediately after the `NewAlertEvent<'a>` struct and before `#[cfg(test)] mod tests`, insert:

```rust
// ---------------------------------------------------------------------------
// Transactional email outbox
// ---------------------------------------------------------------------------

/// One rendered, queued message.
///
/// Derives neither `Serialize` nor `Debug`, deliberately. No `Serialize`, so a
/// pending row's body cannot reach an API view struct by someone adding
/// `#[derive(Serialize)]` upstream. `QueryableByName` because the claim is a
/// `sql_query` with `RETURNING *`.
#[derive(Clone, Queryable, Selectable, QueryableByName)]
#[diesel(table_name = mail_outbox)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct MailOutbox {
    pub id: Uuid,
    pub kind: String,
    pub recipient: String,
    pub recipient_key: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: String,
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for MailOutbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A pending body is a live credential — a working password-reset URL.
        // One `warn!(row = ?r, ...)` in the drain loop would otherwise write it
        // to the journal, where it outlives the row and reaches a broader reader
        // set than the database does. Same precedent as `SecretCipher`.
        f.debug_struct("MailOutbox")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("recipient", &self.recipient)
            .field("status", &self.status)
            .field("attempts", &self.attempts)
            .field("body_text", &"<redacted>")
            .field("body_html", &"<redacted>")
            .finish()
    }
}

/// Insert side of [`MailOutbox`]. `Insertable` only — a `Queryable` derive here
/// would decode positionally against a seventeen-column table and silently bind
/// `subject` into `recipient_key`.
#[derive(Insertable)]
#[diesel(table_name = mail_outbox)]
pub struct NewMailOutbox<'a> {
    pub kind: &'a str,
    pub recipient: &'a str,
    pub recipient_key: &'a str,
    pub subject: &'a str,
    pub body_text: &'a str,
    pub body_html: &'a str,
    pub user_id: Option<Uuid>,
}
```

- [ ] **Step 8: Apply the migration to the dev database.**
  `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`
  Expected: it prints that it ran `2026-08-01-000034_mail_outbox` and exits 0.

- [ ] **Step 9: Run the test and watch it pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test mail_outbox`
  Expected: `test result: ok. 2 passed`. If `schema_column_order_matches_the_migration` reports "skipping", `TEST_DATABASE_URL` was not exported — the assertion did not run.

- [ ] **Step 10: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 2: `SmtpSettings`, `SmtpTls` and the pure `build_smtp`

**Files:**
- Modify `backend/crates/sauron-core/src/config.rs` (insert the two new types and `build_smtp` after the `parse` helper at line 108-110, before `impl Config`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum SmtpTls { Implicit, StartTls, None }` deriving `Debug, Clone, Copy, PartialEq, Eq`
  - `pub struct SmtpSettings { pub host: String, pub port: u16, pub username: Option<String>, pub password: Option<String>, pub from_address: String, pub from_name: String, pub tls: SmtpTls, pub allow_private: bool, pub timeout_ms: u64, pub sink: bool }` deriving `Clone` plus a hand-written redacting `Debug`
  - `pub fn build_smtp(host: Option<String>, port: u16, username: Option<String>, password: Option<String>, from_address: Option<String>, from_name: String, tls_raw: Option<String>, allow_private: bool, timeout_ms: u64, sink: bool) -> Result<SmtpSettings, String>`

Both types live in `sauron-core` and nowhere else. `sauron-mail` depends on `sauron-core`, so defining them there too would be a second, incompatible type — and `Config` cannot depend on `sauron-mail` without a cycle.

- [ ] **Step 1: Write the failing truth-table test.**
  Append to `backend/crates/sauron-core/src/config.rs` a `#[cfg(test)] mod tests` block at the very end of the file (the file has none today):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// `build_smtp` takes already-read values rather than reading env itself:
    /// env vars are process-global and `cargo test` runs tests in threads, so a
    /// test that sets `SMTP_HOST` races every other test in the binary.
    #[allow(clippy::too_many_arguments)]
    fn call(
        host: Option<&str>,
        port: u16,
        from: Option<&str>,
        tls_raw: Option<&str>,
        timeout_ms: u64,
        sink: bool,
    ) -> Result<SmtpSettings, String> {
        build_smtp(
            host.map(|s| s.to_string()),
            port,
            None,
            None,
            from.map(|s| s.to_string()),
            "Sauron".to_string(),
            tls_raw.map(|s| s.to_string()),
            false,
            timeout_ms,
            sink,
        )
    }

    #[test]
    fn unset_host_disables_mail_and_names_the_variable() {
        let err = call(None, 587, None, None, 10_000, false).unwrap_err();
        assert!(err.contains("SMTP_HOST"), "got: {err}");
        assert!(err.contains("SMTP_SINK"), "got: {err}");
    }

    #[test]
    fn host_without_from_names_the_missing_variable() {
        let err = call(Some("smtp.example.test"), 587, None, None, 10_000, false).unwrap_err();
        assert!(err.contains("SMTP_FROM"), "got: {err}");
    }

    #[test]
    fn from_is_shape_checked_at_boot_not_at_send() {
        for bad in ["nobody", "a@b@c", "a b@c.test", "@c.test", "a@", "a@c\r\nBcc: x@y"] {
            let err = call(Some("smtp.example.test"), 587, Some(bad), None, 10_000, false)
                .unwrap_err();
            assert!(err.contains("SMTP_FROM"), "{bad} gave: {err}");
        }
        assert!(call(Some("smtp.example.test"), 587, Some("a@c.test"), None, 10_000, false).is_ok());
    }

    #[test]
    fn tls_defaults_follow_the_port_the_way_channel_resolution_does() {
        let s = call(Some("smtp.example.test"), 465, Some("a@c.test"), None, 10_000, false).unwrap();
        assert_eq!(s.tls, SmtpTls::Implicit);
        let s = call(Some("smtp.example.test"), 587, Some("a@c.test"), None, 10_000, false).unwrap();
        assert_eq!(s.tls, SmtpTls::StartTls);
    }

    #[test]
    fn tls_aliases_parse_and_garbage_lists_the_accepted_values() {
        for (raw, want) in [
            ("implicit", SmtpTls::Implicit),
            ("smtps", SmtpTls::Implicit),
            ("starttls", SmtpTls::StartTls),
            ("required", SmtpTls::StartTls),
            ("STARTTLS", SmtpTls::StartTls),
        ] {
            let s = call(Some("smtp.example.test"), 587, Some("a@c.test"), Some(raw), 10_000, false)
                .unwrap();
            assert_eq!(s.tls, want, "raw={raw}");
        }
        let err = call(
            Some("smtp.example.test"),
            587,
            Some("a@c.test"),
            Some("garbage"),
            10_000,
            false,
        )
        .unwrap_err();
        assert!(err.contains("implicit"), "got: {err}");
        assert!(err.contains("starttls"), "got: {err}");
        assert!(err.contains("none"), "got: {err}");
    }

    #[test]
    fn cleartext_is_refused_at_boot_unless_the_relay_is_loopback() {
        let err = call(
            Some("192.168.1.20"),
            25,
            Some("a@c.test"),
            Some("none"),
            10_000,
            false,
        )
        .unwrap_err();
        assert!(err.contains("SMTP_TLS"), "got: {err}");
        assert!(err.contains("192.168.1.20"), "got: {err}");

        for ok_host in ["localhost", "127.0.0.1", "::1", "[::1]"] {
            let s = call(Some(ok_host), 25, Some("a@c.test"), Some("none"), 10_000, false)
                .unwrap_or_else(|e| panic!("{ok_host} rejected: {e}"));
            assert_eq!(s.tls, SmtpTls::None);
        }
    }

    #[test]
    fn timeout_clamps_the_same_way_the_alert_engine_does() {
        let s = call(Some("smtp.example.test"), 587, Some("a@c.test"), None, 10, false).unwrap();
        assert_eq!(s.timeout_ms, 1_000);
        let s =
            call(Some("smtp.example.test"), 587, Some("a@c.test"), None, 900_000, false).unwrap();
        assert_eq!(s.timeout_ms, 60_000);
    }

    #[test]
    fn sink_without_a_host_is_a_working_configuration() {
        let s = call(None, 587, None, None, 10_000, true).unwrap();
        assert!(s.sink);
        assert_eq!(s.host, "(sink)");
        assert_eq!(s.from_address, "sauron@localhost");
        let s = call(None, 587, Some("noreply@corp.test"), None, 10_000, true).unwrap();
        assert_eq!(s.from_address, "noreply@corp.test");
    }

    #[test]
    fn smtp_settings_debug_redacts_the_password() {
        let s = build_smtp(
            Some("smtp.example.test".into()),
            587,
            Some("mailer".into()),
            Some("hunter2".into()),
            Some("a@c.test".into()),
            "Sauron".into(),
            None,
            false,
            10_000,
            false,
        )
        .unwrap();
        let printed = format!("{s:?}");
        assert!(printed.contains("<redacted>"), "got: {printed}");
        assert!(!printed.contains("hunter2"), "got: {printed}");
        // The username is not a secret and stays legible.
        assert!(printed.contains("mailer"), "got: {printed}");
    }
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-core build_smtp`
  Expected: `error[E0425]: cannot find function build_smtp in this scope` and `cannot find type SmtpSettings in this scope`.

- [ ] **Step 3: Add `SmtpTls` and `SmtpSettings`.**
  In `backend/crates/sauron-core/src/config.rs`, immediately after the `parse` helper (line 108-110) and before `impl Config`, insert:

```rust
/// How the SMTP connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    /// Implicit TLS (SMTPS): handshake immediately, usually :465.
    Implicit,
    /// STARTTLS, and abort if the server will not upgrade. Usually :587.
    StartTls,
    /// Cleartext. Only ever accepted for a relay on this host — see
    /// [`build_smtp`] rule 6 and the matching structural check at connect time.
    None,
}

/// Deployment-level SMTP relay. Distinct from the per-org SMTP credentials in
/// `notification_channels`: this one carries mail addressed to a *person*, which
/// is why it must exist even for a user who belongs to no org.
#[derive(Clone)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from_address: String,
    pub from_name: String,
    pub tls: SmtpTls,
    pub allow_private: bool,
    pub timeout_ms: u64,
    pub sink: bool,
}

impl std::fmt::Debug for SmtpSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // This struct is reachable from `Config`, and `Config` is the thing an
        // engineer reaches for with `debug!("{cfg:?}")` during an incident. A
        // `#[derive(Debug)]` here would put the relay password in the journal and
        // clippy would not say a word.
        f.debug_struct("SmtpSettings")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("from_address", &self.from_address)
            .field("from_name", &self.from_name)
            .field("tls", &self.tls)
            .field("allow_private", &self.allow_private)
            .field("timeout_ms", &self.timeout_ms)
            .field("sink", &self.sink)
            .finish()
    }
}
```

- [ ] **Step 4: Add `build_smtp`.**
  Directly below the `SmtpSettings` `Debug` impl, insert:

```rust
/// Hosts for which `SMTP_TLS=none` is accepted. Cleartext SMTP puts the relay
/// password and every password-reset link on the wire; the only topology where
/// that is defensible is a relay listening on this machine.
const SMTP_LOOPBACK_HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "::1", "[::1]"];

/// Validate the SMTP settings without reading the environment.
///
/// Takes already-read values on purpose: env vars are process-global and
/// `cargo test` runs a binary's tests on threads, so a `build_smtp` that read
/// `std::env` could not be tested without racing every other test in the crate.
///
/// Returns `Err(reason)` rather than panicking or bailing. `Config::from_env` is
/// shared by every binary; a `bail!` here would take down `sauron-ingest` and
/// `sauron-tier` over a relay setting they never read — which is exactly what
/// happened to `jwt_secret` and is why that field is a recorded `Result` too.
#[allow(clippy::too_many_arguments)]
pub fn build_smtp(
    host: Option<String>,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    from_address: Option<String>,
    from_name: String,
    tls_raw: Option<String>,
    allow_private: bool,
    timeout_ms: u64,
    sink: bool,
) -> Result<SmtpSettings, String> {
    // 1. The dev sink needs no relay at all, so it must not be blocked by the
    //    host/from rules below. A developer with SMTP_SINK=1 and nothing else set
    //    still exercises every template, every enqueue and the whole outbox state
    //    machine.
    if sink && host.is_none() {
        return Ok(SmtpSettings {
            host: "(sink)".to_string(),
            port,
            username,
            password,
            from_address: from_address.unwrap_or_else(|| "sauron@localhost".to_string()),
            from_name,
            tls: SmtpTls::StartTls,
            allow_private,
            timeout_ms: timeout_ms.clamp(1_000, 60_000),
            sink: true,
        });
    }

    // 2. No relay configured. This is the ordinary state of a deployment that has
    //    not enabled transactional email, so the message tells an operator what to
    //    set rather than reading as a fault.
    let host = host.ok_or_else(|| {
        "SMTP_HOST is not set; transactional email is disabled. Set SMTP_HOST/SMTP_FROM, \
         or SMTP_SINK=1 to log mail instead of sending it."
            .to_string()
    })?;

    // 3-4. A From address that lettre will reject at send time is a message that
    //      fails eight times in a retry loop and reaches nobody. Catch the obvious
    //      shapes at boot, where a human is looking. Real parsing still happens in
    //      lettre.
    let from_address =
        from_address.ok_or_else(|| "SMTP_FROM is required when SMTP_HOST is set".to_string())?;
    let at_count = from_address.matches('@').count();
    let (local, domain) = from_address.split_once('@').unwrap_or(("", ""));
    if at_count != 1
        || local.is_empty()
        || domain.is_empty()
        || from_address.chars().any(|c| c.is_whitespace())
    {
        return Err(format!(
            "SMTP_FROM must be a bare address with exactly one '@' and no whitespace, \
             e.g. sauron@example.com (got {from_address:?})"
        ));
    }

    // 5. Unset follows the port, the same rule notification-channel resolution
    //    uses for `implicit_tls`.
    let tls = match tls_raw.as_deref().map(str::trim) {
        None | Some("") => {
            if port == 465 {
                SmtpTls::Implicit
            } else {
                SmtpTls::StartTls
            }
        }
        Some(v) if v.eq_ignore_ascii_case("implicit") || v.eq_ignore_ascii_case("smtps") => {
            SmtpTls::Implicit
        }
        Some(v) if v.eq_ignore_ascii_case("starttls") || v.eq_ignore_ascii_case("required") => {
            SmtpTls::StartTls
        }
        Some(v) if v.eq_ignore_ascii_case("none") || v.eq_ignore_ascii_case("plain") => {
            SmtpTls::None
        }
        Some(other) => {
            return Err(format!(
                "SMTP_TLS={other:?} is not recognised; accepted values are \
                 implicit (or smtps), starttls (or required), none (or plain)"
            ))
        }
    };

    // 6. The syntactic half of the loopback rule. The structural half runs against
    //    the RESOLVED address inside `SmtpClient::connect`, which is what survives
    //    a `localhost` that has been pointed somewhere else. Both exist: this one
    //    is loud and early, that one is true.
    //
    //    Deliberately NOT gated on SMTP_ALLOW_PRIVATE. That flag would then be the
    //    only consent gate for shipping reset links across a LAN, and it is a flag
    //    an operator may have set for an unrelated internal webhook.
    if tls == SmtpTls::None && !SMTP_LOOPBACK_HOSTS.contains(&host.as_str()) {
        return Err(format!(
            "SMTP_TLS=none sends the SMTP password and password-reset links in cleartext \
             and is only accepted for a relay on this host; SMTP_HOST={host} is not loopback"
        ));
    }

    Ok(SmtpSettings {
        host,
        port,
        username,
        password,
        from_address,
        from_name,
        tls,
        allow_private,
        // 7. Same bounds `AlertEngine::new` applies, so the two delivery paths
        //    cannot be tuned into disagreement.
        timeout_ms: timeout_ms.clamp(1_000, 60_000),
        sink,
    })
}
```

- [ ] **Step 5: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-core config::tests`
  Expected: `test result: ok. 9 passed`.

- [ ] **Step 6: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 3: `Config` fields, `require_*` accessors, and a redacting `Debug`

**Files:**
- Modify `backend/crates/sauron-core/src/config.rs` (struct fields ~line 10-95; `#[derive(Debug, Clone)]` at line 9; `impl Config` at line 112; `from_env` body at line 126-220; the `#[cfg(test)] mod tests` block added in Task 2)

**Interfaces:**
- Consumes: `build_smtp`, `SmtpSettings`, `SmtpTls` (Task 2).
- Produces:
  - `Config.dev_mode: bool` (public — S1 consumes it; today `SAURON_DEV` is a throwaway local at line 143)
  - `Config.mail_drain_tick_secs: u64`, `Config.mail_outbox_retention_days: i64` (public)
  - `pub fn require_smtp(&self) -> anyhow::Result<&SmtpSettings>`
  - `pub fn require_dashboard_url(&self) -> anyhow::Result<&str>`
  - a hand-written `impl Debug for Config`

- [ ] **Step 1: Write the failing accessor + redaction tests.**
  Inside the `#[cfg(test)] mod tests` block created in Task 2, append:

```rust
    /// `from_env` must never bail on a missing relay or dashboard URL. Bailing in
    /// `from_env` once took down `sauron-ingest` and `sauron-tier`, which read
    /// neither. The failure is recorded and raised at the point of use.
    #[test]
    fn config_records_rather_than_raises_missing_mail_settings() {
        let cfg = Config {
            smtp: Err("no relay".to_string()),
            dashboard_url: Err("no dashboard url".to_string()),
            ..sample_config()
        };
        assert!(cfg.require_smtp().is_err());
        assert!(cfg.require_dashboard_url().is_err());
    }

    #[test]
    fn require_accessors_hand_back_the_configured_values() {
        let settings = build_smtp(
            Some("smtp.example.test".into()),
            587,
            None,
            None,
            Some("a@c.test".into()),
            "Sauron".into(),
            None,
            false,
            10_000,
            false,
        )
        .unwrap();
        let cfg = Config {
            smtp: Ok(settings),
            dashboard_url: Ok("https://sauron.example.test".to_string()),
            ..sample_config()
        };
        assert_eq!(cfg.require_smtp().unwrap().host, "smtp.example.test");
        assert_eq!(
            cfg.require_dashboard_url().unwrap(),
            "https://sauron.example.test"
        );
    }

    /// A single `debug!(?cfg)` added during an incident must not dump the
    /// Postgres password, the JWT signing key and the SMTP password at once.
    #[test]
    fn config_debug_redacts_every_secret_it_holds() {
        let settings = build_smtp(
            Some("smtp.example.test".into()),
            587,
            Some("mailer".into()),
            Some("smtp-hunter2".into()),
            Some("a@c.test".into()),
            "Sauron".into(),
            None,
            false,
            10_000,
            false,
        )
        .unwrap();
        let cfg = Config {
            database_url: "postgres://sauron:pg-hunter2@db/sauron".to_string(),
            redis_url: "redis://:redis-hunter2@cache:6379".to_string(),
            jwt_secret: Ok("jwt-hunter2-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            notify_secret_key: Some("notify-hunter2".to_string()),
            symbols_redis_url: Some("redis://:symbols-hunter2@cache:6379/1".to_string()),
            smtp: Ok(settings),
            ..sample_config()
        };
        let printed = format!("{cfg:?}");
        for secret in [
            "pg-hunter2",
            "redis-hunter2",
            "jwt-hunter2",
            "notify-hunter2",
            "symbols-hunter2",
            "smtp-hunter2",
        ] {
            assert!(!printed.contains(secret), "{secret} leaked into: {printed}");
        }
        assert!(printed.contains("<redacted>"), "got: {printed}");
        // Non-secret fields must still be legible, or the impl is useless.
        assert!(printed.contains("api_port"), "got: {printed}");
    }
```

  and, still inside `mod tests`, add the constructor those three share:

```rust
    /// A `Config` with every field at a harmless value, so a test can override
    /// exactly the two or three it cares about with struct-update syntax.
    fn sample_config() -> Config {
        Config {
            database_url: "postgres://localhost/sauron".to_string(),
            redis_url: "redis://localhost:6379".to_string(),
            ingest_port: 8081,
            api_port: 8080,
            jwt_secret: Err("unset".to_string()),
            jwt_access_ttl_secs: 900,
            jwt_refresh_ttl_secs: 2_592_000,
            dev_mode: false,
            worker_concurrency: 4,
            cors_allowed_origins: vec![],
            ingest_rate_limit_per_min: 6000,
            ingest_max_body_bytes: 1_048_576,
            ingest_uds_path: None,
            ingest_backlog: 4096,
            ingest_trust_forwarded_headers: false,
            api_trust_forwarded_headers: false,
            monitor_tick_ms: 1000,
            monitor_batch: 100,
            monitor_max_concurrency: 50,
            monitor_check_retention_days: 30,
            monitor_ssrf_allow_private: false,
            tier_hot_days: 30,
            tier_granularity: "day".to_string(),
            tier_cold_path: "/var/lib/sauron/cold".to_string(),
            tier_drop_lag_hours: 24,
            tier_tick_secs: 3600,
            tier_partition_ahead: 7,
            search_scan_clamp_days: 30,
            symbols_cache_mb: 256,
            symbols_redis_url: None,
            symbols_redis_max_blob_mb: 8,
            symbols_max_artifact_mb: 128,
            symbols_max_uncompressed_mb: 512,
            symbols_ingest_timeout_ms: 150,
            notify_secret_key: None,
            alerts_tick_secs: 30,
            alerts_deliver_timeout_ms: 10_000,
            alerts_allow_private: false,
            alert_event_retention_days: 90,
            smtp: Err("unset".to_string()),
            dashboard_url: Err("unset".to_string()),
            mail_drain_tick_secs: 60,
            mail_outbox_retention_days: 30,
        }
    }
```

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-core config::tests`
  Expected: `error[E0560]: struct Config has no field named dev_mode` (and the same for `smtp`, `dashboard_url`, `mail_drain_tick_secs`, `mail_outbox_retention_days`), plus `no method named require_smtp`.

- [ ] **Step 3: Add the fields.**
  In `backend/crates/sauron-core/src/config.rs`, change line 9 from `#[derive(Debug, Clone)]` to `#[derive(Clone)]` (the hand-written `Debug` comes in Step 6), then add these fields to `pub struct Config` — `dev_mode` immediately after `jwt_refresh_ttl_secs`, the rest at the end of the struct just before the closing brace:

```rust
    /// `SAURON_DEV=1`. Today this only relaxes the `JWT_SECRET` rule, but it is
    /// promoted to a field because it is also the second half of the dev-sink
    /// body-logging gate, and S1 reads it. A local that three places need is a
    /// field.
    pub dev_mode: bool,
```

```rust
    // --- transactional email ---
    /// The deployment-level relay, or the reason there isn't a usable one.
    ///
    /// Private on purpose: reach it through [`Config::require_smtp`]. `from_env`
    /// must not bail, because `sauron-ingest` and `sauron-tier` read this same
    /// struct and never read this field.
    smtp: Result<SmtpSettings, String>,
    /// The browser-facing origin of the dashboard SPA, or the reason there isn't
    /// one. In the shipped nginx topology this is NOT the API's origin — nginx
    /// serves the SPA and does not proxy the API — so nothing can derive it.
    ///
    /// Private on purpose: reach it through [`Config::require_dashboard_url`].
    dashboard_url: Result<String, String>,
    /// How often `sauron-api` drains `mail_outbox`.
    pub mail_drain_tick_secs: u64,
    /// How long terminal (`sent`/`failed`/`sink`) outbox rows are kept.
    pub mail_outbox_retention_days: i64,
```

- [ ] **Step 4: Add the accessors.**
  In `impl Config`, immediately after `require_jwt_secret`, insert:

```rust
    /// The configured SMTP relay, or an error explaining why there isn't one.
    ///
    /// Fails closed at the point of use. Callers must degrade rather than refuse
    /// to boot: a deployment with no relay has to serve everything else.
    pub fn require_smtp(&self) -> anyhow::Result<&SmtpSettings> {
        match &self.smtp {
            Ok(s) => Ok(s),
            Err(reason) => anyhow::bail!("{reason}"),
        }
    }

    /// The dashboard's browser-facing origin, trailing slashes already stripped.
    ///
    /// This is what makes "any email containing a link requires DASHBOARD_URL"
    /// enforceable: `sauron_mail::Branding::link` refuses to build a URL without
    /// it, rather than guessing an origin and sending a link to nowhere that
    /// every server-side signal reports as delivered.
    pub fn require_dashboard_url(&self) -> anyhow::Result<&str> {
        match &self.dashboard_url {
            Ok(s) => Ok(s.as_str()),
            Err(reason) => anyhow::bail!("{reason}"),
        }
    }
```

- [ ] **Step 5: Populate the fields in `from_env`.**
  In `from_env`, leave the existing `let dev_mode = ...` binding at lines 143-145 exactly as it is — it needs no change, it is already computed before `jwt_secret` reads it, and the struct-literal addition later in this step just moves it into the new field. Add, just after the `cors_allowed_origins` block (lines 161-166):

```rust
        // The SPA origin. Validated here so a typo is a boot-time message rather
        // than a message that renders, sends, reaches 'sent', and lands in a
        // mailbox with a link to nowhere.
        let dashboard_url = match var("DASHBOARD_URL") {
            None => Err("DASHBOARD_URL is not set; any email containing a link cannot be \
                         rendered. Set it to the browser-facing origin of the dashboard, \
                         e.g. https://sauron.example.com"
                .to_string()),
            Some(u) if u.starts_with("http://") || u.starts_with("https://") => {
                Ok(u.trim_end_matches('/').to_string())
            }
            Some(u) => Err(format!(
                "DASHBOARD_URL must start with http:// or https:// (got {u:?})"
            )),
        };

        let smtp = build_smtp(
            var("SMTP_HOST"),
            parse("SMTP_PORT", 587u16),
            var("SMTP_USERNAME"),
            var("SMTP_PASSWORD"),
            var("SMTP_FROM"),
            var("SMTP_FROM_NAME").unwrap_or_else(|| "Sauron".to_string()),
            var("SMTP_TLS"),
            // Deliberately NOT inheriting ALERTS_ALLOW_PRIVATE: that flag unlocks
            // private delivery for USER-SUPPLIED webhook URLs, a strictly larger
            // surface. Declaring a LAN Slack endpoint is not declaring anything
            // about the relay.
            var("SMTP_ALLOW_PRIVATE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            parse("SMTP_TIMEOUT_MS", 10_000u64),
            // Deliberately NOT inheriting SAURON_DEV: that variable exists to get
            // past a JWT_SECRET complaint, and an operator who sets it during a
            // stalled first boot must not thereby convert every reset link into a
            // log line.
            var("SMTP_SINK")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        );
```

  and add to the `Ok(Self { ... })` literal, after `alerts_allow_private`:

```rust
            dev_mode,
            smtp,
            dashboard_url,
            mail_drain_tick_secs: parse::<u64>("MAIL_DRAIN_TICK_SECS", 60).clamp(10, 3600),
            mail_outbox_retention_days: parse("MAIL_OUTBOX_RETENTION_DAYS", 30),
```

- [ ] **Step 6: Add the hand-written `Debug`.**
  Immediately after the closing brace of `pub struct Config { ... }`, insert:

```rust
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hand-written rather than derived. Nothing prints `Config` today, so
        // this is a latent leak — but S0 adds the most tempting one, and a single
        // `debug!(?cfg)` typed during an incident would otherwise dump the
        // Postgres password, the JWT signing key and the SMTP password into the
        // journal at once, where they outlive the process and reach a wider
        // reader set than the database does.
        //
        // A field added later and forgotten here simply does not print. That is
        // the safe direction to fail.
        const R: &str = "<redacted>";
        f.debug_struct("Config")
            .field("database_url", &R)
            .field("redis_url", &R)
            .field("ingest_port", &self.ingest_port)
            .field("api_port", &self.api_port)
            .field("jwt_secret", &self.jwt_secret.as_ref().map(|_| R))
            .field("jwt_access_ttl_secs", &self.jwt_access_ttl_secs)
            .field("jwt_refresh_ttl_secs", &self.jwt_refresh_ttl_secs)
            .field("dev_mode", &self.dev_mode)
            .field("worker_concurrency", &self.worker_concurrency)
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field("ingest_rate_limit_per_min", &self.ingest_rate_limit_per_min)
            .field("ingest_max_body_bytes", &self.ingest_max_body_bytes)
            .field("ingest_uds_path", &self.ingest_uds_path)
            .field("ingest_backlog", &self.ingest_backlog)
            .field(
                "ingest_trust_forwarded_headers",
                &self.ingest_trust_forwarded_headers,
            )
            .field(
                "api_trust_forwarded_headers",
                &self.api_trust_forwarded_headers,
            )
            .field("monitor_tick_ms", &self.monitor_tick_ms)
            .field("monitor_batch", &self.monitor_batch)
            .field("monitor_max_concurrency", &self.monitor_max_concurrency)
            .field(
                "monitor_check_retention_days",
                &self.monitor_check_retention_days,
            )
            .field("monitor_ssrf_allow_private", &self.monitor_ssrf_allow_private)
            .field("tier_hot_days", &self.tier_hot_days)
            .field("tier_granularity", &self.tier_granularity)
            .field("tier_cold_path", &self.tier_cold_path)
            .field("tier_drop_lag_hours", &self.tier_drop_lag_hours)
            .field("tier_tick_secs", &self.tier_tick_secs)
            .field("tier_partition_ahead", &self.tier_partition_ahead)
            .field("search_scan_clamp_days", &self.search_scan_clamp_days)
            .field("symbols_cache_mb", &self.symbols_cache_mb)
            .field("symbols_redis_url", &self.symbols_redis_url.as_ref().map(|_| R))
            .field("symbols_redis_max_blob_mb", &self.symbols_redis_max_blob_mb)
            .field("symbols_max_artifact_mb", &self.symbols_max_artifact_mb)
            .field(
                "symbols_max_uncompressed_mb",
                &self.symbols_max_uncompressed_mb,
            )
            .field("symbols_ingest_timeout_ms", &self.symbols_ingest_timeout_ms)
            .field("notify_secret_key", &self.notify_secret_key.as_ref().map(|_| R))
            .field("alerts_tick_secs", &self.alerts_tick_secs)
            .field("alerts_deliver_timeout_ms", &self.alerts_deliver_timeout_ms)
            .field("alerts_allow_private", &self.alerts_allow_private)
            .field(
                "alert_event_retention_days",
                &self.alert_event_retention_days,
            )
            .field("smtp", &self.smtp.as_ref().map(|_| R))
            .field("dashboard_url", &self.dashboard_url)
            .field("mail_drain_tick_secs", &self.mail_drain_tick_secs)
            .field("mail_outbox_retention_days", &self.mail_outbox_retention_days)
            .finish()
    }
}
```

- [ ] **Step 7: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-core config::tests`
  Expected: `test result: ok. 12 passed`.

- [ ] **Step 8: Confirm nothing else in the workspace relied on `Config: Debug` being derived.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`
  Expected: exit 0. (The hand-written impl keeps `Config: Debug` satisfied, so any `#[derive(Debug)]` struct embedding it still compiles.)

- [ ] **Step 9: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 4: The `sauron-mail` crate and the moved text primitives

**Files:**
- Create `backend/crates/sauron-mail/Cargo.toml`
- Create `backend/crates/sauron-mail/src/lib.rs`
- Create `backend/crates/sauron-mail/src/text.rs`
- Modify `backend/Cargo.toml` (the `# --- internal crates ---` block, after `sauron-query`)
- Modify `backend/crates/sauron-alerts/Cargo.toml` (add `sauron-mail`; `lettre` is removed in Task 8, not here — `deliver.rs` still uses it until then)
- Modify `backend/crates/sauron-alerts/src/render.rs` (delete `substitute` at lines 106-131 and `html_escape` at lines 133-138; delete the two moved tests at lines 251-273; add two `use` lines)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - crate `sauron-mail`, picked up automatically by `members = ["crates/*", "bins/*"]`
  - `sauron_mail::text::substitute(template: &str, vars: &BTreeMap<String, String>) -> String`
  - `sauron_mail::text::html_escape(s: &str) -> String`
  - `sauron_mail::{substitute, html_escape}` (re-exports)
  - `sauron_alerts::render::substitute` stays a public path (`pub use`), so `AlertContext::message` and every existing caller are untouched

- [ ] **Step 1: Create the crate manifest.**
  Create `backend/crates/sauron-mail/Cargo.toml`:

```toml
[package]
name = "sauron-mail"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
# Deliberately NOT sauron-db. Keeping the data layer out is what lets this stay a
# leaf crate anything can link — including a future digest worker that must not
# drag the alerting engine in to send one message.
sauron-core = { workspace = true }
sauron-monitor-core = { workspace = true }

lettre = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
```

- [ ] **Step 2: Register the crate in the workspace.**
  In `backend/Cargo.toml`, in the `# --- internal crates ---` block, after the `sauron-query` line, add:

```toml
sauron-mail = { path = "crates/sauron-mail" }
```

- [ ] **Step 3: Add the dependency to `sauron-alerts`.**
  In `backend/crates/sauron-alerts/Cargo.toml`, after `sauron-monitor-core = { workspace = true }`, add:

```toml
sauron-mail = { workspace = true }
```

- [ ] **Step 4: Write `text.rs` with the moved functions and their moved tests.**
  Create `backend/crates/sauron-mail/src/text.rs`:

```rust
//! The two string primitives every message body goes through.
//!
//! These moved here verbatim from `sauron_alerts::render`, tests included, so the
//! move is provably behaviour-preserving. They are `pub` here and were not there:
//! `html_escape` was a private `fn`, which is why an earlier plan for password
//! reset proposed widening it in a file this slice removes the code from.

use std::collections::BTreeMap;

/// Replace `{{key}}` occurrences with `vars[key]`. Unknown keys are left blank
/// (not echoed) so a template can't leak the literal placeholder. Whitespace
/// inside the braces is tolerated: `{{ key }}`.
///
/// This copies bytes and escapes NOTHING. Every value handed to it for an HTML
/// template must already be escaped by the caller.
pub fn substitute(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        // Everything before the placeholder is copied verbatim. Slicing on the
        // byte index returned by `find` is UTF-8 safe because `{{` is ASCII, so
        // the index always lands on a char boundary.
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        match after.find("}}") {
            Some(close) => {
                if let Some(val) = vars.get(after[..close].trim()) {
                    out.push_str(val);
                }
                rest = &after[close + 2..];
            }
            // Unterminated `{{` — emit it literally and stop scanning.
            None => {
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Escape the four characters that break out of HTML text and double-quoted
/// attribute values.
///
/// It does NOT escape `'`. That is safe only because every attribute in the
/// house email layout is double-quoted — a property `LAYOUT_HTML`'s doc comment
/// states out loud rather than leaving as tribal knowledge.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_replaces_known_and_blanks_unknown() {
        let mut vars = BTreeMap::new();
        vars.insert("name".to_string(), "api".to_string());
        assert_eq!(substitute("hi {{name}}!", &vars), "hi api!");
        assert_eq!(substitute("{{ name }} up", &vars), "api up");
        assert_eq!(substitute("x {{missing}} y", &vars), "x  y");
        assert_eq!(substitute("no braces", &vars), "no braces");
        // Unterminated braces are passed through literally.
        assert_eq!(substitute("{{oops", &vars), "{{oops");
    }

    #[test]
    fn substitute_preserves_multibyte_text() {
        let mut vars = BTreeMap::new();
        vars.insert("svc".to_string(), "café".to_string());
        // Non-ASCII on both sides of the placeholder and in the value.
        assert_eq!(
            substitute("héllo {{svc}} — naïve ✅", &vars),
            "héllo café — naïve ✅"
        );
        assert_eq!(substitute("日本語のみ", &vars), "日本語のみ");
    }

    #[test]
    fn html_escape_covers_exactly_four_characters() {
        assert_eq!(
            html_escape("<script>alert(1)</script>&\""),
            "&lt;script&gt;alert(1)&lt;/script&gt;&amp;&quot;"
        );
        // The single quote is deliberately NOT escaped; the layout compensates by
        // double-quoting every attribute. Pinning it here means a future change to
        // the escape set is a deliberate act with a failing test attached.
        assert_eq!(html_escape("it's"), "it's");
    }
}
```

- [ ] **Step 5: Write `lib.rs` with only `text` wired up for now.**
  Create `backend/crates/sauron-mail/src/lib.rs`:

```rust
//! `sauron-mail` — compose and transmit one message.
//!
//! This crate knows how to compose and transmit a message. It does **not** know
//! what a user is, where a message queues, or when to retry. Those live in
//! `sauron-db` (`mail_outbox` + its repository functions) and in
//! `sauron-api`'s `mail.rs`, and that split is why this crate can stay a leaf
//! with no data-layer dependency.
//!
//! **The outbox is this codebase's async side-effect primitive, not a mail
//! detail.** `mail_outbox` plus its claim/drain/backoff/reap loop is the first
//! durable, restart-surviving, observable deferred-work mechanism here. Anything
//! that wants "do this after the response" should enqueue rather than
//! `tokio::spawn` a detached network call: a spawn dies with the process, has no
//! backoff, has no bound on concurrency under a burst, and cannot be observed by
//! an integration test.

pub mod text;

pub use text::{html_escape, substitute};
```

- [ ] **Step 6: Run the moved tests in their new home.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-mail`
  Expected: `test result: ok. 3 passed`.

- [ ] **Step 7: Delete the originals from `sauron-alerts` and re-point the module.**
  In `backend/crates/sauron-alerts/src/render.rs`:
  - Delete the whole `pub fn substitute(...)` body (lines 103-131 including its doc comment) and the whole `fn html_escape(...)` (lines 133-138).
  - In their place, insert:

```rust
/// Re-exported so `sauron_alerts::render::substitute` stays a working public
/// path: `AlertContext::message` and admin-authored channel templates both go
/// through it, and moving the definition must not move the name.
pub use sauron_mail::text::substitute;

use sauron_mail::text::html_escape;
```

  - Delete the two moved tests from the `#[cfg(test)] mod tests` block:
    `substitute_replaces_known_and_blanks_unknown` and `substitute_preserves_multibyte_text`.
    **Keep** `matrix_html_escapes_user_content` — it exercises `matrix_content`, which stays in this file, and it is the regression that proves the re-pointed `html_escape` is still wired in.

- [ ] **Step 8: Run the alerts tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts`
  Expected: `test result: ok`, with `render::tests::matrix_html_escapes_user_content`, `render::tests::message_uses_template_then_summary` and `render::tests::slack_and_discord_shapes` all passing. If `substitute` reports as unresolved, Step 7 deleted the `pub use` line as well as the function.

- [ ] **Step 9: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 5: `MailKind`

**Files:**
- Create `backend/crates/sauron-mail/src/kind.rs`
- Modify `backend/crates/sauron-mail/src/lib.rs` (add `pub mod kind;` and the re-export)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum MailKind { PasswordReset, NotificationDigest, PersonalNotification, SmtpTest }` deriving `Debug, Clone, Copy, PartialEq, Eq`
  - `MailKind::as_str(&self) -> &'static str`
  - `MailKind::dedup_window(&self) -> std::time::Duration`

- [ ] **Step 1: Write the failing test.**
  Create `backend/crates/sauron-mail/src/kind.rs` containing only the test module (the implementation lands in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn wire_strings_are_stable_and_distinct() {
        let all = [
            MailKind::PasswordReset,
            MailKind::NotificationDigest,
            MailKind::PersonalNotification,
            MailKind::SmtpTest,
        ];
        let names: Vec<&str> = all.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "password_reset",
                "notification_digest",
                "personal_notification",
                "smtp_test",
            ]
        );
        // These strings are written into `mail_outbox.kind`, which has no CHECK.
        // Nothing in the database will notice a collision, so the test does.
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }

    #[test]
    fn dedup_windows_are_the_reviewed_values() {
        // 5 minutes: the backoff ladder (about 45 minutes) fits inside even the
        // shorter of PasswordReset's two token lifetimes, and 5 minutes is short
        // enough not to defeat a user who genuinely did not receive the first mail.
        assert_eq!(
            MailKind::PasswordReset.dedup_window(),
            Duration::from_secs(300)
        );
        // 15 minutes bounds how stale a delivered digest can be.
        assert_eq!(
            MailKind::NotificationDigest.dedup_window(),
            Duration::from_secs(900)
        );
        // ZERO, and it must stay zero. A user is capped at 20 notifications an
        // hour upstream, so a 15-minute window here would suppress roughly 16 of
        // them — and suppression is indistinguishable from success, because the
        // enqueue returns the same `Ok(None)` it returns for a deliberate discard.
        assert_eq!(
            MailKind::PersonalNotification.dedup_window(),
            Duration::ZERO
        );
        // An operator clicking "test" twice must get two mails.
        assert_eq!(MailKind::SmtpTest.dedup_window(), Duration::ZERO);
    }
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-mail kind`
  Expected: `error[E0432]: unresolved import super::*` / `cannot find type MailKind in this scope` — and, before that, `file not found for module kind` until Step 4 declares it. Add `pub mod kind;` to `lib.rs` first if the compiler stops at the module declaration.

- [ ] **Step 3: Write the implementation above the test module.**
  Prepend to `backend/crates/sauron-mail/src/kind.rs`:

```rust
//! What a message *is*, and how often the same person may receive one.
//!
//! This enum is the authority for `mail_outbox.kind`, which deliberately carries
//! no CHECK constraint: the value set keeps growing, and the slice that adds the
//! fifth kind must not also have to widen a CHECK on a table holding live
//! credentials. Kind and dedup window have to change together, so keeping both
//! here is what stops them drifting apart.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailKind {
    PasswordReset,
    NotificationDigest,
    PersonalNotification,
    SmtpTest,
}

impl MailKind {
    /// The value written to `mail_outbox.kind`. Stable wire strings: an operator
    /// requeueing a row by hand and a dedup probe both match on them.
    pub fn as_str(&self) -> &'static str {
        match self {
            MailKind::PasswordReset => "password_reset",
            MailKind::NotificationDigest => "notification_digest",
            MailKind::PersonalNotification => "personal_notification",
            MailKind::SmtpTest => "smtp_test",
        }
    }

    /// Per-recipient suppression window. `Duration::ZERO` disables it.
    ///
    /// This is the only chokepoint where a per-recipient cap can live. Treating
    /// it as "the relay's problem" is wrong: the relay is the operator's own, and
    /// it is what gets throttled and blacklisted. With a Redis limiter alone an
    /// unauthenticated attacker sends roughly 14k mails a day to one victim, and
    /// that limiter degrades to a *per-process* window on any Redis blip,
    /// multiplied by replica count.
    pub fn dedup_window(&self) -> Duration {
        match self {
            MailKind::PasswordReset => Duration::from_secs(300),
            MailKind::NotificationDigest => Duration::from_secs(900),
            MailKind::PersonalNotification => Duration::ZERO,
            MailKind::SmtpTest => Duration::ZERO,
        }
    }
}
```

  **There is deliberately no `ttl()`.** How long a rendered body stays deliverable is a property of what the body carries, not of its kind: a one-hour self-service reset token and a 24-hour admin-initiated one both come from `PasswordReset`. A per-kind constant would mark the second `expired before delivery` an hour in, blank its body and destroy the manual requeue path — while the token it carried stayed valid for another 23 hours. Expiry is an argument to `enqueue`.

- [ ] **Step 4: Wire the module into `lib.rs`.**
  In `backend/crates/sauron-mail/src/lib.rs`, change the module block to:

```rust
pub mod kind;
pub mod text;

pub use kind::MailKind;
pub use text::{html_escape, substitute};
```

- [ ] **Step 5: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-mail`
  Expected: `test result: ok. 5 passed`.

- [ ] **Step 6: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 6: Templates — `MailContent`, `Branding`, `Cta`, `render`

**Files:**
- Create `backend/crates/sauron-mail/src/template.rs`
- Modify `backend/crates/sauron-mail/src/lib.rs` (add `pub mod template;` and re-exports)

**Interfaces:**
- Consumes: `sauron_mail::text::{html_escape, substitute}` (Task 4).
- Produces:
  - `pub struct MailContent { pub subject: String, pub heading: String, pub paragraphs: Vec<String>, pub cta: Option<Cta>, pub footnotes: Vec<String> }`
  - `pub struct Cta` with `pub fn new(label: impl Into<String>, url: impl Into<String>) -> Result<Cta, TemplateError>`
  - `pub struct Branding { pub product_name: String, pub dashboard_url: Option<String>, pub footer: String }` with `pub fn link(&self, hash_path: &str) -> Result<String, TemplateError>`
  - `pub struct RenderedMail { pub subject: String, pub text: String, pub html: String }`
  - `pub enum TemplateError { NoDashboardUrl, BadCtaUrl(String) }` (`thiserror`)
  - `pub fn render(b: &Branding, c: &MailContent) -> Result<RenderedMail, TemplateError>`

- [ ] **Step 1: Write the failing tests.**
  Create `backend/crates/sauron-mail/src/template.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn branding() -> Branding {
        Branding {
            product_name: "Sauron".into(),
            dashboard_url: Some("https://sauron.example.test".into()),
            footer: "Sent by Sauron.".into(),
        }
    }

    fn hostile_content() -> MailContent {
        MailContent {
            subject: "<script>alert(1)</script>&\"".into(),
            heading: "<script>alert(1)</script>&\"".into(),
            paragraphs: vec!["<script>alert(1)</script>&\"".into()],
            cta: Some(
                Cta::new(
                    "<script>alert(1)</script>&\"",
                    "https://sauron.example.test/#/reset-password?token=abc",
                )
                .unwrap(),
            ),
            footnotes: vec!["<script>alert(1)</script>&\"".into()],
        }
    }

    /// Collect every `{{key}}` a template declares. Used to pin the placeholder
    /// key sets, which is the only thing that catches a stray `{{` in the CSS
    /// silently deleting the stylesheet.
    fn placeholder_keys(t: &str) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        let mut rest = t;
        while let Some(open) = rest.find("{{") {
            let after = &rest[open + 2..];
            match after.find("}}") {
                Some(close) => {
                    keys.insert(after[..close].trim().to_string());
                    rest = &after[close + 2..];
                }
                None => break,
            }
        }
        keys
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn html_escapes_every_user_supplied_field() {
        let out = render(&branding(), &hostile_content()).unwrap();
        assert!(!out.html.contains("<script>"), "raw script tag in html");
        // Six escape sites, one per place a hostile value reaches the layout:
        // <title> (subject), the preheader span (first paragraph), the <h1>
        // (heading), the paragraph itself, the CTA label, and the footnote.
        // Counting them means dropping an escape site fails here rather than in
        // someone's inbox.
        assert_eq!(out.html.matches("&lt;script&gt;").count(), 6);
    }

    #[test]
    fn text_part_carries_user_content_verbatim_because_it_is_not_markup() {
        let out = render(&branding(), &hostile_content()).unwrap();
        assert!(out.text.contains("<script>alert(1)</script>&\""));
        assert!(!out.text.contains("&lt;"), "entities leaked into the text part");
        assert!(!out.text.contains("&amp;"), "entities leaked into the text part");
    }

    #[test]
    fn text_part_is_plain_readable_prose_with_the_url_on_its_own_line() {
        let content = MailContent {
            subject: "Reset your password".into(),
            heading: "Reset your password".into(),
            paragraphs: vec!["First paragraph.".into(), "Second paragraph.".into()],
            cta: Some(
                Cta::new(
                    "Choose a new password",
                    "https://sauron.example.test/#/reset-password?token=abc",
                )
                .unwrap(),
            ),
            footnotes: vec!["If the button does not work, paste the link above.".into()],
        };
        let out = render(&branding(), &content).unwrap();
        assert!(!out.text.contains('<'), "markup leaked into the text part: {}", out.text);
        assert!(out
            .text
            .contains("\nhttps://sauron.example.test/#/reset-password?token=abc\n"));
        let first = out.text.find("First paragraph.").unwrap();
        let second = out.text.find("Second paragraph.").unwrap();
        assert!(first < second, "paragraph order not preserved");
        assert!(out.text.contains("First paragraph.\n\nSecond paragraph."));
        assert!(out.text.trim_end().ends_with("Sauron"));
    }

    #[test]
    fn layout_placeholders_are_exactly_the_known_set() {
        assert_eq!(
            placeholder_keys(LAYOUT_HTML),
            set(&[
                "subject",
                "preheader",
                "product",
                "heading",
                "paragraphs",
                "cta",
                "footnotes",
                "footer",
            ])
        );
        assert_eq!(placeholder_keys(P_HTML), set(&["text"]));
        assert_eq!(placeholder_keys(FOOTNOTE_HTML), set(&["text"]));
        assert_eq!(placeholder_keys(CTA_HTML), set(&["url", "label"]));
    }

    #[test]
    fn layout_invariants_survive_editing() {
        let out = render(&branding(), &hostile_content()).unwrap();
        assert!(out.html.contains("max-width:600px"));
        assert!(out.html.contains("width=\"600\""));
        assert!(out.html.contains("role=\"presentation\""));
        assert!(out.html.contains("color-scheme"));
        // Remote images are blocked by default in Outlook and Gmail, so a logo is
        // an empty box in most inboxes. There is no <img> and there must not be.
        assert!(!out.html.contains("<img"));
        assert_eq!(out.html.matches("<!doctype").count(), 1);
        assert_eq!(out.html.matches("<head>").count(), 1);
        assert_eq!(out.html.matches("<body").count(), 1);
    }

    #[test]
    fn cta_rejects_every_scheme_that_is_not_http() {
        for bad in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "/reset",
            "reset-password",
            "HTTPS:/sauron.example.test",
        ] {
            assert!(Cta::new("Go", bad).is_err(), "{bad} was accepted");
        }
        assert!(Cta::new("Go", "http://localhost:3000/#/x").is_ok());
        assert!(Cta::new("Go", "https://sauron.example.test/#/x").is_ok());
    }

    #[test]
    fn link_requires_a_dashboard_url_and_produces_one_slash_before_the_hash() {
        let none = Branding {
            product_name: "Sauron".into(),
            dashboard_url: None,
            footer: String::new(),
        };
        assert!(matches!(
            none.link("/reset-password?token=abc"),
            Err(TemplateError::NoDashboardUrl)
        ));

        for base in [
            "https://sauron.example.test",
            "https://sauron.example.test/",
            "https://sauron.example.test///",
        ] {
            let b = Branding {
                product_name: "Sauron".into(),
                dashboard_url: Some(base.into()),
                footer: String::new(),
            };
            assert_eq!(
                b.link("/reset-password?token=abc").unwrap(),
                "https://sauron.example.test/#/reset-password?token=abc"
            );
        }
    }

    #[test]
    fn subject_cannot_carry_a_second_header() {
        let content = MailContent {
            subject: "Reset\r\nBcc: attacker@evil.test".into(),
            heading: "h".into(),
            paragraphs: vec![],
            cta: None,
            footnotes: vec![],
        };
        let out = render(&branding(), &content).unwrap();
        assert_eq!(out.subject, "Reset  Bcc: attacker@evil.test");
        assert!(!out.subject.contains('\r'));
        assert!(!out.subject.contains('\n'));
    }

    #[test]
    fn subject_truncates_to_two_hundred_characters() {
        let content = MailContent {
            subject: "x".repeat(500),
            heading: "h".into(),
            paragraphs: vec![],
            cta: None,
            footnotes: vec![],
        };
        let out = render(&branding(), &content).unwrap();
        assert_eq!(out.subject.chars().count(), 200);
    }

    #[test]
    fn preheader_is_the_first_paragraph_so_the_inbox_preview_is_not_garbage() {
        let content = MailContent {
            subject: "s".into(),
            heading: "h".into(),
            paragraphs: vec!["Someone asked to reset your password.".into()],
            cta: None,
            footnotes: vec![],
        };
        let out = render(&branding(), &content).unwrap();
        let preheader_at = out
            .html
            .find("Someone asked to reset your password.")
            .expect("preheader missing");
        let body_at = out.html.find("<h1").expect("card heading missing");
        assert!(preheader_at < body_at, "preheader must precede the card");
    }
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-mail template`
  Expected: `file not found for module template` until Step 5, then `cannot find type Branding in this scope` and nine more of the same.

- [ ] **Step 3: Add the content model and errors above the test module.**
  Prepend to `backend/crates/sauron-mail/src/template.rs`:

```rust
//! One content model, two independent renderers, and the HTML shell they share.
//!
//! Nothing here ever strips tags to produce the plain-text part. Tag-stripping
//! leaves entities behind as `&amp;`, drops the CTA's href leaving a bare label
//! with nowhere to go, and turns the table scaffolding into ragged whitespace.
//! The text part is written, not derived.

use std::collections::BTreeMap;

use crate::text::{html_escape, substitute};

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error(
        "DASHBOARD_URL is not set, so an email containing a link cannot be rendered; \
         set it to the browser-facing origin of the dashboard"
    )]
    NoDashboardUrl,
    #[error("call-to-action url must start with http:// or https:// (got {0:?})")]
    BadCtaUrl(String),
}

/// A button. Constructed through [`Cta::new`] so the scheme check cannot be
/// skipped by building the struct literally.
#[derive(Debug, Clone)]
pub struct Cta {
    label: String,
    url: String,
}

impl Cta {
    /// Belt and braces against a `javascript:` href. Every URL this codebase
    /// builds today comes from the scheme-validated `DASHBOARD_URL`, so this is
    /// the check that survives the first caller that builds one from something
    /// else.
    pub fn new(label: impl Into<String>, url: impl Into<String>) -> Result<Cta, TemplateError> {
        let url = url.into();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(TemplateError::BadCtaUrl(url));
        }
        Ok(Cta {
            label: label.into(),
            url,
        })
    }
}

/// Deployment-level chrome: the product name, where links point, and the footer.
#[derive(Debug, Clone)]
pub struct Branding {
    pub product_name: String,
    /// `None` when `DASHBOARD_URL` is unset. Every link-building path then fails
    /// loudly at render time rather than guessing an origin.
    pub dashboard_url: Option<String>,
    pub footer: String,
}

impl Branding {
    /// Build an absolute dashboard URL for a hash route.
    ///
    /// The `#` is load-bearing: the dashboard is `svelte-spa-router`, so a reset
    /// link is `https://host/#/reset-password?token=...`. Drop the `#` and the
    /// browser asks the static server for a path it does not serve.
    ///
    /// This is where "any email containing a link requires DASHBOARD_URL" is
    /// actually enforced.
    pub fn link(&self, hash_path: &str) -> Result<String, TemplateError> {
        let base = self
            .dashboard_url
            .as_deref()
            .ok_or(TemplateError::NoDashboardUrl)?
            .trim_end_matches('/');
        Ok(format!("{base}/#{hash_path}"))
    }
}

/// What a sender writes. Deliberately structural rather than a blob of markup:
/// a sender that hands over HTML is a sender that can be talked into handing
/// over someone else's HTML.
#[derive(Debug, Clone)]
pub struct MailContent {
    pub subject: String,
    pub heading: String,
    pub paragraphs: Vec<String>,
    pub cta: Option<Cta>,
    pub footnotes: Vec<String>,
}

/// What the transport sends.
#[derive(Debug, Clone)]
pub struct RenderedMail {
    pub subject: String,
    pub text: String,
    pub html: String,
}
```

- [ ] **Step 4: Add the templates and the two renderers.**
  Insert into `backend/crates/sauron-mail/src/template.rs`, after `RenderedMail` and before the `#[cfg(test)]` block:

```rust
/// The one HTML shell every product email renders into.
///
/// ESCAPING RULE, FIRST BECAUSE IT IS THE ONE THAT BITES: `html_escape` replaces
/// exactly `& < > "` and does NOT escape `'`. Every attribute below is therefore
/// double-quoted. Adding a single-quoted attribute introduces attribute breakout
/// the first time a value containing an apostrophe lands in it.
///
/// `substitute` treats any `{{` as a placeholder opener and renders an unknown
/// key as an empty string, so two adjacent `{` anywhere in the stylesheet would
/// silently delete everything up to the next `}}` — no error, no failing test, an
/// email that still sends and merely looks broken. The layout avoids that by
/// construction; `layout_placeholders_are_exactly_the_known_set` is what keeps it
/// true after the next edit.
///
/// Tables, never divs: Outlook 2016+ renders through Word, which ignores flex and
/// most margins. The `width="600"` attribute is for Word, which ignores
/// `max-width`; `max-width:600px` is for everyone else; `width:100%` keeps it
/// fluid on a phone. No `<img>` anywhere: remote images are blocked by default in
/// Outlook and Gmail, so a logo is an empty box in most inboxes — the wordmark is
/// text.
///
/// Dark mode is best-effort and this comment says so. Gmail strips
/// `prefers-color-scheme`, Outlook.com rewrites CSS, and Apple Mail and some
/// Android clients force-invert on their own. The promise is not a pixel-matched
/// dark variant; it is that the *inline* palette is legible whether or not any of
/// that happens, because dark ink on white reads correctly either way.
const LAYOUT_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="light dark">
<meta name="supported-color-schemes" content="light dark">
<title>{{subject}}</title>
<style>
:root { color-scheme: light dark; }
@media (prefers-color-scheme: dark) {
  .s-page { background-color: #0b0d12 !important; }
  .s-card { background-color: #151922 !important; border-color: #262c38 !important; }
  .s-h1 { color: #f3f4f6 !important; }
  .s-body { color: #d1d5db !important; }
  .s-muted { color: #9ca3af !important; }
  .s-foot { color: #6b7280 !important; }
}
</style>
</head>
<body class="s-page" style="margin:0;padding:0;background-color:#f4f5f7;-webkit-text-size-adjust:100%">
<span style="display:none;font-size:1px;color:#f4f5f7;line-height:1px;max-height:0;max-width:0;opacity:0;overflow:hidden">{{preheader}}&#8199;&#65279;</span>
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="border-collapse:collapse;mso-table-lspace:0pt;mso-table-rspace:0pt">
<tr>
<td align="center" style="padding:32px 12px">
<table role="presentation" width="600" cellpadding="0" cellspacing="0" border="0" style="width:100%;max-width:600px;border-collapse:collapse">
<tr>
<td class="s-muted" style="padding:0 0 16px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:13px;font-weight:600;letter-spacing:0.08em;text-transform:uppercase;color:#6b7280">{{product}}</td>
</tr>
<tr>
<td class="s-card" style="background-color:#ffffff;border:1px solid #e5e7eb;border-radius:10px;padding:32px">
<h1 class="s-h1" style="margin:0 0 16px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:22px;line-height:1.3;font-weight:600;color:#111827">{{heading}}</h1>
{{paragraphs}}
{{cta}}
{{footnotes}}
</td>
</tr>
<tr>
<td class="s-foot" style="padding:16px 0 0;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:12px;line-height:1.5;color:#9ca3af">{{footer}}</td>
</tr>
</table>
</td>
</tr>
</table>
</body>
</html>
"##;

/// One body paragraph. `substitute` cannot loop, so repeated blocks render one at
/// a time into an accumulator and go in as a single pre-escaped variable.
const P_HTML: &str = r##"<p class="s-body" style="margin:0 0 14px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:15px;line-height:1.6;color:#374151">{{text}}</p>
"##;

/// A footnote. `word-break:break-all` because the raw-URL fallback a CTA needs is
/// long enough to blow the 600px card open on a phone otherwise.
const FOOTNOTE_HTML: &str = r##"<p class="s-body" style="margin:14px 0 0;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:13px;line-height:1.6;color:#6b7280;word-break:break-all">{{text}}</p>
"##;

/// The bulletproof-button pattern: a one-cell table with `bgcolor` and a
/// border-radius wrapping an inline-block anchor, because Outlook ignores
/// padding on an `<a>` and background-color on anything it renders through Word.
/// The accent is the same blue as `Severity::Info`.
const CTA_HTML: &str = r##"<table role="presentation" cellpadding="0" cellspacing="0" border="0" style="margin:8px 0 4px;border-collapse:collapse">
<tr>
<td align="center" bgcolor="#3b82f6" style="border-radius:8px">
<a href="{{url}}" style="display:inline-block;padding:12px 26px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;font-size:15px;font-weight:600;line-height:1;color:#ffffff;text-decoration:none;border-radius:8px">{{label}}</a>
</td>
</tr>
</table>
"##;

/// Longest subject we will emit. A long subject is truncated by every client
/// anyway; the cap exists so a caller cannot push kilobytes into a header.
const MAX_SUBJECT_CHARS: usize = 200;

/// Flatten a header value to one line and cap it.
///
/// A user-supplied fragment in a subject — an app name, a display name — is
/// exactly how SMTP header injection happens: a bare CRLF ends the Subject header
/// and starts a `Bcc:` one.
fn sanitize_header(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .take(MAX_SUBJECT_CHARS)
        .collect()
}

fn render_html(b: &Branding, c: &MailContent) -> String {
    let mut paragraphs = String::new();
    for p in &c.paragraphs {
        let mut one = BTreeMap::new();
        one.insert("text".to_string(), html_escape(p));
        paragraphs.push_str(&substitute(P_HTML, &one));
    }

    let mut footnotes = String::new();
    for note in &c.footnotes {
        let mut one = BTreeMap::new();
        one.insert("text".to_string(), html_escape(note));
        footnotes.push_str(&substitute(FOOTNOTE_HTML, &one));
    }

    let cta = match &c.cta {
        None => String::new(),
        Some(cta) => {
            let mut v = BTreeMap::new();
            v.insert("url".to_string(), html_escape(&cta.url));
            v.insert("label".to_string(), html_escape(&cta.label));
            substitute(CTA_HTML, &v)
        }
    };

    // Named `escaped` because that is the invariant, not a description. Every
    // value below is either already through `html_escape` or is markup this
    // module built itself. `substitute` copies bytes and escapes nothing, so a
    // raw value here is stored XSS in someone's inbox.
    let mut escaped = BTreeMap::new();
    escaped.insert(
        "subject".to_string(),
        html_escape(&sanitize_header(&c.subject)),
    );
    escaped.insert(
        "preheader".to_string(),
        html_escape(c.paragraphs.first().map(String::as_str).unwrap_or("")),
    );
    escaped.insert("product".to_string(), html_escape(&b.product_name));
    escaped.insert("heading".to_string(), html_escape(&c.heading));
    escaped.insert("paragraphs".to_string(), paragraphs);
    escaped.insert("cta".to_string(), cta);
    escaped.insert("footnotes".to_string(), footnotes);
    escaped.insert("footer".to_string(), html_escape(&b.footer));
    substitute(LAYOUT_HTML, &escaped)
}

/// The plain-text part. No escaping anywhere, because it is not markup — a text
/// part carrying `&amp;` is the tell that someone derived it from the HTML.
fn render_text(b: &Branding, c: &MailContent) -> String {
    let mut out = String::new();
    out.push_str(&c.heading);
    out.push_str("\n\n");
    for p in &c.paragraphs {
        out.push_str(p);
        out.push_str("\n\n");
    }
    if let Some(cta) = &c.cta {
        out.push_str(&cta.label);
        out.push_str(":\n");
        out.push_str(&cta.url);
        out.push_str("\n\n");
    }
    for note in &c.footnotes {
        out.push_str(note);
        out.push('\n');
    }
    out.push_str("\n—\n");
    out.push_str(&b.product_name);
    out.push('\n');
    out
}

/// Render one message into both parts.
///
/// Returns `Result` even though nothing in the two renderers can fail today: the
/// fallible steps (`Cta::new`, `Branding::link`) run in the caller, and keeping
/// the signature fallible means adding a fallible step later is not a breaking
/// change across S1 and S3's call sites.
pub fn render(b: &Branding, c: &MailContent) -> Result<RenderedMail, TemplateError> {
    Ok(RenderedMail {
        subject: sanitize_header(&c.subject),
        text: render_text(b, c),
        html: render_html(b, c),
    })
}
```

- [ ] **Step 5: Wire the module into `lib.rs`.**
  In `backend/crates/sauron-mail/src/lib.rs`, replace the module/re-export block with:

```rust
pub mod kind;
pub mod template;
pub mod text;

pub use kind::MailKind;
pub use template::{render, Branding, Cta, MailContent, RenderedMail, TemplateError};
pub use text::{html_escape, substitute};
```

- [ ] **Step 6: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-mail`
  Expected: `test result: ok. 15 passed`.

- [ ] **Step 7: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 7: `smtp.rs` — params, errors, the total deadline, and the dev sink

**Files:**
- Create `backend/crates/sauron-mail/src/smtp.rs`
- Modify `backend/crates/sauron-mail/src/lib.rs` (add `pub mod smtp;`, the re-exports, and the `sauron_core::config` re-export)

**Interfaces:**
- Consumes: `sauron_core::config::{SmtpSettings, SmtpTls}` (Task 2); `sauron_monitor_core::ssrf::resolve_checked(host: &str, allow_private: bool) -> Result<Vec<std::net::SocketAddr>, String>`.
- Produces:
  - `pub struct SmtpParams { pub host: String, pub port: u16, pub username: Option<String>, pub password: Option<String>, pub tls: SmtpTls, pub allow_private: bool, pub op_timeout: Duration, pub total_deadline: Duration, pub sink: bool, pub sink_log_body: bool }` with `pub fn from_settings(s: &SmtpSettings) -> Self` and a hand-written redacting `Debug`
  - `pub enum MailBody { Text(String), Alternative { text: String, html: String } }`
  - `pub struct OutgoingMail { pub from_address: String, pub from_name: Option<String>, pub to: Vec<String>, pub reply_to: Option<String>, pub subject: String, pub body: MailBody }`
  - `pub enum MailError { InvalidFrom(String), InvalidRecipient(String), Tls(String), Dns(String), Blocked(String), Build(String), Send(String), Rejected(String), DeadlineExceeded(u64) }`
  - `pub struct SmtpClient` with `pub async fn connect(p: &SmtpParams) -> Result<SmtpClient, MailError>` and `pub async fn send(&self, mail: &OutgoingMail) -> Result<(), MailError>`
  - `pub async fn send(p: &SmtpParams, mail: &OutgoingMail) -> Result<(), MailError>` (one-shot)
  - `pub fn is_transient(msg: &str) -> bool`
  - `pub fn normalize_recipient(raw: &str) -> Result<String, MailError>`

- [ ] **Step 1: Write the failing unit tests.**
  Create `backend/crates/sauron-mail/src/smtp.rs` with only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipient_normalization_collapses_the_variants_one_mailbox_accepts() {
        let key = "victim@corp.test";
        assert_eq!(normalize_recipient("victim@corp.test").unwrap(), key);
        assert_eq!(normalize_recipient("Victim@Corp.Test").unwrap(), key);
        assert_eq!(normalize_recipient("victim@corp.test ").unwrap(), key);
        assert_eq!(normalize_recipient("  victim@corp.test").unwrap(), key);
    }

    #[test]
    fn recipient_normalization_rejects_rather_than_truncates() {
        // lettre's parser discards the unparsed remainder, so a "parse and keep
        // going" barrier is not a barrier: this string and `victim@corp.test`
        // would otherwise be two rows delivering to one mailbox and each getting
        // its own per-recipient budget.
        for bad in [
            "victim@corp.test <x>",
            "victim@corp.test, other@corp.test",
            "victim@corp.test\r\nBcc: attacker@evil.test",
            "not-an-address",
            "",
        ] {
            assert!(
                normalize_recipient(bad).is_err(),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn params_debug_redacts_the_password_and_keeps_the_username() {
        let p = SmtpParams {
            host: "smtp.example.test".into(),
            port: 587,
            username: Some("mailer".into()),
            password: Some("hunter2".into()),
            tls: SmtpTls::StartTls,
            allow_private: false,
            op_timeout: Duration::from_millis(10_000),
            total_deadline: Duration::from_millis(30_000),
            sink: false,
            sink_log_body: false,
        };
        let printed = format!("{p:?}");
        assert!(printed.contains("<redacted>"), "got: {printed}");
        assert!(!printed.contains("hunter2"), "got: {printed}");
        assert!(printed.contains("mailer"), "got: {printed}");
    }

    #[test]
    fn total_deadline_is_three_operation_timeouts_capped_at_a_minute() {
        let base = sauron_core::config::build_smtp(
            Some("smtp.example.test".into()),
            587,
            None,
            None,
            Some("a@c.test".into()),
            "Sauron".into(),
            None,
            false,
            10_000,
            false,
        )
        .unwrap();
        let p = SmtpParams::from_settings(&base);
        assert_eq!(p.op_timeout, Duration::from_millis(10_000));
        assert_eq!(p.total_deadline, Duration::from_millis(30_000));

        let slow = sauron_core::config::build_smtp(
            Some("smtp.example.test".into()),
            587,
            None,
            None,
            Some("a@c.test".into()),
            "Sauron".into(),
            None,
            false,
            60_000,
            false,
        )
        .unwrap();
        let p = SmtpParams::from_settings(&slow);
        // Without the cap the worst case is 3 minutes of one drain slot held on a
        // tarpitting relay, which outlives the stale-row threshold and lets a
        // requeued duplicate race the original.
        assert_eq!(p.total_deadline, Duration::from_secs(60));
    }

    /// The alerting engine decides whether to retry by substring-matching the
    /// error's Display. Moving the transport into another crate turned that
    /// contract into a `thiserror` attribute in a different file, where
    /// "improving" the wording would silently stop every alert email retrying
    /// with nothing failing to compile. This test is the other side of that
    /// coupling; `sauron-alerts` carries the matching one.
    #[test]
    fn is_transient_matches_the_four_substrings_alerting_relies_on() {
        assert!(is_transient("request failed: connection refused"));
        assert!(is_transient("HTTP 503 from target"));
        assert!(is_transient("HTTP 429 from target"));
        assert!(is_transient(&MailError::Send("connection reset".into()).to_string()));
        assert!(is_transient(&MailError::DeadlineExceeded(30_000).to_string()));
        // Rejected's Display is byte-identical to Send's, so alerting keeps
        // retrying 5xx exactly as it did before this refactor.
        assert!(is_transient(&MailError::Rejected("550 no such user".into()).to_string()));

        assert!(!is_transient(&MailError::InvalidFrom("x".into()).to_string()));
        assert!(!is_transient(&MailError::InvalidRecipient("x".into()).to_string()));
        assert!(!is_transient(&MailError::Blocked("x".into()).to_string()));
        assert!(!is_transient(&MailError::Build("x".into()).to_string()));
        assert!(!is_transient(&MailError::Dns("x".into()).to_string()));
        assert!(!is_transient(&MailError::Tls("x".into()).to_string()));
    }

    #[test]
    fn rejected_and_send_display_identically_because_a_route_returns_the_string() {
        // `POST /v1/notification-channels/{id}/test` returns this verbatim as the
        // `error` field and persists it to `alert_events`. An earlier draft added
        // a "(permanent)" infix; that would have changed a user-visible string
        // while claiming byte-for-byte parity. The drain distinguishes the two by
        // VARIANT, which is free.
        assert_eq!(
            MailError::Send("boom".into()).to_string(),
            MailError::Rejected("boom".into()).to_string()
        );
        assert!(MailError::Send("boom".into())
            .to_string()
            .starts_with("smtp send failed"));
    }

    #[test]
    fn resolve_errors_classify_toward_transient_when_unrecognised() {
        assert!(matches!(
            classify_resolve_error("DNS resolution failed: timed out".into()),
            MailError::Dns(_)
        ));
        assert!(matches!(
            classify_resolve_error("target x did not resolve".into()),
            MailError::Dns(_)
        ));
        assert!(matches!(
            classify_resolve_error("target x resolves to a blocked address".into()),
            MailError::Blocked(_)
        ));
        // Anything unrecognised is Dns, i.e. transient. That is the safe
        // direction: if the upstream wording drifts, mail retries and eventually
        // fails out, rather than being marked permanent on the first hiccup.
        assert!(matches!(
            classify_resolve_error("something new upstream".into()),
            MailError::Dns(_)
        ));
    }

    #[test]
    fn blocked_message_names_the_variable_the_upstream_error_omits() {
        let e = classify_resolve_error("target 127.0.0.1 resolves to a blocked address".into());
        let text = e.to_string();
        assert!(text.contains("SMTP_ALLOW_PRIVATE"), "got: {text}");
    }

    #[tokio::test]
    async fn the_sink_never_opens_a_socket() {
        // Host deliberately unresolvable. If the sink branch were placed after
        // resolution this would fail with a DNS error instead of succeeding.
        let p = SmtpParams {
            host: "no-such-host.invalid".into(),
            port: 587,
            username: None,
            password: None,
            tls: SmtpTls::StartTls,
            allow_private: false,
            op_timeout: Duration::from_millis(10_000),
            total_deadline: Duration::from_millis(30_000),
            sink: true,
            sink_log_body: false,
        };
        let client = SmtpClient::connect(&p).await.expect("sink connect");
        let mail = OutgoingMail {
            from_address: "sauron@localhost".into(),
            from_name: Some("Sauron".into()),
            to: vec!["victim@corp.test".into()],
            reply_to: None,
            subject: "Reset your password".into(),
            body: MailBody::Alternative {
                text: "plain".into(),
                html: "<p>html</p>".into(),
            },
        };
        client.send(&mail).await.expect("sink send");
    }

    #[tokio::test]
    async fn cleartext_to_a_non_loopback_relay_is_blocked_at_connect() {
        // The structural half of the loopback rule: `build_smtp` checks the
        // configured string, this checks what it actually resolved to, which is
        // what survives a `localhost` that has been pointed off-box.
        //
        // The host is an IP literal, not a name: `tokio::net::lookup_host`
        // short-circuits a literal without touching the resolver, so this unit
        // test does no DNS and cannot stall or flake on a machine with no
        // network. A name here would put a live lookup inside
        // `cargo test --workspace`.
        let p = SmtpParams {
            host: "93.184.216.34".into(),
            port: 25,
            username: None,
            password: None,
            tls: SmtpTls::None,
            allow_private: false,
            op_timeout: Duration::from_millis(2_000),
            total_deadline: Duration::from_millis(6_000),
            sink: false,
            sink_log_body: false,
        };
        match SmtpClient::connect(&p).await {
            Err(MailError::Blocked(m)) => assert!(m.contains("loopback"), "got: {m}"),
            // The Ok payload is never formatted: `SmtpClient` holds a lettre
            // `AsyncSmtpTransport`, which has no `Debug`, so a `{other:?}`
            // catch-all over the whole `Result` does not compile.
            Ok(_) => panic!("expected Blocked, got Ok"),
            Err(e) => panic!("expected Blocked, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn starttls_to_a_private_address_is_refused_by_the_ssrf_guard() {
        let p = SmtpParams {
            host: "127.0.0.1".into(),
            port: 587,
            username: None,
            password: None,
            tls: SmtpTls::StartTls,
            allow_private: false,
            op_timeout: Duration::from_millis(2_000),
            total_deadline: Duration::from_millis(6_000),
            sink: false,
            sink_log_body: false,
        };
        match SmtpClient::connect(&p).await {
            Err(MailError::Blocked(_)) => {}
            Ok(_) => panic!("expected Blocked, got Ok"),
            Err(e) => panic!("expected Blocked, got {e:?}"),
        }
    }
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-mail smtp`
  Expected: `cannot find type SmtpParams in this scope`, `cannot find function normalize_recipient in this scope`, and eight more of the same.

- [ ] **Step 3: Write the types, the error enum and the two pure predicates.**
  Prepend to `backend/crates/sauron-mail/src/smtp.rs`:

```rust
//! Transmission: build one RFC 5322 message and get it to a relay, inside one
//! total deadline, without ever dialling an address that was not validated.

use std::str::FromStr;
use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::response::Severity;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tracing::warn;

use sauron_core::config::{SmtpSettings, SmtpTls};
use sauron_monitor_core::ssrf::resolve_checked;

/// Everything one send needs, with no reference to where the message came from.
#[derive(Clone)]
pub struct SmtpParams {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub tls: SmtpTls,
    pub allow_private: bool,
    /// Applied by lettre per socket operation (connect, EHLO, STARTTLS, AUTH,
    /// MAIL FROM, RCPT TO, DATA, end-of-data, QUIT).
    pub op_timeout: Duration,
    /// Applied by us over the whole send, DNS included. Without it the worst case
    /// is unbounded: the per-operation timeout multiplies by the number of
    /// operations, and `resolve_checked`'s `lookup_host` has no timeout at all.
    pub total_deadline: Duration,
    /// Return before touching a socket and write the message to the log instead.
    pub sink: bool,
    /// Log the plain-text BODY as well as the header line. Requires both
    /// `SMTP_SINK=1` and `SAURON_DEV=1`; see the module doc on the sink below.
    pub sink_log_body: bool,
}

impl std::fmt::Debug for SmtpParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // This is the copy that lives inside the drain loop, and it is the struct
        // a contributor debugging a delivery failure reaches for with
        // `debug!(?params, ...)`. clippy would not object to a `#[derive(Debug)]`
        // here and it would bypass every redaction in `sauron-core`.
        f.debug_struct("SmtpParams")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("tls", &self.tls)
            .field("allow_private", &self.allow_private)
            .field("op_timeout", &self.op_timeout)
            .field("total_deadline", &self.total_deadline)
            .field("sink", &self.sink)
            .field("sink_log_body", &self.sink_log_body)
            .finish()
    }
}

/// Hard ceiling on the total deadline, whatever `SMTP_TIMEOUT_MS` says.
const MAX_TOTAL_DEADLINE: Duration = Duration::from_secs(60);

impl SmtpParams {
    pub fn from_settings(s: &SmtpSettings) -> Self {
        let op_timeout = Duration::from_millis(s.timeout_ms);
        Self {
            host: s.host.clone(),
            port: s.port,
            username: s.username.clone(),
            password: s.password.clone(),
            tls: s.tls,
            allow_private: s.allow_private,
            op_timeout,
            total_deadline: std::cmp::min(op_timeout * 3, MAX_TOTAL_DEADLINE),
            sink: s.sink,
            // Off unless the caller opts in with the second variable.
            sink_log_body: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum MailBody {
    /// `text/plain`, byte-identical to what alert mail has always sent.
    Text(String),
    /// `multipart/alternative`.
    Alternative { text: String, html: String },
}

#[derive(Debug, Clone)]
pub struct OutgoingMail {
    pub from_address: String,
    pub from_name: Option<String>,
    pub to: Vec<String>,
    pub reply_to: Option<String>,
    pub subject: String,
    pub body: MailBody,
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("invalid from address: {0}")]
    InvalidFrom(String),
    #[error("invalid recipient: {0}")]
    InvalidRecipient(String),
    #[error("smtp tls setup failed: {0}")]
    Tls(String),
    #[error("{0}")]
    Dns(String),
    #[error("{0}")]
    Blocked(String),
    #[error("email build failed: {0}")]
    Build(String),
    #[error("smtp send failed: {0}")]
    Send(String),
    /// A 5xx from the relay. Display is DELIBERATELY identical to `Send`'s —
    /// `test_channel` returns this string verbatim in an HTTP response body and
    /// persists it to `alert_events`, so adding a "(permanent)" infix would have
    /// changed a user-visible string while claiming byte-for-byte parity. The
    /// drain distinguishes the two by variant, which is free.
    #[error("smtp send failed: {0}")]
    Rejected(String),
    #[error("smtp send failed: deadline exceeded after {0}ms")]
    DeadlineExceeded(u64),
}

/// Whether an error message describes something worth retrying.
///
/// The four substrings are exactly the ones `sauron_alerts::engine` used inline
/// before this crate existed. Moving them here rather than reimplementing them is
/// the point: the coupling between an error's wording and whether alert email
/// retries is invisible to the compiler, so it has to be visible to a reader.
pub fn is_transient(msg: &str) -> bool {
    msg.contains("request failed")
        || msg.contains("HTTP 5")
        || msg.contains("HTTP 429")
        || msg.contains("smtp send failed")
}

/// Split `resolve_checked`'s untyped errors into the two that mean different
/// things, defaulting everything else to the transient side.
fn classify_resolve_error(e: String) -> MailError {
    if e.contains("resolves to a blocked address") {
        // The upstream message names the host but not the variable, and an
        // operator reading it in a journal has no way to know which flag governs.
        MailError::Blocked(format!(
            "{e}; set SMTP_ALLOW_PRIVATE=true only if the relay is deliberately on a \
             private network"
        ))
    } else {
        MailError::Dns(e)
    }
}

/// Parse, reject anything unparseable, and return the lowercased address for
/// `recipient_key`.
///
/// Delegating the entire header-injection barrier to a transitive dependency that
/// discards its unparsed remainder is not a barrier.
pub fn normalize_recipient(raw: &str) -> Result<String, MailError> {
    let trimmed = raw.trim();
    let addr = Address::from_str(trimmed)
        .map_err(|_| MailError::InvalidRecipient(trimmed.replace(['\r', '\n'], " ")))?;
    Ok(addr.to_string().to_lowercase())
}
```

- [ ] **Step 4: Add the message builder and the error classifier.**
  Append to `backend/crates/sauron-mail/src/smtp.rs`, before the `#[cfg(test)]` block:

```rust
fn build_message(mail: &OutgoingMail) -> Result<Message, MailError> {
    // Parsed as a bare `Address` and handed to `Mailbox::new` with the display
    // name separate, so lettre does the RFC 2047 encoding rather than us
    // `format!`-ing a header — which is how a display name containing a newline
    // becomes a second header.
    let from_addr = Address::from_str(mail.from_address.trim())
        .map_err(|_| MailError::InvalidFrom(mail.from_address.replace(['\r', '\n'], " ")))?;
    let from = Mailbox::new(mail.from_name.clone(), from_addr);

    let mut builder = Message::builder()
        .from(from)
        .subject(mail.subject.as_str());

    if let Some(rt) = &mail.reply_to {
        let addr = Address::from_str(rt.trim())
            .map_err(|_| MailError::InvalidFrom(rt.replace(['\r', '\n'], " ")))?;
        builder = builder.reply_to(Mailbox::new(None, addr));
    }

    for rcpt in &mail.to {
        let addr = Address::from_str(rcpt.trim())
            .map_err(|_| MailError::InvalidRecipient(rcpt.replace(['\r', '\n'], " ")))?;
        builder = builder.to(Mailbox::new(None, addr));
    }

    let built = match &mail.body {
        MailBody::Text(t) => builder.header(ContentType::TEXT_PLAIN).body(t.clone()),
        MailBody::Alternative { text, html } => {
            builder.multipart(MultiPart::alternative_plain_html(text.clone(), html.clone()))
        }
    };
    built.map_err(|e| MailError::Build(e.to_string()))
}

fn classify_smtp_error(e: lettre::transport::smtp::Error) -> MailError {
    if let Some(code) = e.status() {
        if code.severity == Severity::PermanentNegativeCompletion {
            return MailError::Rejected(e.to_string());
        }
    }
    MailError::Send(e.to_string())
}
```

- [ ] **Step 5: Add `SmtpClient` and the one-shot `send`.**
  Append to `backend/crates/sauron-mail/src/smtp.rs`, still before the `#[cfg(test)]` block:

```rust
/// A relay connection built once and reused for a batch.
///
/// The transport is the expensive part: a DNS lookup, a TCP connect, a full TLS
/// handshake and an AUTH round trip. Rebuilding it per message is tolerable for
/// one alert and is 10k connection+AUTH cycles at digest volume, which postfix's
/// `smtpd_client_connection_rate_limit` and every hosted relay will throttle.
pub struct SmtpClient {
    /// `None` means the dev sink: nothing was opened and nothing will be sent.
    transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
    deadline: Duration,
    sink_log_body: bool,
}

impl SmtpClient {
    pub async fn connect(p: &SmtpParams) -> Result<SmtpClient, MailError> {
        let d = p.total_deadline;
        tokio::time::timeout(d, Self::connect_inner(p))
            .await
            .map_err(|_| MailError::DeadlineExceeded(d.as_millis() as u64))?
    }

    async fn connect_inner(p: &SmtpParams) -> Result<SmtpClient, MailError> {
        // The sink sits at the single narrowest point that would otherwise open a
        // connection, so every caller, every template and the whole outbox state
        // machine are exercised identically to production.
        if p.sink {
            return Ok(SmtpClient {
                transport: None,
                deadline: p.total_deadline,
                sink_log_body: p.sink_log_body,
            });
        }

        // Always resolve and always pin, so the value that was validated is the
        // value dialled. TLS still validates the certificate against the
        // configured hostname, so pinning costs no authenticity. The shipped
        // alerting path skipped resolution entirely when allow_private was set,
        // which quietly dropped the DNS-rebinding pin on exactly the deployments
        // most likely to need it.
        let addrs = resolve_checked(&p.host, p.allow_private || p.tls == SmtpTls::None)
            .await
            .map_err(classify_resolve_error)?;

        if p.tls == SmtpTls::None && !addrs.iter().all(|a| a.ip().is_loopback()) {
            return Err(MailError::Blocked(format!(
                "SMTP_TLS=none requires SMTP_HOST to resolve to loopback; {} resolves to {} \
                 — use SMTP_TLS=starttls, or put a local relay in front",
                p.host,
                addrs[0].ip()
            )));
        }
        let pinned = addrs[0].ip().to_string();

        let tls = match p.tls {
            SmtpTls::Implicit => Tls::Wrapper(
                TlsParameters::new(p.host.clone()).map_err(|e| MailError::Tls(e.to_string()))?,
            ),
            // `Required` aborts if the server will not upgrade, so there is no
            // silent fallback to cleartext on this branch.
            SmtpTls::StartTls => Tls::Required(
                TlsParameters::new(p.host.clone()).map_err(|e| MailError::Tls(e.to_string()))?,
            ),
            SmtpTls::None => Tls::None,
        };

        let mut tb = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(pinned)
            .tls(tls)
            .port(p.port)
            .timeout(Some(p.op_timeout));
        if let (Some(u), Some(pw)) = (p.username.clone(), p.password.clone()) {
            tb = tb.credentials(Credentials::new(u, pw));
        }

        Ok(SmtpClient {
            transport: Some(tb.build()),
            deadline: p.total_deadline,
            sink_log_body: p.sink_log_body,
        })
    }

    pub async fn send(&self, mail: &OutgoingMail) -> Result<(), MailError> {
        let d = self.deadline;
        tokio::time::timeout(d, self.send_inner(mail))
            .await
            .map_err(|_| MailError::DeadlineExceeded(d.as_millis() as u64))?
    }

    async fn send_inner(&self, mail: &OutgoingMail) -> Result<(), MailError> {
        let msg = build_message(mail)?;
        match &self.transport {
            None => {
                // The header line always, at warn!, so a sink can never be
                // silently on in production.
                warn!(
                    to = %mail.to.join(","),
                    subject = %mail.subject,
                    "SMTP_SINK=1: message NOT transmitted"
                );
                if self.sink_log_body {
                    // Logs are routinely shipped to an aggregator with a broader
                    // reader set and a longer retention than the database, so a
                    // sink that logs bodies strictly worsens the exposure the rest
                    // of this design narrows. Two explicit variables gate it, and
                    // RUST_LOG is no gate: the shipped default is
                    // `info,sauron=debug` and EnvFilter matches targets by prefix.
                    //
                    // The PLAIN-TEXT body is the one logged, not the HTML: it is
                    // the readable one and it contains the same URL.
                    let text = match &mail.body {
                        MailBody::Text(t) => t.as_str(),
                        MailBody::Alternative { text, .. } => text.as_str(),
                    };
                    warn!(body = %text, "SMTP_SINK body (SAURON_DEV=1)");
                }
                Ok(())
            }
            Some(t) => t.send(msg).await.map(|_| ()).map_err(classify_smtp_error),
        }
    }
}

/// Connect, send, drop. What `sauron-alerts` calls: one alert, one relay, no
/// batch to amortise a transport over.
pub async fn send(p: &SmtpParams, mail: &OutgoingMail) -> Result<(), MailError> {
    let d = p.total_deadline;
    tokio::time::timeout(d, async {
        let client = SmtpClient::connect_inner(p).await?;
        client.send_inner(mail).await
    })
    .await
    .map_err(|_| MailError::DeadlineExceeded(d.as_millis() as u64))?
}
```

- [ ] **Step 6: Wire the module into `lib.rs`.**
  In `backend/crates/sauron-mail/src/lib.rs`, replace the module/re-export block with:

```rust
pub mod kind;
pub mod smtp;
pub mod template;
pub mod text;

pub use kind::MailKind;
pub use smtp::{
    is_transient, normalize_recipient, send, MailBody, MailError, OutgoingMail, SmtpClient,
    SmtpParams,
};
pub use template::{render, Branding, Cta, MailContent, RenderedMail, TemplateError};
pub use text::{html_escape, substitute};

// Single home for both: `sauron-core`. This crate depends on `sauron-core`, so
// defining them here too would be a second, incompatible type — and `Config`
// cannot depend on `sauron-mail` without a cycle.
pub use sauron_core::config::{SmtpSettings, SmtpTls};
```

- [ ] **Step 7: Run the tests and watch them pass.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-mail`
  Expected: `test result: ok. 26 passed`.
  `starttls_to_a_private_address_is_refused_by_the_ssrf_guard` and `the_sink_never_opens_a_socket` are the two that would catch the sink being placed after resolution.

- [ ] **Step 8: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 8: Route `sauron-alerts` through `sauron-mail` and drop its `lettre` dependency

**Files:**
- Modify `backend/crates/sauron-alerts/Cargo.toml` (remove the `lettre` line)
- Modify `backend/crates/sauron-alerts/src/deliver.rs` (replace `deliver_email` at lines 138-201; delete the `lettre::*` imports at lines 7-11)
- Modify `backend/crates/sauron-alerts/src/engine.rs` (the four-substring predicate at lines 209-213)

**Interfaces:**
- Consumes: `sauron_mail::{send, is_transient, MailBody, MailError, OutgoingMail, SmtpParams, SmtpTls}` (Task 7).
- Produces: no new public API. The observable contract this task must preserve is that `POST /v1/notification-channels/{id}/test` still returns the same `smtp send failed: ...` string in its `error` field, and that alert email is still `text/plain` with the `[Sauron/{severity}] {title}` subject and the `— Sauron alerting` footer.

- [ ] **Step 1: Write the failing coupling test.**
  In `backend/crates/sauron-alerts/src/engine.rs`, append a `#[cfg(test)] mod tests` block at the end of the file (the file has none today):

```rust
#[cfg(test)]
mod tests {
    use sauron_mail::{is_transient, MailError};

    /// The retry decision for alert email is made by substring-matching an error
    /// string produced in a different crate. Nothing about that coupling is
    /// visible to the compiler, so it is pinned from both sides: this is the
    /// `sauron-alerts` half, and `sauron_mail::smtp`'s
    /// `is_transient_matches_the_four_substrings_alerting_relies_on` is the other.
    #[test]
    fn every_mail_error_variant_keeps_the_retry_behaviour_it_had_before_the_move() {
        // Retried, exactly as before the transport moved crates.
        assert!(is_transient(&MailError::Send("connection reset".into()).to_string()));
        assert!(is_transient(&MailError::DeadlineExceeded(30_000).to_string()));
        // Still retried. Splitting SMTP 4xx from 5xx for the alerting path is a
        // deliberate follow-up with its own decision, NOT a side effect of this
        // refactor — a permanently misconfigured email channel burning three
        // attempts is the behaviour that exists today.
        assert!(is_transient(&MailError::Rejected("550 no such user".into()).to_string()));

        // Never retried: configuration faults that will not heal.
        assert!(!is_transient(&MailError::InvalidFrom("x@".into()).to_string()));
        assert!(!is_transient(&MailError::InvalidRecipient("x@".into()).to_string()));
        assert!(!is_transient(&MailError::Blocked("blocked".into()).to_string()));
        assert!(!is_transient(&MailError::Build("bad".into()).to_string()));
    }
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts every_mail_error_variant`
  Expected: compiles and **passes**, because `is_transient` already exists from Task 7 and the alerting engine's own predicate is not yet involved. That is the point: this test pins the contract before the call site changes, so Step 4's edit is provably a no-op on behaviour. Record the pass; the failing signal for this task is Step 5.

- [ ] **Step 3: Rewrite `deliver_email`.**
  In `backend/crates/sauron-alerts/src/deliver.rs`:
  - Delete the five `lettre` import lines (7-11):
    `use lettre::message::header::ContentType;`, `use lettre::message::Mailbox;`, `use lettre::transport::smtp::authentication::Credentials;`, `use lettre::transport::smtp::client::TlsParameters;`, `use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};`
  - Add in their place:

```rust
use sauron_mail::{MailBody, OutgoingMail, SmtpParams, SmtpTls};
```

  - Replace the entire body of `async fn deliver_email` (lines 138-201) with:

```rust
async fn deliver_email(
    e: &crate::channel::EmailDest,
    ctx: &AlertContext,
    message: &str,
    opts: &DeliverOpts,
) -> Result<(), String> {
    // Everything this function used to do by hand — SSRF resolution, IP pinning,
    // TLS selection, credential wiring, message building — now happens once in
    // `sauron-mail`, so the reset-mail path and the alert path cannot drift apart
    // on any of it. The one behaviour that CHANGES here is the total deadline:
    // lettre applies its timeout per socket operation, so before this a
    // tarpitting relay could hold one alert delivery indefinitely.
    let params = SmtpParams {
        host: e.host.clone(),
        port: e.port,
        username: e.username.clone(),
        password: e.password.clone(),
        // Only ever Implicit or StartTls here, so this path's "never cleartext"
        // guarantee is preserved exactly — `SmtpTls::None` is unreachable from a
        // notification channel.
        tls: if e.implicit_tls {
            SmtpTls::Implicit
        } else {
            SmtpTls::StartTls
        },
        allow_private: opts.allow_private,
        op_timeout: opts.timeout,
        total_deadline: std::cmp::min(opts.timeout * 3, Duration::from_secs(60)),
        sink: false,
        sink_log_body: false,
    };

    let mail = OutgoingMail {
        from_address: e.from.clone(),
        from_name: None,
        to: e.to.clone(),
        reply_to: None,
        subject: render::email_subject(ctx),
        // Text, not Alternative: alert mail stays byte-identical to what it has
        // always been. Rendering it through the new HTML layout is an obvious
        // follow-up and an obvious way to break six channel kinds at once.
        body: MailBody::Text(render::email_body(ctx, message)),
    };

    sauron_mail::send(&params, &mail)
        .await
        .map_err(|err| err.to_string())
}
```

  Note the deliberate tightening this carries: `e.from` and each `e.to` are now parsed as a bare `Address` rather than as a `Mailbox`, so a channel configured with a display-name form (`Sauron <alerts@corp.test>`) is rejected at send with `invalid from address: ...` instead of accepted. That is the same parser the outbox uses, and accepting two different address grammars on two paths into one relay is how a header-injection barrier stops being one. Call it out in the release note.

- [ ] **Step 4: Replace the engine's inline predicate.**
  In `backend/crates/sauron-alerts/src/engine.rs`, replace lines 209-213:

```rust
                    // Config-level errors (SSRF-blocked, bad address) won't heal
                    // with retries; only transient transport errors are retried.
                    let transient = e.contains("request failed")
                        || e.contains("HTTP 5")
                        || e.contains("HTTP 429")
                        || e.contains("smtp send failed");
```

  with:

```rust
                    // Config-level errors (SSRF-blocked, bad address) won't heal
                    // with retries; only transient transport errors are retried.
                    // The four substrings moved into `sauron-mail` alongside the
                    // errors that produce them, because "improving" one of those
                    // error strings would otherwise stop every alert email
                    // retrying with nothing failing to compile.
                    let transient = sauron_mail::is_transient(&e);
```

- [ ] **Step 5: Drop the `lettre` dependency and watch the build break if anything still uses it.**
  In `backend/crates/sauron-alerts/Cargo.toml`, delete the line `lettre = { workspace = true }`.
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`
  Expected: exit 0. If it reports `use of undeclared crate or module lettre`, an import survived Step 3.
  The workspace `lettre` entry in `backend/Cargo.toml` is **untouched** — same version, same five features, still rustls-only — it simply moves one crate down. The set of binaries linking it is unchanged, because `sauron-api` already linked it transitively.

- [ ] **Step 6: Run the alerts and mail tests together.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts -p sauron-mail`
  Expected: both crates `ok`, with `deliver::tests::urlencode_escapes_matrix_specials`, `render::tests::matrix_html_escapes_user_content` and `engine::tests::every_mail_error_variant_keeps_the_retry_behaviour_it_had_before_the_move` all passing.

- [ ] **Step 7: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 9: Repository — enqueue, claim, heartbeat, and the two terminal marks

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (append a new `// === Transactional email outbox ===` section at the end of the file, after the last existing function)
- Modify `backend/crates/sauron-db/tests/mail_outbox.rs` (append tests; delete the `_unused` shim only after Task 10)

**Interfaces:**
- Consumes: `sauron_db::models::{MailOutbox, NewMailOutbox}` (Task 1).
- Produces:
  - `pub async fn enqueue_mail(conn: &mut AsyncPgConnection, row: NewMailOutbox<'_>, ttl_secs: i64, dedup_secs: i64, commit: bool) -> QueryResult<Option<Uuid>>`
  - `pub async fn claim_due_mail(conn: &mut AsyncPgConnection, batch: i64) -> QueryResult<Vec<MailOutbox>>`
  - `pub async fn heartbeat_mail(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize>`
  - `pub async fn mark_mail_sent(conn: &mut AsyncPgConnection, id: Uuid, attempts: i32, sink: bool) -> QueryResult<usize>`
  - `pub async fn mark_mail_failed(conn: &mut AsyncPgConnection, id: Uuid, attempts: i32, error: &str, permanent: bool) -> QueryResult<usize>`

- [ ] **Step 1: Write the failing integration tests.**
  Append to `backend/crates/sauron-db/tests/mail_outbox.rs`:

```rust
use sauron_db::models::NewMailOutbox;
use sauron_db::repo;
use uuid::Uuid;

/// A `NewMailOutbox` with every field at a recognisable value.
fn new_row<'a>(kind: &'a str, recipient_key: &'a str) -> NewMailOutbox<'a> {
    NewMailOutbox {
        kind,
        recipient: recipient_key,
        recipient_key,
        subject: "Reset your password",
        body_text: "plain body with a token",
        body_html: "<p>html body with a token</p>",
        user_id: None,
    }
}

async fn status_of(conn: &mut sauron_db::AsyncPgConnection, id: Uuid) -> String {
    #[derive(diesel::QueryableByName)]
    struct S {
        #[diesel(sql_type = Text)]
        status: String,
    }
    let row: S = diesel::sql_query("SELECT status FROM mail_outbox WHERE id = $1")
        .bind::<SqlUuid, _>(id)
        .get_result(conn)
        .await
        .expect("status");
    row.status
}

async fn count_rows(conn: &mut sauron_db::AsyncPgConnection) -> i64 {
    #[derive(diesel::QueryableByName)]
    struct C {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let row: C = diesel::sql_query("SELECT count(*)::bigint AS n FROM mail_outbox")
        .get_result(conn)
        .await
        .expect("count");
    row.n
}

#[tokio::test]
async fn happy_path_enqueue_claim_send_scrubs_the_credential() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");

    let row: MailOutbox = mail_outbox::table
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("select");
    assert_eq!(row.status, "pending");
    assert_eq!(row.attempts, 0);
    assert!(row.next_attempt_at <= Utc::now());

    let claimed = repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, id);
    assert_eq!(claimed[0].status, "sending");
    assert_eq!(claimed[0].attempts, 1);
    // The claim returns the body BY VALUE, which is what makes it safe for the
    // hygiene sweep to blank a row a drainer is mid-send on.
    assert_eq!(claimed[0].body_text, "plain body with a token");

    assert_eq!(
        repo::mark_mail_sent(&mut conn, id, 1, false)
            .await
            .expect("mark sent"),
        1
    );

    let row: MailOutbox = mail_outbox::table
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("select");
    assert_eq!(row.status, "sent");
    assert!(row.sent_at.is_some());
    // The assertion that matters: a delivered row holds no credential.
    assert_eq!(row.body_text, "");
    assert_eq!(row.body_html, "");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_sink_delivery_is_never_reported_as_sent() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(&mut conn, new_row("smtp_test", "op@corp.test"), 300, 0, true)
        .await
        .expect("enqueue")
        .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    repo::mark_mail_sent(&mut conn, id, 1, true)
        .await
        .expect("mark sink");

    // `status='sent'` is the one observable this whole design offers. A sink row
    // reporting `sent` for mail that was never transmitted makes the single place
    // an operator would look actively lie.
    assert_eq!(status_of(&mut conn, id).await, "sink");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn dedup_suppresses_inside_the_window_and_a_failed_row_does_not_block_a_retry() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let first = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        300,
        true,
    )
    .await
    .expect("first")
    .expect("committed");

    let second = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        300,
        true,
    )
    .await
    .expect("second");
    assert!(second.is_none(), "second enqueue was not suppressed");
    assert_eq!(count_rows(&mut conn).await, 1);

    // A different kind to the same mailbox is a different budget.
    let other = repo::enqueue_mail(
        &mut conn,
        new_row("smtp_test", "victim@corp.test"),
        300,
        300,
        true,
    )
    .await
    .expect("other kind");
    assert!(other.is_some());

    // A permanently-failed attempt must not block a genuine retry.
    diesel::sql_query("UPDATE mail_outbox SET status = 'failed' WHERE id = $1")
        .bind::<SqlUuid, _>(first)
        .execute(&mut conn)
        .await
        .expect("force failed");
    let retry = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        300,
        true,
    )
    .await
    .expect("retry");
    assert!(retry.is_some(), "a failed row suppressed a legitimate retry");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_discard_costs_the_same_round_trip_and_inserts_nothing() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let discarded = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "discard@invalid"),
        3600,
        0,
        false,
    )
    .await
    .expect("discard");
    assert!(discarded.is_none());
    assert_eq!(count_rows(&mut conn).await, 0);

    let committed = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "discard@invalid"),
        3600,
        0,
        true,
    )
    .await
    .expect("commit");
    assert!(committed.is_some());
    assert_eq!(count_rows(&mut conn).await, 1);

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn expiry_comes_from_the_caller_not_from_the_kind() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    // Same kind, two token lifetimes an order of magnitude apart — the exact case
    // a per-kind constant would get wrong, scrubbing a live 24-hour admin reset
    // link at the one-hour mark.
    repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "self@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("self-service")
    .expect("committed");
    repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "admin@corp.test"),
        86_400,
        0,
        true,
    )
    .await
    .expect("admin-initiated")
    .expect("committed");

    let rows: Vec<MailOutbox> = mail_outbox::table
        .select(MailOutbox::as_select())
        .order(mail_outbox::expires_at.asc())
        .load(&mut conn)
        .await
        .expect("load");
    assert_eq!(rows.len(), 2);
    let gap = (rows[1].expires_at - rows[0].expires_at).num_seconds();
    assert!(
        (82_000..=83_000).contains(&gap),
        "expected roughly 23 hours between the two, got {gap}s"
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn concurrent_claims_never_hand_the_same_row_to_two_drainers() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    {
        let mut conn = db.conn().await;
        for i in 0..3 {
            let key = format!("victim{i}@corp.test");
            repo::enqueue_mail(&mut conn, new_row("password_reset", &key), 3600, 0, true)
                .await
                .expect("enqueue")
                .expect("committed");
        }
    }

    // Two separate connections, claiming at the same time. The test pool has
    // exactly two slots, so both are held simultaneously by construction.
    let mut a = db.conn().await;
    let mut b = db.conn().await;
    let (ra, rb) = tokio::join!(
        repo::claim_due_mail(&mut a, 2),
        repo::claim_due_mail(&mut b, 2)
    );
    let mut ids: Vec<Uuid> = ra
        .expect("claim a")
        .into_iter()
        .chain(rb.expect("claim b"))
        .map(|r| r.id)
        .collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "a row was claimed twice");
    assert_eq!(total, 3, "some rows were never claimed");

    drop(a);
    drop(b);
    db.cleanup().await;
}

#[tokio::test]
async fn an_expired_row_is_never_claimed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    diesel::sql_query("UPDATE mail_outbox SET expires_at = now() - interval '1 minute' WHERE id = $1")
        .bind::<SqlUuid, _>(id)
        .execute(&mut conn)
        .await
        .expect("expire");

    // A body that survived its own deadline could never be delivered, only
    // stolen, so refusing it here is what makes the hygiene sweep's blanking free.
    assert!(repo::claim_due_mail(&mut conn, 10)
        .await
        .expect("claim")
        .is_empty());

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_lost_claim_cannot_be_completed_by_the_zombie_that_lost_it() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    // Simulate the row being reclaimed underneath a slow sender: attempts is now 2.
    diesel::sql_query("UPDATE mail_outbox SET attempts = 2 WHERE id = $1")
        .bind::<SqlUuid, _>(id)
        .execute(&mut conn)
        .await
        .expect("bump attempts");

    // Without the `attempts = $2` fence the zombie would blank the body and mark
    // `sent` a row another drainer is mid-send on.
    assert_eq!(
        repo::mark_mail_sent(&mut conn, id, 1, false)
            .await
            .expect("mark"),
        0
    );
    assert_eq!(status_of(&mut conn, id).await, "sending");
    assert_eq!(
        repo::mark_mail_failed(&mut conn, id, 1, "boom", false)
            .await
            .expect("mark"),
        0
    );
    assert_eq!(status_of(&mut conn, id).await, "sending");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn backoff_keeps_the_body_and_giving_up_is_reachable_two_ways() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    // First failure: back to pending, ~30s out, body intact.
    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "a@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    assert_eq!(
        repo::mark_mail_failed(&mut conn, id, 1, "connection reset", false)
            .await
            .expect("fail"),
        1
    );
    let row: MailOutbox = mail_outbox::table
        .filter(mail_outbox::id.eq(id))
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("reload");
    assert_eq!(row.status, "pending");
    assert_eq!(row.last_error.as_deref(), Some("connection reset"));
    let delay = (row.next_attempt_at - Utc::now()).num_seconds();
    assert!((25..=35).contains(&delay), "expected ~30s, got {delay}s");
    // NOT blanked. Blanking on failure is what made a misclassification
    // irreversible; the expiry sweep covers the credential instead, and until
    // then an operator can requeue the row by hand.
    assert_eq!(row.body_text, "plain body with a token");

    // Exhausting max_attempts gives up.
    diesel::sql_query(
        "UPDATE mail_outbox SET status = 'sending', attempts = max_attempts WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .execute(&mut conn)
    .await
    .expect("exhaust");
    assert_eq!(
        repo::mark_mail_failed(&mut conn, id, 8, "connection reset", false)
            .await
            .expect("fail"),
        1
    );
    assert_eq!(status_of(&mut conn, id).await, "failed");

    // A permanent error gives up on the first attempt without consuming the rest.
    let id2 = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "b@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    assert_eq!(
        repo::mark_mail_failed(&mut conn, id2, 1, "550 no such user", true)
            .await
            .expect("fail"),
        1
    );
    assert_eq!(status_of(&mut conn, id2).await, "failed");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_heartbeat_moves_updated_at_only_while_the_row_is_sending() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    // Pending: nothing to keep alive.
    assert_eq!(repo::heartbeat_mail(&mut conn, id).await.expect("hb"), 0);

    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    diesel::sql_query("UPDATE mail_outbox SET updated_at = now() - interval '10 minutes' WHERE id = $1")
        .bind::<SqlUuid, _>(id)
        .execute(&mut conn)
        .await
        .expect("age it");
    assert_eq!(repo::heartbeat_mail(&mut conn, id).await.expect("hb"), 1);

    let row: MailOutbox = mail_outbox::table
        .filter(mail_outbox::id.eq(id))
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("reload");
    assert!((Utc::now() - row.updated_at).num_seconds() < 5);

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run them and watch them fail.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test mail_outbox`
  Expected: `error[E0425]: cannot find function enqueue_mail in module repo` and four more of the same.

- [ ] **Step 3: Add `enqueue_mail`.**
  Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
// ===========================================================================
// Transactional email outbox
// ===========================================================================

/// Queue one rendered message, subject to a per-recipient suppression window,
/// and optionally throw it away without telling the caller.
///
/// One statement, no `conn.transaction`: the dedup probe and the INSERT have to
/// be atomic, and `INSERT ... SELECT ... WHERE` gives that for free.
///
/// `ttl_secs` is the CALLER'S, not the kind's. The only code that knows how long
/// a body is worth delivering is whatever minted the credential inside it, and
/// `password_reset` alone spans two token lifetimes an order of magnitude apart.
///
/// `dedup_secs` is the only chokepoint where a per-recipient cap can live. The
/// `status <> 'failed'` term means a permanently-failed attempt does not block a
/// genuine retry. `0` disables suppression.
///
/// `commit` is how the timing oracle is closed. `enqueue` is only reachable when
/// a user row was found, so without it an existing address pays a render plus a
/// round trip and an unknown address pays nothing — the same class of gap
/// `spend_dummy_verify` exists to close on the login path. `commit = false` runs
/// the same statement, against the same index, over the network, and inserts
/// nothing. The honest claim is not "identical cost"; it is that the SMTP round
/// trip is off the request path entirely and the enqueue itself costs one round
/// trip either way, leaving only a planner-level difference orders of magnitude
/// below network jitter.
pub async fn enqueue_mail(
    conn: &mut AsyncPgConnection,
    row: NewMailOutbox<'_>,
    ttl_secs: i64,
    dedup_secs: i64,
    commit: bool,
) -> QueryResult<Option<Uuid>> {
    #[derive(QueryableByName)]
    struct Inserted {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
    }

    let inserted: Vec<Inserted> = diesel::sql_query(
        "INSERT INTO mail_outbox (kind, recipient, recipient_key, subject, body_text, \
                                  body_html, user_id, expires_at) \
         SELECT $1, $2, $3, $4, $5, $6, $7, now() + make_interval(secs => $8::double precision) \
          WHERE $10 \
            AND ($9 = 0 OR NOT EXISTS ( \
                  SELECT 1 FROM mail_outbox \
                   WHERE kind = $1 AND recipient_key = $3 AND status <> 'failed' \
                     AND created_at > now() - make_interval(secs => $9::double precision))) \
         RETURNING id",
    )
    .bind::<Text, _>(row.kind)
    .bind::<Text, _>(row.recipient)
    .bind::<Text, _>(row.recipient_key)
    .bind::<Text, _>(row.subject)
    .bind::<Text, _>(row.body_text)
    .bind::<Text, _>(row.body_html)
    .bind::<Nullable<SqlUuid>, _>(row.user_id)
    .bind::<BigInt, _>(ttl_secs)
    .bind::<BigInt, _>(dedup_secs)
    .bind::<Bool, _>(commit)
    .get_results(conn)
    .await?;

    Ok(inserted.into_iter().next().map(|r| r.id))
}
```

- [ ] **Step 4: Add `claim_due_mail` and `heartbeat_mail`.**
  Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// Atomically claim due messages and flip them to `sending` so no other drainer
/// picks the same rows.
///
/// Shape copied from `claim_due_monitors`, the concurrency-safe worker pattern
/// this repository already uses. There are zero advisory locks in this codebase
/// and this does not introduce the first one: a lock held by a process killed
/// with SIGKILL has no owner to release it, and nothing here handles SIGTERM.
///
/// `expires_at > now()` is what stops a stale message being delivered on
/// authorization that has since been revoked — a digest rendered at enqueue is a
/// snapshot, and the drain cannot consult `role_grants` because the body is
/// already rendered.
pub async fn claim_due_mail(
    conn: &mut AsyncPgConnection,
    batch: i64,
) -> QueryResult<Vec<MailOutbox>> {
    diesel::sql_query(
        "UPDATE mail_outbox SET status = 'sending', attempts = attempts + 1, updated_at = now() \
         WHERE id IN ( \
             SELECT id FROM mail_outbox \
              WHERE status = 'pending' AND next_attempt_at <= now() AND expires_at > now() \
              ORDER BY next_attempt_at FOR UPDATE SKIP LOCKED LIMIT $1 \
         ) RETURNING *",
    )
    .bind::<BigInt, _>(batch)
    .get_results(conn)
    .await
}

/// Push a claimed row's `updated_at` forward immediately before its send.
///
/// This is what makes the stale-row threshold independent of the batch size and
/// the send concurrency: without it, the last row in a batch can sit for the
/// whole batch's duration before its send even starts, and the next person to
/// tune those two numbers without re-deriving the threshold reintroduces a
/// duplicate reset email.
pub async fn heartbeat_mail(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::sql_query("UPDATE mail_outbox SET updated_at = now() WHERE id = $1 AND status = 'sending'")
        .bind::<SqlUuid, _>(id)
        .execute(conn)
        .await
}
```

- [ ] **Step 5: Add `mark_mail_sent` and `mark_mail_failed`.**
  Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// Complete a claimed row and scrub its body.
///
/// The `status = 'sending' AND attempts = $2` fence is load-bearing: without it a
/// slow drainer whose row was reclaimed underneath it can blank and mark `sent` a
/// row another drainer is mid-send on. Returns the affected count so the caller
/// can log a lost claim at `warn!` rather than silently doing nothing.
///
/// `sink` writes `status = 'sink'`, never `'sent'` — `sent` is the one observable
/// this design offers, and a sink row reporting it makes the single place an
/// operator would look actively lie.
pub async fn mark_mail_sent(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    attempts: i32,
    sink: bool,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox \
            SET status = CASE WHEN $3 THEN 'sink' ELSE 'sent' END, \
                sent_at = now(), updated_at = now(), \
                body_text = '', body_html = '', \
                last_error = CASE WHEN $3 THEN 'delivered to log sink (SMTP_SINK=1)' ELSE NULL END \
          WHERE id = $1 AND status = 'sending' AND attempts = $2",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Integer, _>(attempts)
    .bind::<Bool, _>(sink)
    .execute(conn)
    .await
}

/// Record a failed attempt: back to `pending` with backoff, or `failed`.
///
/// Ladder: 30/60/120/240/480/900/900 seconds, about 45 minutes of coverage at the
/// default `max_attempts` of 8. The exponent is clamped at 6 because
/// `POWER(2, attempts - 1)::int` overflows an `int` once an operator hand-bumps
/// `max_attempts` past ~38 — and the clamp changes nothing below that, since
/// `LEAST(900, ...)` has already flattened the ladder by then.
///
/// It deliberately does NOT blank the body. Blanking on failure is what made a
/// misclassification irreversible; the expiry sweep covers the credential
/// instead, and until `expires_at` passes an operator can requeue the row by hand.
pub async fn mark_mail_failed(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    attempts: i32,
    error: &str,
    permanent: bool,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox \
            SET status = CASE WHEN $4 OR attempts >= max_attempts THEN 'failed' ELSE 'pending' END, \
                last_error = $3, \
                next_attempt_at = now() + make_interval(secs => \
                    LEAST(900, (30 * POWER(2, LEAST(GREATEST(attempts - 1, 0), 6)))::int)), \
                updated_at = now() \
          WHERE id = $1 AND status = 'sending' AND attempts = $2",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Integer, _>(attempts)
    .bind::<Text, _>(error)
    .bind::<Bool, _>(permanent)
    .execute(conn)
    .await
}
```

- [ ] **Step 6: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test mail_outbox`
  Expected: `test result: ok. 12 passed`.
  If `enqueue_mail` fails with `operator does not exist: bigint = integer` or a `make_interval` argument error, the `::double precision` casts in Step 3 were dropped — Postgres infers a bound parameter's type from the declared bind, and `make_interval(secs => ...)` wants `double precision`.

- [ ] **Step 7: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 10: Repository — orphan recovery, expiry, body scrubbing, retention, depth

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (append to the `Transactional email outbox` section added in Task 9)
- Modify `backend/crates/sauron-db/tests/mail_outbox.rs` (append tests; delete the `_unused` shim and any now-unused imports)

**Interfaces:**
- Consumes: `enqueue_mail`, `claim_due_mail` (Task 9).
- Produces:
  - `pub async fn requeue_stuck_mail(conn: &mut AsyncPgConnection, stale_secs: i64) -> QueryResult<usize>`
  - `pub async fn expire_stale_mail(conn: &mut AsyncPgConnection) -> QueryResult<usize>`
  - `pub async fn blank_expired_mail_bodies(conn: &mut AsyncPgConnection) -> QueryResult<usize>`
  - `pub async fn prune_mail_outbox(conn: &mut AsyncPgConnection, older_than_days: i64, batch: i64) -> QueryResult<usize>`
  - `pub async fn mail_outbox_depth(conn: &mut AsyncPgConnection) -> QueryResult<(i64, Option<i64>)>`

- [ ] **Step 1: Write the failing integration tests.**
  Append to `backend/crates/sauron-db/tests/mail_outbox.rs`:

```rust
#[tokio::test]
async fn a_row_orphaned_mid_send_is_requeued_with_backoff_and_can_still_give_up() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    diesel::sql_query("UPDATE mail_outbox SET updated_at = now() - interval '10 minutes' WHERE id = $1")
        .bind::<SqlUuid, _>(id)
        .execute(&mut conn)
        .await
        .expect("age it");

    assert_eq!(
        repo::requeue_stuck_mail(&mut conn, 300)
            .await
            .expect("requeue"),
        1
    );
    let row: MailOutbox = mail_outbox::table
        .filter(mail_outbox::id.eq(id))
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("reload");
    assert_eq!(row.status, "pending");
    // Without resetting next_attempt_at, a requeued row is immediately eligible
    // for the very next claim, bypassing the backoff ladder entirely on exactly
    // the path that most needs it.
    assert!(
        row.next_attempt_at > Utc::now(),
        "requeued row bypassed the backoff ladder"
    );

    // A row whose send reliably kills the process must eventually be given up on.
    // The give-up decision otherwise lives only in `mark_mail_failed`, which a
    // process that crashed or was OOM-killed never reaches, so the row would be
    // claimed → orphaned → requeued → claimed, forever.
    diesel::sql_query(
        "UPDATE mail_outbox SET status = 'sending', attempts = max_attempts, \
                updated_at = now() - interval '10 minutes' WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .execute(&mut conn)
    .await
    .expect("exhaust");
    assert_eq!(
        repo::requeue_stuck_mail(&mut conn, 300)
            .await
            .expect("requeue"),
        1
    );
    assert_eq!(status_of(&mut conn, id).await, "failed");

    // A row that is still being worked on is untouched.
    let fresh = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "other@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    assert_eq!(
        repo::requeue_stuck_mail(&mut conn, 300)
            .await
            .expect("requeue"),
        0
    );
    assert_eq!(status_of(&mut conn, fresh).await, "sending");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn expiry_sweep_fails_the_row_and_body_scrubbing_keys_off_the_rows_own_deadline() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    // One expired, one with a live 24-hour deadline.
    let expired = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "expired@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    let live = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "live@corp.test"),
        86_400,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    diesel::sql_query("UPDATE mail_outbox SET expires_at = now() - interval '1 minute' WHERE id = $1")
        .bind::<SqlUuid, _>(expired)
        .execute(&mut conn)
        .await
        .expect("expire");

    assert_eq!(
        repo::blank_expired_mail_bodies(&mut conn)
            .await
            .expect("blank"),
        1
    );
    let rows: Vec<MailOutbox> = mail_outbox::table
        .select(MailOutbox::as_select())
        .order(mail_outbox::expires_at.asc())
        .load(&mut conn)
        .await
        .expect("load");
    assert_eq!(rows[0].id, expired);
    assert_eq!(rows[0].body_text, "");
    assert_eq!(rows[0].body_html, "");
    // Status untouched: blanking is not a state transition.
    assert_eq!(rows[0].status, "pending");
    // THE assertion that catches anyone reintroducing a flat age cutoff and
    // scrubbing a live 24-hour admin reset mail an hour after it was queued.
    assert_eq!(rows[1].id, live);
    assert_eq!(rows[1].body_text, "plain body with a token");

    assert_eq!(repo::expire_stale_mail(&mut conn).await.expect("expire"), 1);
    let row: MailOutbox = mail_outbox::table
        .filter(mail_outbox::id.eq(expired))
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("reload");
    assert_eq!(row.status, "failed");
    assert_eq!(row.last_error.as_deref(), Some("expired before delivery"));
    assert_eq!(status_of(&mut conn, live).await, "pending");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn retention_deletes_only_terminal_rows_and_reports_queue_depth() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    for (key, status) in [
        ("sent@corp.test", "sent"),
        ("failed@corp.test", "failed"),
        ("sink@corp.test", "sink"),
        ("pending@corp.test", "pending"),
        ("sending@corp.test", "sending"),
    ] {
        let id = repo::enqueue_mail(&mut conn, new_row("smtp_test", key), 3600, 0, true)
            .await
            .expect("enqueue")
            .expect("committed");
        diesel::sql_query("UPDATE mail_outbox SET status = $2 WHERE id = $1")
            .bind::<SqlUuid, _>(id)
            .bind::<Text, _>(status)
            .execute(&mut conn)
            .await
            .expect("force status");
    }

    // Age 0 days, so every terminal row is eligible regardless of clock skew.
    assert_eq!(
        repo::prune_mail_outbox(&mut conn, 0, 5000)
            .await
            .expect("prune"),
        3
    );
    assert_eq!(count_rows(&mut conn).await, 2);
    // A second pass returns 0, which is the loop's termination condition.
    assert_eq!(
        repo::prune_mail_outbox(&mut conn, 0, 5000)
            .await
            .expect("prune"),
        0
    );

    let (pending, oldest) = repo::mail_outbox_depth(&mut conn).await.expect("depth");
    assert_eq!(pending, 1);
    assert!(oldest.is_some());

    diesel::sql_query("DELETE FROM mail_outbox WHERE status = 'pending'")
        .execute(&mut conn)
        .await
        .expect("clear");
    let (pending, oldest) = repo::mail_outbox_depth(&mut conn).await.expect("depth");
    assert_eq!(pending, 0);
    assert!(oldest.is_none(), "an empty queue has no oldest row");

    drop(conn);
    db.cleanup().await;
}
```

  Then delete the `_unused` shim and the `#[allow(dead_code)]` line at the bottom of the file — every import it existed to keep alive now has a real caller.

- [ ] **Step 2: Run them and watch them fail.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test mail_outbox`
  Expected: `error[E0425]: cannot find function requeue_stuck_mail in module repo` and four more of the same.

- [ ] **Step 3: Add `requeue_stuck_mail`.**
  Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// Recover rows orphaned by a process killed mid-send.
///
/// Nothing else ever reclaims them: `claim_due_mail` only looks at `pending`.
///
/// Three guards, each covering a failure the obvious version has. The
/// `attempts >= max_attempts` branch exists because the give-up decision
/// otherwise lives only in `mark_mail_failed`, which a process that crashed or
/// was OOM-killed never reaches — so a row whose send reliably kills the process
/// would be claimed, orphaned, requeued and claimed again, forever. Resetting
/// `next_attempt_at` exists because a requeued row is otherwise immediately
/// eligible for the very next claim, bypassing the backoff ladder on exactly the
/// path that most needs it. And the `updated_at` window is what the per-send
/// heartbeat keeps honest.
pub async fn requeue_stuck_mail(
    conn: &mut AsyncPgConnection,
    stale_secs: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox \
            SET status = CASE WHEN attempts >= max_attempts THEN 'failed' ELSE 'pending' END, \
                last_error = CASE WHEN attempts >= max_attempts \
                             THEN 'orphaned mid-send ' || attempts || ' times; giving up' \
                             ELSE 'orphaned mid-send; requeued' END, \
                next_attempt_at = now() + make_interval(secs => \
                    LEAST(900, (30 * POWER(2, LEAST(GREATEST(attempts - 1, 0), 6)))::int)), \
                updated_at = now() \
          WHERE status = 'sending' AND updated_at < now() - make_interval(secs => $1::double precision)",
    )
    .bind::<BigInt, _>(stale_secs)
    .execute(conn)
    .await
}
```

- [ ] **Step 4: Add the two expiry sweeps.**
  Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// Fail every non-terminal row whose own deadline has passed.
///
/// Neither this nor [`blank_expired_mail_bodies`] is indexed: the non-terminal
/// set is small by construction, and every status transition already rewrites two
/// partial indexes, so a fifth index costs more than these sweeps save.
pub async fn expire_stale_mail(conn: &mut AsyncPgConnection) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox \
            SET status = 'failed', last_error = 'expired before delivery', updated_at = now() \
          WHERE status IN ('pending', 'sending') AND expires_at < now()",
    )
    .execute(conn)
    .await
}

/// Scrub the body of any row past its own `expires_at`, whatever its status.
///
/// Takes no age argument on purpose. The row already carries the only deadline
/// that means anything, and a second flat constant sitting beside it is the drift
/// that scrubs a live 24-hour reset link at the one-hour mark — destroying the
/// manual requeue path while the token it carried stays valid for another 23
/// hours.
///
/// Blanking a row the drain is mid-send on is harmless: `claim_due_mail` returned
/// the body by value, so the sender is working from its own copy.
pub async fn blank_expired_mail_bodies(conn: &mut AsyncPgConnection) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox SET body_text = '', body_html = '', updated_at = now() \
          WHERE (body_text <> '' OR body_html <> '') AND expires_at < now()",
    )
    .execute(conn)
    .await
}
```

- [ ] **Step 5: Add `prune_mail_outbox` and `mail_outbox_depth`.**
  Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// Delete up to `batch` terminal rows older than `older_than_days`. Call in a
/// loop until it returns 0.
///
/// Bounded and non-blocking, unlike `prune_alert_events`, which is an unbounded
/// DELETE — that one runs in a standalone worker, this one runs inside
/// `sauron-api`, which serves HTTP from a 16-connection pool. An operator
/// lowering `MAIL_OUTBOX_RETENTION_DAYS` after a digest run would otherwise hold
/// one of those 16 for minutes.
///
/// The `FOR UPDATE SKIP LOCKED` is also what lets N API instances reap
/// concurrently without serialising on row locks.
pub async fn prune_mail_outbox(
    conn: &mut AsyncPgConnection,
    older_than_days: i64,
    batch: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM mail_outbox WHERE id IN ( \
             SELECT id FROM mail_outbox \
              WHERE status IN ('sent', 'failed', 'sink') \
                AND created_at < now() - ($1 || ' days')::interval \
              ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
    )
    .bind::<Text, _>(older_than_days.to_string())
    .bind::<BigInt, _>(batch)
    .execute(conn)
    .await
}

/// `(pending_count, age_of_oldest_pending_row_in_seconds)`.
///
/// The only queue-depth signal this slice ships, and it is logged
/// unconditionally: there is no metrics endpoint and no admin view, so without it
/// a stalled queue is invisible until a user reports that password reset does not
/// work.
pub async fn mail_outbox_depth(conn: &mut AsyncPgConnection) -> QueryResult<(i64, Option<i64>)> {
    #[derive(QueryableByName)]
    struct Depth {
        #[diesel(sql_type = BigInt)]
        pending: i64,
        #[diesel(sql_type = Nullable<BigInt>)]
        oldest_secs: Option<i64>,
    }

    let row: Depth = diesel::sql_query(
        "SELECT count(*)::bigint AS pending, \
                (EXTRACT(EPOCH FROM (now() - min(created_at))))::bigint AS oldest_secs \
           FROM mail_outbox WHERE status = 'pending'",
    )
    .get_result(conn)
    .await?;
    Ok((row.pending, row.oldest_secs))
}
```

- [ ] **Step 6: Run the tests and watch them pass.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test mail_outbox`
  Expected: `test result: ok. 15 passed`.

- [ ] **Step 7: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 11: The background-task supervisor

**Files:**
- Create `backend/bins/sauron-api/src/tasks.rs`
- Modify `backend/bins/sauron-api/src/main.rs` (add `mod tasks;` to the module list at lines 7-11 only — the mounting happens in Task 13)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct TaskHealth` with `pub fn last_success_secs(&self) -> Option<u64>` and `pub fn consecutive_failures(&self) -> u32`
  - `pub fn supervise<F, Fut>(name: &'static str, interval: Duration, f: F) -> Arc<TaskHealth> where F: Fn() -> Fut + Send + Sync + 'static, Fut: Future<Output = anyhow::Result<()>> + Send + 'static`
  - `pub struct TaskStatus { pub name: &'static str, pub last_success_secs: Option<u64>, pub consecutive_failures: u32 }` deriving `serde::Serialize`
  - `pub fn snapshot() -> Vec<TaskStatus>`

- [ ] **Step 1: Write the failing tests.**
  Create `backend/bins/sauron-api/src/tasks.rs` with only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A panicking task must not stop the loop. In this process the loop is a
    /// detached task whose `JoinHandle` is dropped; the workspace sets no
    /// `panic = "abort"` and nothing installs a panic hook, so tokio catches the
    /// panic and the task simply stops — the HTTP server keeps serving, /health
    /// keeps returning 200, systemd sees a healthy unit, and transactional email
    /// stops forever.
    #[tokio::test]
    async fn a_panicking_tick_does_not_kill_the_loop() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let health = supervise("test_panics", Duration::from_millis(10), || async {
            let n = CALLS.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                panic!("first tick explodes");
            }
            Ok(())
        });

        for _ in 0..200 {
            if health.last_success_secs().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            health.last_success_secs().is_some(),
            "the loop never recovered from a panicking tick"
        );
        assert!(CALLS.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn failures_accumulate_and_a_success_resets_them() {
        static FAIL: AtomicU32 = AtomicU32::new(1);
        let health = supervise("test_failures", Duration::from_millis(10), || async {
            if FAIL.load(Ordering::SeqCst) == 1 {
                anyhow::bail!("still broken");
            }
            Ok(())
        });

        for _ in 0..200 {
            if health.consecutive_failures() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(health.consecutive_failures() >= 2, "failures did not accumulate");
        assert!(health.last_success_secs().is_none());

        FAIL.store(0, Ordering::SeqCst);
        for _ in 0..400 {
            if health.consecutive_failures() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(health.consecutive_failures(), 0, "a success did not reset the counter");
        assert!(health.last_success_secs().is_some());
    }

    #[test]
    fn backoff_grows_with_failures_and_stops_at_five_minutes() {
        let i = Duration::from_secs(60);
        assert_eq!(backoff(i, 0), Duration::from_secs(0));
        assert_eq!(backoff(i, 1), Duration::from_secs(60));
        assert_eq!(backoff(i, 4), Duration::from_secs(240));
        assert_eq!(backoff(i, 5), Duration::from_secs(300));
        assert_eq!(backoff(i, 8), Duration::from_secs(300));
        assert_eq!(backoff(i, 900), Duration::from_secs(300));
    }

    #[tokio::test]
    async fn a_supervised_task_is_visible_on_the_health_snapshot_immediately() {
        // Registration happens synchronously, before the initial jitter sleep.
        // A 15-minute hygiene task that only appeared after its first tick would
        // be indistinguishable from one that was never mounted.
        supervise("test_registered", Duration::from_secs(900), || async { Ok(()) });
        let names: Vec<&str> = snapshot().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"test_registered"), "got: {names:?}");
    }
}
```

- [ ] **Step 2: Declare the module and run the tests.**
  In `backend/bins/sauron-api/src/main.rs`, add `mod tasks;` to the module list (keep it alphabetical: after `mod symbolicate;`).
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api tasks`
  Expected: `cannot find function supervise in this scope`, `cannot find function backoff in this scope`, `cannot find function snapshot in this scope`.

- [ ] **Step 3: Write the supervisor above the test module.**
  Prepend to `backend/bins/sauron-api/src/tasks.rs`:

```rust
//! Supervised background loops for `sauron-api`.
//!
//! **No task's initialization may `?` out of `main()`.** This is the absolute
//! rule, and the blast radius is exact: `packaging/rpm/systemd/
//! sauron-migrate.service` has no `[Install]` section, `sauron.spec` runs
//! `%systemd_postun_with_restart` on the API, and `sauron-api.service` is
//! `Restart=on-failure` with no `StartLimit` override — so a `?` against a table
//! a skipped migration never created burns systemd's five-starts-in-ten-seconds
//! budget and leaves the unit `failed` with no HTTP surface left to diagnose
//! from. Start with an empty state, log at ERROR on every failed tick, and let
//! the `/health` age make it visible.
//!
//! The `tick + last_prune` loop in `bins/sauron-alerts/src/main.rs` looks like
//! the thing to copy and is not: there the loop *is* `main()`, so a panic aborts
//! the process and `Restart=on-failure` brings it back. Here it would be a
//! detached task whose `JoinHandle` is dropped. The workspace sets no
//! `panic = "abort"` and `sauron-telemetry` installs no panic hook, so tokio
//! catches the panic and the task simply stops — the HTTP server keeps serving,
//! `/health` keeps returning 200, systemd sees a healthy unit, and the work stops
//! forever. Hence: each tick is its own `tokio::spawn` whose `JoinHandle` is
//! awaited, so a panic arrives as an `Err(JoinError)` the loop can log.

use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::error;

/// Ceiling on how far a failing task backs off. Long enough to stop hammering a
/// broken dependency, short enough that recovery is noticed within one `/health`
/// glance.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Liveness of one supervised loop.
pub struct TaskHealth {
    name: &'static str,
    last_success: Mutex<Option<Instant>>,
    consecutive_failures: AtomicU32,
}

impl TaskHealth {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            last_success: Mutex::new(None),
            consecutive_failures: AtomicU32::new(0),
        }
    }

    /// Seconds since the last successful tick, or `None` before the first one.
    pub fn last_success_secs(&self) -> Option<u64> {
        let guard = self.last_success.lock().ok()?;
        guard.map(|t| t.elapsed().as_secs())
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    fn record_success(&self) {
        if let Ok(mut g) = self.last_success.lock() {
            *g = Some(Instant::now());
        }
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    fn record_failure(&self) -> u32 {
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// One row of `/health`'s `tasks` array.
#[derive(serde::Serialize)]
pub struct TaskStatus {
    pub name: &'static str,
    /// `null` before the first success. It NEVER changes the status code:
    /// `packaging/rpm/SETUP.md` documents `curl -fsS .../health` and
    /// `tests/http_env_scoping.rs` polls it for readiness, and both read a non-2xx
    /// as "the API is down", which a stalled reaper is not.
    pub last_success_secs: Option<u64>,
    pub consecutive_failures: u32,
}

fn registry() -> &'static Mutex<Vec<Arc<TaskHealth>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Arc<TaskHealth>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Every supervised task's current state, for `/health`.
pub fn snapshot() -> Vec<TaskStatus> {
    let guard = match registry().lock() {
        Ok(g) => g,
        // A poisoned registry means a previous reader panicked while holding it.
        // `/health` must still answer; an empty task list is the honest report.
        Err(_) => return Vec::new(),
    };
    guard
        .iter()
        .map(|h| TaskStatus {
            name: h.name,
            last_success_secs: h.last_success_secs(),
            consecutive_failures: h.consecutive_failures(),
        })
        .collect()
}

fn backoff(interval: Duration, failures: u32) -> Duration {
    std::cmp::min(interval * failures.min(8), MAX_BACKOFF)
}

/// Run `f` every `interval`, forever, surviving panics and errors.
///
/// The returned handle is registered for `/health` synchronously, before the
/// initial jitter sleep, so a 15-minute task is visible from the first request
/// rather than only after its first tick.
pub fn supervise<F, Fut>(name: &'static str, interval: Duration, f: F) -> Arc<TaskHealth>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let health = Arc::new(TaskHealth::new(name));
    if let Ok(mut g) = registry().lock() {
        g.push(health.clone());
    }

    let h = health.clone();
    tokio::spawn(async move {
        // Per-process jitter. With N instances behind a load balancer, a rolling
        // restart otherwise makes all N fire the identical reaper within seconds
        // of each other — N times the lock contention and N times the pool
        // pressure, at the same instant, forever.
        let span_nanos = u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX).max(1);
        let jitter = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64
            % span_nanos;
        tokio::time::sleep(Duration::from_nanos(jitter)).await;

        loop {
            // Each tick is its own spawn so a panic comes back as Err(JoinError)
            // rather than unwinding this loop out of existence.
            match tokio::spawn(f()).await {
                Ok(Ok(())) => {
                    h.record_success();
                    tokio::time::sleep(interval).await;
                }
                Ok(Err(e)) => {
                    let n = h.record_failure();
                    error!(task = name, error = %e, consecutive_failures = n, "background task failed");
                    tokio::time::sleep(backoff(interval, n)).await;
                }
                Err(join) => {
                    let n = h.record_failure();
                    error!(task = name, error = %join, consecutive_failures = n, "background task panicked");
                    tokio::time::sleep(backoff(interval, n)).await;
                }
            }
        }
    });

    health
}
```

- [ ] **Step 4: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api tasks`
  Expected: `test result: ok. 4 passed`.
  `a_panicking_tick_does_not_kill_the_loop` prints a panic backtrace on stderr — that is the panic being caught, not a failure.

- [ ] **Step 5: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0. If clippy reports `dead_code` on `supervise` or `snapshot`, Task 13 has not mounted them yet — add `#![allow(dead_code)]` at the top of `tasks.rs` **only** if the gate blocks, and delete it again at the end of Task 13.

---

## Task 12: `MailSender` — render, enqueue, nudge, drain, hygiene

**Files:**
- Create `backend/bins/sauron-api/src/mail.rs`
- Modify `backend/bins/sauron-api/Cargo.toml` (add `sauron-mail.workspace = true` after `sauron-alerts.workspace = true`)
- Modify `backend/bins/sauron-api/src/main.rs` (add `mod mail;` to the module list — the wiring happens in Task 13)

**Interfaces:**
- Consumes: `sauron_mail::{render, Branding, MailBody, MailContent, MailError, MailKind, OutgoingMail, SmtpClient, SmtpParams, normalize_recipient}` (Tasks 5-7); `sauron_db::repo::{enqueue_mail, claim_due_mail, heartbeat_mail, mark_mail_sent, mark_mail_failed, requeue_stuck_mail, expire_stale_mail, blank_expired_mail_bodies, prune_mail_outbox, mail_outbox_depth}` (Tasks 9-10); `sauron_db::models::NewMailOutbox` (Task 1).
- Produces:
  - `pub struct MailSender` deriving `Clone` (never `Debug` — `params` holds the relay password)
  - `pub fn MailSender::new(pool: PgPool, params: SmtpParams, from_address: String, from_name: String, branding: Branding) -> MailSender`
  - `pub async fn MailSender::enqueue(&self, kind: MailKind, recipient: &str, content: &MailContent, user_id: Option<Uuid>, ttl: Duration) -> anyhow::Result<Option<Uuid>>`
  - `pub async fn MailSender::enqueue_or_discard(&self, kind: MailKind, recipient: Option<&str>, content: &MailContent, user_id: Option<Uuid>, ttl: Duration) -> anyhow::Result<Option<Uuid>>`
  - `pub fn MailSender::nudge(&self)`
  - `pub async fn MailSender::drain_once(&self) -> usize`
  - `pub async fn hygiene(pool: &PgPool, retention_days: i64) -> anyhow::Result<()>` (free function — see reconciliation 2)

- [ ] **Step 1: Add the dependency.**
  In `backend/bins/sauron-api/Cargo.toml`, after `sauron-alerts.workspace = true`, add:

```toml
sauron-mail.workspace = true
```

- [ ] **Step 2: Write the failing unit tests.**
  Create `backend/bins/sauron-api/src/mail.rs` with only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn params(total_deadline_secs: u64) -> SmtpParams {
        SmtpParams {
            host: "smtp.example.test".into(),
            port: 587,
            username: None,
            password: None,
            tls: sauron_mail::SmtpTls::StartTls,
            allow_private: false,
            op_timeout: Duration::from_secs(10),
            total_deadline: Duration::from_secs(total_deadline_secs),
            sink: false,
            sink_log_body: false,
        }
    }

    /// A hardcoded stale threshold with a tunable batch size and a tunable
    /// timeout is how a drain robs its own sibling mid-send and a user gets two
    /// reset emails. It is derived from both.
    #[test]
    fn stale_threshold_is_derived_from_batch_concurrency_and_deadline() {
        // Defaults: (16 / 4) * 30 * 2 + 60 = 300.
        assert_eq!(stale_secs(&params(30)), 300);
        // Double the deadline, double the window it must cover.
        assert_eq!(stale_secs(&params(60)), 540);
        // And it is always strictly larger than one batch's worst-case hold.
        let worst_case = (BATCH as u64 / SEND_CONCURRENCY as u64) * 30;
        assert!(stale_secs(&params(30)) as u64 > worst_case);
    }

    /// The drain's ladder and the alerting path's string predicate disagree on
    /// purpose. Classifying Dns/Tls as permanent — as an earlier draft did — meant
    /// a 20-second resolver hiccup during a nightly restart marked every row in
    /// that window `failed` after one attempt.
    #[test]
    fn drain_retries_transport_faults_and_gives_up_on_configuration_faults() {
        assert!(is_retryable(&MailError::Send("connection reset".into())));
        assert!(is_retryable(&MailError::DeadlineExceeded(30_000)));
        assert!(is_retryable(&MailError::Dns("DNS resolution failed: x".into())));
        assert!(is_retryable(&MailError::Tls("handshake failed".into())));

        assert!(!is_retryable(&MailError::Rejected("550 no such user".into())));
        assert!(!is_retryable(&MailError::InvalidFrom("x".into())));
        assert!(!is_retryable(&MailError::InvalidRecipient("x".into())));
        assert!(!is_retryable(&MailError::Build("x".into())));
        assert!(!is_retryable(&MailError::Blocked("x".into())));
    }

    #[test]
    fn a_missing_table_is_reported_once_and_names_the_migration_step() {
        // The exact symptom an RPM upgrade produces: new binary, old schema. The
        // opaque diesel error repeated every 60 seconds tells an operator nothing.
        assert!(looks_like_missing_outbox(
            "relation \"mail_outbox\" does not exist"
        ));
        assert!(!looks_like_missing_outbox("column x does not exist"));
        assert!(!looks_like_missing_outbox("connection closed"));
    }
}
```

- [ ] **Step 3: Declare the module and run the tests.**
  In `backend/bins/sauron-api/src/main.rs`, add `mod mail;` to the module list (alphabetical: after `mod error;`).
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api mail`
  Expected: `cannot find function stale_secs in this scope`, `cannot find function is_retryable in this scope`, `cannot find function looks_like_missing_outbox in this scope`, `cannot find value BATCH in this scope`.

- [ ] **Step 4: Write the struct, the constants and the pure helpers.**
  Prepend to `backend/bins/sauron-api/src/mail.rs`:

```rust
//! Orchestration for transactional email: render at enqueue, queue durably,
//! drain off the request path.
//!
//! Sits alongside `admin_storage.rs` / `symbolicate.rs` / `tier_read.rs`, the
//! house pattern for orchestration that is neither a route nor a repo function.
//!
//! Rendering happens at ENQUEUE, not at send. The body is then fixed at request
//! time, a template error surfaces to a handler that can report it instead of
//! inside a retry loop that will only fail eight times, and the drain becomes
//! pure I/O with nothing fallible but the network.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sauron_db::models::NewMailOutbox;
use sauron_db::{repo, PgPool};
use sauron_mail::{
    normalize_recipient, render, Branding, MailBody, MailContent, MailError, MailKind,
    OutgoingMail, SmtpClient, SmtpParams,
};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Rows claimed per pass. Small enough that one batch's worst-case hold stays
/// well inside the stale threshold derived below.
const BATCH: i64 = 16;
/// Sends in flight at once. Mirrors `monitor_max_concurrency`'s existence, and
/// keeps at most four short connection checkouts live out of the process's 16.
const SEND_CONCURRENCY: usize = 4;
/// Wall clock one drain tick may spend, so a backlog actually drains instead of
/// moving 16 messages a minute — but cannot monopolise the process either.
const DRAIN_BUDGET: Duration = Duration::from_secs(300);
/// Concurrent drains permitted. A third `nudge` under a burst is a no-op, because
/// the `SKIP LOCKED` claim will pick its row up anyway.
const DRAIN_SLOTS: usize = 2;
/// Rows deleted per retention pass; the loop repeats until a pass returns 0.
const PRUNE_BATCH: i64 = 500;
/// Address used for a discarded enqueue. `.invalid` is RFC 2606 reserved, so it
/// can never be a real mailbox even if a row somehow escaped.
const DISCARD_RECIPIENT: &str = "discard@invalid";

/// How long a claimed row may go without a heartbeat before another drain
/// reclaims it.
///
/// Derived, not hardcoded. With the defaults this is 300 seconds — the same
/// number a hardcoded constant would have given, but now provably larger than one
/// batch's worst-case hold. A hardcoded constant with a tunable batch size and a
/// tunable timeout is how a drain robs its own sibling and a user gets two reset
/// emails.
fn stale_secs(params: &SmtpParams) -> i64 {
    let waves = (BATCH as u64).div_ceil(SEND_CONCURRENCY as u64);
    (waves * params.total_deadline.as_secs() * 2 + 60) as i64
}

/// Whether the drain should schedule another attempt.
///
/// Deliberately different from `sauron_mail::is_transient`, which the alerting
/// path uses: the drain owns its own ladder and can afford to burn 45 minutes on
/// a genuinely broken relay, while alerting keeps its string predicate
/// byte-compatible so its behaviour is unchanged.
fn is_retryable(e: &MailError) -> bool {
    matches!(
        e,
        MailError::Send(_) | MailError::DeadlineExceeded(_) | MailError::Dns(_) | MailError::Tls(_)
    )
}

/// Postgres reports a missing relation as SQLSTATE 42P01; diesel has no variant
/// for it, so the message is the only signal available.
fn looks_like_missing_outbox(msg: &str) -> bool {
    msg.contains("mail_outbox") && msg.contains("does not exist")
}

/// Log the missing-table diagnosis once rather than the same opaque diesel error
/// every 60 seconds. This is the exact symptom an RPM upgrade produces: upgrades
/// never re-run `sauron-migrate`, so a new binary meets an old schema.
fn report_db_error(context: &'static str, e: &diesel::result::Error) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REPORTED: AtomicBool = AtomicBool::new(false);

    let msg = e.to_string();
    // The `return` sits outside the `swap` on purpose. Folding both conditions
    // into one `if` sends every tick after the first back down to the `warn!`,
    // which is the per-60-second stream of opaque diesel text this function
    // exists to replace: the diagnosis is logged once, and the symptom is then
    // silent rather than merely quieter.
    if looks_like_missing_outbox(&msg) {
        if !REPORTED.swap(true, Ordering::Relaxed) {
            error!(
                "mail_outbox does not exist — this deployment was upgraded without running \
                 sauron-migrate. Stop sauron-api, run `systemctl start sauron-migrate`, then \
                 start it again (packaging/rpm/SETUP.md section 11)."
            );
        }
        return;
    }
    warn!(context, error = %msg, "mail outbox query failed");
}

/// Renders, enqueues and drains transactional email.
///
/// Never `Debug`: `params` holds the relay password.
#[derive(Clone)]
pub struct MailSender {
    pool: PgPool,
    params: Arc<SmtpParams>,
    from_address: Arc<str>,
    from_name: Arc<str>,
    branding: Arc<Branding>,
    drain_slots: Arc<Semaphore>,
}
```

- [ ] **Step 5: Write the enqueue side.**
  Append to `backend/bins/sauron-api/src/mail.rs`, before the `#[cfg(test)]` block:

```rust
impl MailSender {
    pub fn new(
        pool: PgPool,
        params: SmtpParams,
        from_address: String,
        from_name: String,
        branding: Branding,
    ) -> MailSender {
        MailSender {
            pool,
            params: Arc::new(params),
            from_address: Arc::from(from_address.as_str()),
            from_name: Arc::from(from_name.as_str()),
            branding: Arc::new(branding),
            drain_slots: Arc::new(Semaphore::new(DRAIN_SLOTS)),
        }
    }

    /// Render and queue one message.
    ///
    /// `ttl` is the CALLER'S credential lifetime, not a round number. It becomes
    /// `expires_at`, which then governs three separate things: whether the drain
    /// will still send the row, when the hygiene sweep scrubs its body, and how
    /// long an operator has to requeue it by hand. A sender that passes a lifetime
    /// shorter than the token it just minted throws away its own recovery path;
    /// one that passes a longer one leaves a working credential in Postgres after
    /// the token it carries is dead.
    ///
    /// Returns `anyhow::Error`, not `ApiError`, so a caller that must return a
    /// fixed 200 whatever happens can swallow it.
    pub async fn enqueue(
        &self,
        kind: MailKind,
        recipient: &str,
        content: &MailContent,
        user_id: Option<Uuid>,
        ttl: Duration,
    ) -> anyhow::Result<Option<Uuid>> {
        self.enqueue_inner(kind, Some(recipient), content, user_id, ttl)
            .await
    }

    /// Render and queue, or render and throw away, at identical cost.
    ///
    /// `Ok(None)` covers both a dedup suppression and a deliberate discard, so the
    /// caller cannot distinguish them either — which is the point. A handler that
    /// branches on whether the recipient exists BEFORE calling this reopens the
    /// enumeration oracle this closes.
    pub async fn enqueue_or_discard(
        &self,
        kind: MailKind,
        recipient: Option<&str>,
        content: &MailContent,
        user_id: Option<Uuid>,
        ttl: Duration,
    ) -> anyhow::Result<Option<Uuid>> {
        self.enqueue_inner(kind, recipient, content, user_id, ttl)
            .await
    }

    async fn enqueue_inner(
        &self,
        kind: MailKind,
        recipient: Option<&str>,
        content: &MailContent,
        user_id: Option<Uuid>,
        ttl: Duration,
    ) -> anyhow::Result<Option<Uuid>> {
        // Rendered unconditionally, including on the discard path: the render is
        // the expensive half and skipping it is a measurable difference a caller
        // can time.
        let rendered = render(&self.branding, content)?;
        let commit = recipient.is_some();
        let raw = recipient.unwrap_or(DISCARD_RECIPIENT);
        let key = normalize_recipient(raw)?;

        let mut conn = sauron_db::conn(&self.pool).await?;
        let id = repo::enqueue_mail(
            &mut conn,
            NewMailOutbox {
                kind: kind.as_str(),
                recipient: raw,
                recipient_key: &key,
                subject: &rendered.subject,
                body_text: &rendered.text,
                body_html: &rendered.html,
                user_id,
            },
            ttl.as_secs() as i64,
            kind.dedup_window().as_secs() as i64,
            commit,
        )
        .await?;
        // The pool is 16 connections for the whole process; nothing below is a
        // database call and the nudge spawns network work.
        drop(conn);

        // Called on BOTH branches, so the spawn and the semaphore acquisition are
        // paid identically whether or not anything was inserted.
        self.nudge();
        Ok(id)
    }

    /// Kick a drain without waiting for the next tick.
    ///
    /// The detached task first tries to take a drain slot and returns immediately
    /// if it cannot: another drain is already running and the `SKIP LOCKED` claim
    /// will pick the row up anyway. That is what bounds spawn under a burst
    /// without introducing a queue.
    pub fn nudge(&self) {
        let me = self.clone();
        tokio::spawn(async move {
            let Ok(permit) = me.drain_slots.clone().try_acquire_owned() else {
                return;
            };
            let _permit = permit;
            me.drain_once().await;
        });
    }
}
```

- [ ] **Step 6: Write the drain.**
  Append to `backend/bins/sauron-api/src/mail.rs`, still before the `#[cfg(test)]` block:

```rust
impl MailSender {
    /// Claim and send until the queue is empty or the budget runs out. Returns
    /// how many messages left the process.
    pub async fn drain_once(&self) -> usize {
        let started = Instant::now();
        let mut total = 0usize;

        loop {
            let mut conn = match sauron_db::conn(&self.pool).await {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "mail drain: no database connection");
                    return total;
                }
            };
            if let Err(e) = repo::requeue_stuck_mail(&mut conn, stale_secs(&self.params)).await {
                report_db_error("requeue_stuck_mail", &e);
            }
            let claimed = match repo::claim_due_mail(&mut conn, BATCH).await {
                Ok(rows) => rows,
                Err(e) => {
                    report_db_error("claim_due_mail", &e);
                    drop(conn);
                    return total;
                }
            };
            // Never hold a pooled connection across network I/O. The pool is 16
            // for the whole process, and this is the documented reason
            // `AlertEngine::fire` takes a pool rather than a connection.
            drop(conn);

            let claimed_len = claimed.len();
            if claimed_len == 0 {
                return total;
            }

            // One transport for the whole batch. Rebuilding it per message costs a
            // DNS lookup, a TCP connect, a TLS handshake and an AUTH round trip
            // each time, which every hosted relay and postfix's
            // `smtpd_client_connection_rate_limit` will throttle at digest volume.
            let client = match SmtpClient::connect(&self.params).await {
                Ok(c) => Arc::new(c),
                Err(e) => {
                    self.fail_batch(&claimed, &e).await;
                    return total;
                }
            };

            let sem = Arc::new(Semaphore::new(SEND_CONCURRENCY));
            let mut set = tokio::task::JoinSet::new();
            for row in claimed {
                let me = self.clone();
                let client = client.clone();
                let sem = sem.clone();
                set.spawn(async move {
                    let Ok(_permit) = sem.acquire_owned().await else {
                        return false;
                    };
                    me.send_one(&client, row).await
                });
            }
            while let Some(res) = set.join_next().await {
                if let Ok(true) = res {
                    total += 1;
                }
            }

            if claimed_len < BATCH as usize || started.elapsed() >= DRAIN_BUDGET {
                return total;
            }
        }
    }

    /// Mark every row of a batch whose transport never came up.
    async fn fail_batch(&self, rows: &[sauron_db::models::MailOutbox], e: &MailError) {
        let permanent = !is_retryable(e);
        let text = e.to_string();
        warn!(rows = rows.len(), error = %text, permanent, "mail drain: relay unavailable");
        let mut conn = match sauron_db::conn(&self.pool).await {
            Ok(c) => c,
            Err(err) => {
                warn!(error = %err, "mail drain: cannot record batch failure");
                return;
            }
        };
        for row in rows {
            if let Err(err) =
                repo::mark_mail_failed(&mut conn, row.id, row.attempts, &text, permanent).await
            {
                report_db_error("mark_mail_failed", &err);
            }
        }
        drop(conn);
    }

    /// Send one claimed row and record the outcome. `true` means it left the
    /// process (including into the sink).
    async fn send_one(&self, client: &SmtpClient, row: sauron_db::models::MailOutbox) -> bool {
        // Heartbeat immediately before the send, so the stale-row reaper cannot
        // reclaim a row this task is about to spend a whole deadline on. Doing it
        // per row rather than per batch is what makes the threshold independent of
        // BATCH and SEND_CONCURRENCY.
        match sauron_db::conn(&self.pool).await {
            Ok(mut c) => {
                if let Err(e) = repo::heartbeat_mail(&mut c, row.id).await {
                    report_db_error("heartbeat_mail", &e);
                }
                drop(c);
            }
            Err(e) => warn!(error = %e, "mail drain: heartbeat checkout failed"),
        }

        let sink = self.params.sink;
        if sink {
            // `sauron-mail` logs recipient and subject; the outbox id and kind
            // live only here, and an operator reading a sink line needs the id to
            // find the row.
            warn!(mail_id = %row.id, kind = %row.kind, "SMTP_SINK=1: message NOT transmitted");
        }

        let mail = OutgoingMail {
            from_address: self.from_address.to_string(),
            from_name: Some(self.from_name.to_string()),
            to: vec![row.recipient.clone()],
            reply_to: None,
            subject: row.subject.clone(),
            body: MailBody::Alternative {
                text: row.body_text.clone(),
                html: row.body_html.clone(),
            },
        };

        let outcome = client.send(&mail).await;

        let mut conn = match sauron_db::conn(&self.pool).await {
            Ok(c) => c,
            Err(e) => {
                warn!(mail_id = %row.id, error = %e, "mail drain: cannot record outcome");
                return outcome.is_ok();
            }
        };
        let sent = match outcome {
            Ok(()) => {
                match repo::mark_mail_sent(&mut conn, row.id, row.attempts, sink).await {
                    // A lost claim: another drainer reclaimed this row underneath
                    // us. Delivery is at-least-once by design, so this is not a
                    // fault — but it is the signal that the stale threshold is too
                    // tight, and it must be visible.
                    Ok(0) => warn!(mail_id = %row.id, "mail drain: claim lost before mark_sent"),
                    Ok(_) => {}
                    Err(e) => report_db_error("mark_mail_sent", &e),
                }
                true
            }
            Err(e) => {
                let permanent = !is_retryable(&e);
                // The recipient is logged only on failure, never on success, so an
                // address — which is PII — stays out of the steady-state log while
                // an operator can still answer "why did this bounce".
                warn!(
                    mail_id = %row.id,
                    kind = %row.kind,
                    recipient = %row.recipient,
                    error = %e,
                    permanent,
                    "mail delivery failed"
                );
                match repo::mark_mail_failed(&mut conn, row.id, row.attempts, &e.to_string(), permanent)
                    .await
                {
                    Ok(0) => warn!(mail_id = %row.id, "mail drain: claim lost before mark_failed"),
                    Ok(_) => {}
                    Err(err) => report_db_error("mark_mail_failed", &err),
                }
                false
            }
        };
        drop(conn);
        sent
    }
}
```

- [ ] **Step 7: Write the hygiene sweep.**
  Append to `backend/bins/sauron-api/src/mail.rs`, still before the `#[cfg(test)]` block:

```rust
/// Expire, scrub and prune the outbox.
///
/// A FREE FUNCTION taking a pool, not a `MailSender` method, because this must
/// run on a deployment with no relay configured at all — where no `MailSender`
/// exists. Gating it on SMTP being switched on inverts the control it implements:
/// an operator who enables SMTP, sends reset mail, then unsets `SMTP_HOST`
/// — rotating relays, cutting cost, or responding to an incident — would
/// otherwise leave every pending row, each holding a working reset URL, in
/// Postgres permanently, backed up and replicated, with no code path that will
/// ever touch it again. This is pure SQL and needs no relay.
pub async fn hygiene(pool: &PgPool, retention_days: i64) -> anyhow::Result<()> {
    let mut conn = sauron_db::conn(pool).await?;

    let expired = repo::expire_stale_mail(&mut conn).await?;
    let blanked = repo::blank_expired_mail_bodies(&mut conn).await?;

    let mut pruned = 0usize;
    loop {
        let n = repo::prune_mail_outbox(&mut conn, retention_days, PRUNE_BATCH).await?;
        pruned += n;
        if n == 0 {
            break;
        }
    }

    let (pending, oldest_secs) = repo::mail_outbox_depth(&mut conn).await?;
    drop(conn);

    if expired > 0 || blanked > 0 || pruned > 0 {
        info!(expired, blanked, pruned, "mail outbox hygiene");
    }
    // Unconditional. There is no metrics endpoint and no admin view, so without
    // this line a stalled queue is invisible until a user reports that password
    // reset does not work.
    info!(
        pending,
        oldest_pending_secs = oldest_secs.unwrap_or(0),
        "mail outbox depth"
    );
    Ok(())
}
```

- [ ] **Step 8: Run the tests and watch them pass.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api mail`
  Expected: `test result: ok. 3 passed`.

- [ ] **Step 9: Gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0. `MailSender::enqueue` and `enqueue_or_discard` have no caller until S1; if clippy blocks on `dead_code`, add `#[allow(dead_code)]` on those two methods with the comment `// First caller lands in S1 (password reset).` and leave it — S1 removes it.

---

## Task 13: Wire it into `sauron-api` — `AppState.mail`, `/health`, two supervised tasks

**Files:**
- Modify `backend/bins/sauron-api/src/main.rs` (module list lines 7-11; `AppState` lines 43-55; `main()` lines 63-120; the `/health` route at line 147)
- Create `backend/bins/sauron-api/tests/http_mail_outbox.rs`

**Interfaces:**
- Consumes: `crate::mail::{MailSender, hygiene}` (Task 12), `crate::tasks::{supervise, snapshot}` (Task 11), `Config::{require_smtp, require_dashboard_url, dev_mode, mail_drain_tick_secs, mail_outbox_retention_days}` (Task 3), `sauron_mail::{Branding, SmtpParams}` (Tasks 6-7).
- Produces:
  - `AppState.mail: Option<crate::mail::MailSender>` — purely additive, no extractor change
  - `GET /health` returning `{"status":"ok","tasks":[{"name":...,"last_success_secs":...,"consecutive_failures":...}]}` with status **always 200**

- [ ] **Step 1: Write the failing integration test.**
  Create `backend/bins/sauron-api/tests/http_mail_outbox.rs`:

```rust
//! Boot-time behaviour of the transactional-email wiring, driven against the
//! real compiled `sauron-api` binary on an ephemeral, migrated database.
//!
//! The regression these three cases exist for: bailing in `Config::from_env` on
//! a missing setting once took down `sauron-ingest` and `sauron-tier`. Every
//! configuration below must leave the API booting and serving.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;

/// Not a real secret — this process and the one it spawns are the only two
/// parties that ever see it, and both live only for this test's duration.
const JWT_SECRET: &str = "http-mail-outbox-test-secret-0000000000000";

/// See `tests/http_env_scoping.rs`'s identical helper for the full reasoning.
fn swap_database(url: &str, new_db: &str) -> String {
    let (scheme, rest) = url
        .split_once("://")
        .expect("TEST_DATABASE_URL must be scheme://...");
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let after = &rest[auth_end..];
    let query = after.find('?').map(|i| &after[i..]).unwrap_or("");
    format!("{scheme}://{authority}/{new_db}{query}")
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
}

fn ephemeral_db_name() -> String {
    format!(
        "sauron_mailtest_{}",
        uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
    )
}

/// Boot the binary with `extra_env` on top of the minimum, poll `/health` until
/// it answers, return the parsed body, then tear everything down.
///
/// Returns `None` when the test environment is not configured, so the caller can
/// skip rather than fail.
async fn health_body_with(extra_env: &[(&str, &str)]) -> Option<Value> {
    let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
    let redis_url = std::env::var("TEST_REDIS_URL").ok()?;

    let db_name = ephemeral_db_name();
    sauron_db::create_database(&admin_url, &db_name)
        .await
        .expect("create ephemeral test database");
    let db_url = swap_database(&admin_url, &db_name);
    sauron_db::run_pending_migrations(&db_url)
        .await
        .expect("run migrations");

    let port = free_port();
    let bin = env!("CARGO_BIN_EXE_sauron-api");
    let mut cmd = tokio::process::Command::new(bin);
    cmd.env("DATABASE_URL", &db_url)
        .env("REDIS_URL", &redis_url)
        .env("JWT_SECRET", JWT_SECRET)
        .env("API_PORT", port.to_string())
        .env("CORS_ALLOWED_ORIGINS", "http://localhost:5173")
        .env("RUST_LOG", "error")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn sauron-api binary");

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let mut body: Option<Value> = None;
    for _ in 0..100 {
        if let Ok(Some(status)) = child.try_wait() {
            let mut stderr = String::new();
            if let Some(mut s) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = s.read_to_string(&mut stderr).await;
            }
            panic!("sauron-api exited early with {status}; stderr:\n{stderr}");
        }
        if let Ok(resp) = client
            .get(format!("{base}/health"))
            .timeout(Duration::from_millis(200))
            .send()
            .await
        {
            if resp.status().is_success() {
                body = resp.json::<Value>().await.ok();
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _ = child.kill().await;
    sauron_db::drop_database(&admin_url, &db_name)
        .await
        .expect("drop ephemeral test database");

    Some(body.expect("/health never returned a successful JSON body"))
}

/// The hygiene task must run on a deployment that has never configured a relay:
/// it is the control that bounds credential-at-rest, and gating it on the feature
/// being switched on inverts it.
#[tokio::test]
async fn health_lists_hygiene_even_with_no_smtp_at_all() {
    let Some(body) = health_body_with(&[]).await else {
        eprintln!("TEST_DATABASE_URL/TEST_REDIS_URL unset — skipping");
        return;
    };
    assert_eq!(body["status"], "ok");
    let names: Vec<&str> = body["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"mail_hygiene"), "got: {names:?}");
    assert!(
        !names.contains(&"mail_drain"),
        "the drain must not mount without a relay: {names:?}"
    );
    // Never a non-2xx and never a missing field: SETUP.md documents
    // `curl -fsS .../health` and http_env_scoping.rs polls it for readiness.
    let first = &body["tasks"][0];
    assert!(first["last_success_secs"].is_null() || first["last_success_secs"].is_u64());
    assert!(first["consecutive_failures"].is_u64());
}

#[tokio::test]
async fn sink_without_a_host_boots_and_mounts_the_drain() {
    let Some(body) = health_body_with(&[("SMTP_SINK", "1")]).await else {
        eprintln!("TEST_DATABASE_URL/TEST_REDIS_URL unset — skipping");
        return;
    };
    assert_eq!(body["status"], "ok");
    let names: Vec<&str> = body["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"mail_drain"), "got: {names:?}");
    assert!(names.contains(&"mail_hygiene"), "got: {names:?}");
}

#[tokio::test]
async fn sink_with_a_from_address_boots_the_same_way() {
    let Some(body) = health_body_with(&[("SMTP_SINK", "1"), ("SMTP_FROM", "sauron@corp.test")])
        .await
    else {
        eprintln!("TEST_DATABASE_URL/TEST_REDIS_URL unset — skipping");
        return;
    };
    assert_eq!(body["status"], "ok");
    assert!(body["tasks"].as_array().expect("tasks array").len() >= 2);
}
```

  This test binary needs `reqwest`'s JSON codec and `uuid`, neither of which the crate's dev-dependencies currently enable. In `backend/bins/sauron-api/Cargo.toml`, change the `[dev-dependencies]` block to:

```toml
[dev-dependencies]
# Drives the real, compiled `sauron-api` binary over HTTP in
# `tests/http_env_scoping.rs` — the only way to exercise the actual extractor
# stack a `parse_env` unit test cannot see. The "json" feature is for
# `tests/http_mail_outbox.rs`, which asserts on `/health`'s body rather than on
# a status code.
reqwest = { workspace = true, features = ["json"] }
```

  `uuid`, `serde_json`, `tokio` and `sauron-db` are already ordinary dependencies of the crate, so integration tests can use them without a dev-dependency entry.

- [ ] **Step 2: Run it and watch it fail.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p sauron-api --test http_mail_outbox`
  Expected: all three fail with `panicked at ... /health never returned a successful JSON body` — `/health` currently returns the string `ok`, which is not JSON.

- [ ] **Step 3: Put `Config` behind an `Arc` earlier and add the state field.**
  In `backend/bins/sauron-api/src/main.rs`:
  - Change line 66 from `let cfg = Config::from_env()?;` to:

```rust
    // Behind an Arc from the start: the background tasks below capture settings
    // out of it, and the state build would otherwise move it first.
    let cfg = Arc::new(Config::from_env()?);
```

  - Change the `AppState` literal's `cfg: Arc::new(cfg),` to `cfg: cfg.clone(),`.
  - Add to `pub struct AppState`, after `pub alerts: sauron_alerts::AlertEngine,`:

```rust
    /// `None` when SMTP is unconfigured. Every caller must degrade rather than
    /// fail: the API has to boot and serve everything else on a deployment with
    /// no relay. An unauthenticated route's response must be identical either
    /// way — a response that distinguishes configured from unconfigured is a
    /// config oracle handed to anyone on the internet.
    pub mail: Option<crate::mail::MailSender>,
```

- [ ] **Step 4: Build the sender and mount the two tasks.**
  In `main()`, after the `let alerts = sauron_alerts::AlertEngine::new(...)` block and **before** `let state = AppState { ... }`, insert:

```rust
    // The pool is moved into the state below; the hygiene task needs its own
    // handle.
    let hygiene_pool = pool.clone();

    let branding = sauron_mail::Branding {
        product_name: "Sauron".to_string(),
        // `.ok()` on purpose: an unset DASHBOARD_URL disables link-bearing mail
        // at render time with a message naming the variable, rather than
        // preventing the process from booting.
        dashboard_url: cfg.require_dashboard_url().ok().map(|s| s.to_string()),
        footer: MAIL_FOOTER.to_string(),
    };

    let mail = match cfg.require_smtp() {
        Err(e) => {
            // One INFO line, not a warning and not a failure. This is the ordinary
            // state of a deployment that has not enabled transactional email.
            info!(reason = %e, "transactional email disabled");
            None
        }
        Ok(s) => {
            let mut params = sauron_mail::SmtpParams::from_settings(s);
            // Two explicit variables, because logs are routinely shipped to an
            // aggregator with a broader reader set and a longer retention than the
            // database. RUST_LOG is no gate: the shipped default is
            // `info,sauron=debug` and EnvFilter matches targets by prefix.
            params.sink_log_body = s.sink && cfg.dev_mode;
            if s.sink {
                tracing::warn!(
                    log_bodies = params.sink_log_body,
                    "SMTP_SINK=1: transactional email is written to the log and NEVER \
                     transmitted; rows are recorded as status='sink'"
                );
            }
            Some(mail::MailSender::new(
                pool.clone(),
                params,
                s.from_address.clone(),
                s.from_name.clone(),
                branding,
            ))
        }
    };

    // The drain only exists where a relay does.
    if let Some(sender) = mail.clone() {
        let tick = Duration::from_secs(cfg.mail_drain_tick_secs);
        tasks::supervise("mail_drain", tick, move || {
            let s = sender.clone();
            async move {
                s.drain_once().await;
                Ok(())
            }
        });
    }

    // UNCONDITIONAL, and that is the whole point of splitting it out. An operator
    // who enables SMTP, sends reset mail, then unsets SMTP_HOST — rotating
    // relays, cutting cost, or responding to an incident — would otherwise leave
    // every pending row, each holding a working reset URL, in Postgres
    // permanently, backed up and replicated, with no code path that will ever
    // touch it again.
    let retention_days = cfg.mail_outbox_retention_days;
    tasks::supervise("mail_hygiene", MAIL_HYGIENE_INTERVAL, move || {
        let p = hygiene_pool.clone();
        async move { mail::hygiene(&p, retention_days).await }
    });
```

  and add `mail,` to the `AppState { ... }` literal.

- [ ] **Step 5: Add the two constants and the health handler.**
  In `backend/bins/sauron-api/src/main.rs`, after `const MAX_INFLIGHT_REQUESTS: usize = 512;`, add:

```rust
/// How often the outbox is expired, scrubbed and pruned. A compile-time constant
/// rather than a variable: three files of documentation for a number nobody tunes
/// is how a config surface becomes unmaintainable.
const MAIL_HYGIENE_INTERVAL: Duration = Duration::from_secs(900);
/// Footer line on every product email. Deliberately says nothing about why the
/// recipient is receiving it — each sender's own footnotes do that.
const MAIL_FOOTER: &str = "Sent by Sauron. This mailbox is not monitored.";
```

  Replace the `/health` route at line 147:

```rust
        .route("/health", get(|| async { "ok" }))
```

  with:

```rust
        .route("/health", get(health))
```

  and add, after `main()`:

```rust
/// ALWAYS 200. `packaging/rpm/SETUP.md` documents `curl -fsS .../health` and
/// `tests/http_env_scoping.rs` polls it for readiness; both read a non-2xx as
/// "the API is down", which a stalled reaper is not. The task list is the signal;
/// the status code is not.
async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "tasks": tasks::snapshot(),
    }))
}
```

- [ ] **Step 6: Run the integration test and watch it pass.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p sauron-api --test http_mail_outbox`
  Expected: `test result: ok. 3 passed`.

- [ ] **Step 7: Confirm the existing readiness polls still work.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p sauron-api`
  Expected: `http_env_scoping` and `http_workflows` both still `ok` — they only assert `resp.status().is_success()` on `/health`, which a JSON 200 satisfies.

- [ ] **Step 8: Remove any temporary `#![allow(dead_code)]`.**
  If Task 11 Step 5 or Task 12 Step 9 added an allow attribute to get past clippy, delete the ones that are now unnecessary (`supervise` and `snapshot` have callers as of this task; `MailSender::enqueue`/`enqueue_or_discard` still do not, and keep theirs).

- [ ] **Step 9: Gate.**
  `cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 14: Environment documentation, compose passthrough, README, and the config-key ratchet

**Files:**
- Create `backend/crates/sauron-core/tests/config_keys_documented.rs`
- Modify `.env.example` (repo root)
- Modify `docker-compose.yml` (the `api:` service `environment:` block, lines 124-136)
- Modify `README.md` (a `DASHBOARD_URL` row in `### Dashboard API` at ~line 172-176; a new `### Transactional email` section after the alerting note at line 210)

**Interfaces:**
- Consumes: the `var("KEY")` / `parse("KEY"` literals in `backend/crates/sauron-core/src/config.rs` (Tasks 2-3).
- Produces: `backend/crates/sauron-core/tests/config_keys_documented.rs`, which runs under the `cargo test --workspace` CI already performs.

- [ ] **Step 1: Write the failing assertion.**
  Create `backend/crates/sauron-core/tests/config_keys_documented.rs`:

```rust
//! Every environment key `config.rs` reads must be documented in `.env.example`.
//!
//! Thirteen new variables land in one slice and roughly thirty across the
//! programme, each needing a row in `.env.example`, `docker-compose.yml`, the
//! relevant `packaging/rpm/config/*.env` and the README table. Nothing enforced
//! any of that. This is the cheapest of the four to enforce and the one the other
//! three are usually copied from.
//!
//! A Rust test rather than a shell step in `ci.yml`: CI already runs
//! `cargo test --workspace`, so this needs no workflow change, and an engineer
//! can reproduce a failure with the same command they already use.

use std::collections::BTreeSet;

/// Keys deliberately absent from `.env.example`, each with the reason.
const EXEMPT: &[(&str, &str)] = &[
    (
        "DATABASE_URL",
        "composed by docker-compose from POSTGRES_USER/PASSWORD/DB, and set \
         per-service in packaging/rpm/config/sauron.env",
    ),
    (
        "REDIS_URL",
        "pinned to the compose service name; not an operator-facing knob there",
    ),
    (
        "SAURON_DEV",
        "a local-development escape hatch that makes tokens forgeable; documenting \
         it in a file operators copy is an invitation",
    ),
];

fn keys_read_by_config() -> BTreeSet<String> {
    let src = include_str!("../src/config.rs");
    let mut out = BTreeSet::new();
    for needle in ["var(\"", "parse(\""] {
        let mut rest = src;
        while let Some(i) = rest.find(needle) {
            let after = &rest[i + needle.len()..];
            match after.find('"') {
                Some(end) => {
                    out.insert(after[..end].to_string());
                    rest = &after[end..];
                }
                None => break,
            }
        }
    }
    out
}

#[test]
fn every_config_key_appears_in_env_example() {
    let example = include_str!("../../../../.env.example");
    let exempt: BTreeSet<&str> = EXEMPT.iter().map(|(k, _)| *k).collect();

    let mut missing: Vec<String> = Vec::new();
    for key in keys_read_by_config() {
        if exempt.contains(key.as_str()) {
            continue;
        }
        // Matched with the `=` (or as a commented `# KEY=`) so `SMTP_HOST` in a
        // prose sentence does not count as documentation.
        let documented = example
            .lines()
            .any(|l| l.trim_start().trim_start_matches("# ").starts_with(&format!("{key}=")));
        if !documented {
            missing.push(key);
        }
    }

    assert!(
        missing.is_empty(),
        "these config keys are read by config.rs but not documented in .env.example: {missing:?}\n\
         Add a line for each (a commented `# KEY=` counts), or add it to EXEMPT with a reason."
    );
}

#[test]
fn exemptions_are_still_read_by_config() {
    // An exemption for a key nobody reads any more is dead weight that makes the
    // list look longer and more negotiable than it is.
    let keys = keys_read_by_config();
    for (key, _) in EXEMPT {
        assert!(
            keys.contains(*key),
            "{key} is exempted but config.rs no longer reads it"
        );
    }
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-core --test config_keys_documented`
  Expected: `every_config_key_appears_in_env_example` fails listing roughly 29 keys — the 13 new ones plus the 16 that were already undocumented before this slice.

- [ ] **Step 3: Add the new transactional-email block to `.env.example`.**
  After the `# --- alerting (sauron-alerts) ---` block (which ends at `ALERTS_ALLOW_PRIVATE=false`, line 51) and before `# --- reverse proxy ---`, insert:

```
# --- transactional email (sauron-api) ---
# DEPLOYMENT-LEVEL mail: password resets and, later, digests. This is NOT the
# per-org notification channels an admin configures in the UI — those carry an
# org's own SMTP credentials and cannot reach a user who belongs to no org, or
# reach one without telling that org's admin.
#
# Leaving SMTP_HOST unset DISABLES password reset. It does not break the API:
# everything else boots and serves normally, and one INFO line at startup says
# why. Only sauron-api reads any of this; sauron-alerts never drains the queue.
# SMTP_HOST=smtp.example.com
SMTP_PORT=587
# SMTP_USERNAME=
# SMTP_PASSWORD=
# Required once SMTP_HOST is set. A bare address, one '@', no display name.
# SMTP_FROM=sauron@example.com
SMTP_FROM_NAME=Sauron
# implicit (or smtps) | starttls (or required) | none (or plain).
# Unset follows the port: implicit at 465, starttls everywhere else.
# `none` sends the SMTP password and password-reset links in CLEARTEXT and is
# accepted ONLY when the relay resolves to loopback — checked at boot against the
# configured name and again at connect against the resolved address.
# SMTP_TLS=starttls
# A relay on a LAN is blocked by the SSRF guard unless this is true. It is read
# on its own and does NOT inherit ALERTS_ALLOW_PRIVATE: that flag unlocks private
# delivery for user-supplied webhook URLs, a strictly larger surface.
SMTP_ALLOW_PRIVATE=false
# Per socket operation. The whole send, DNS included, is bounded at 3x this,
# capped at 60s.
SMTP_TIMEOUT_MS=10000
# Write mail to the log instead of sending it. Read on its own; it does NOT
# inherit SAURON_DEV. The BODY is logged only when SAURON_DEV=1 as well, because
# a logged body is a working account-takeover URL in your log aggregator.
SMTP_SINK=false
# How often sauron-api drains the outbox (clamped to 10..3600).
MAIL_DRAIN_TICK_SECS=60
# How long delivered/failed outbox rows are kept.
MAIL_OUTBOX_RETENTION_DAYS=30
```

- [ ] **Step 4: Add `DASHBOARD_URL` to the CORS / URLs block.**
  In `.env.example`, in the `# --- CORS / URLs ---` block, after the `INGEST_BASE_URL=...` line, insert:

```
# Browser-facing origin of the DASHBOARD, used to build links inside emails.
# In the shipped nginx topology this is NOT the API's origin — nginx serves the
# SPA and does not proxy the API — so nothing can derive it.
#
# Deliberately has no default anywhere. A plausible-looking fallback would let
# every server-side signal report success while the recipient's browser hits
# their own machine.
# DASHBOARD_URL=http://localhost:10002
```

- [ ] **Step 5: Document the sixteen keys that were already missing.**
  These predate this slice; the assertion cannot be switched on with them outstanding, and a sixteen-entry exemption list is a ratchet nobody would respect. Add each under the block it belongs to.

  In `# --- Auth ---`, after the `JWT_SECRET=` line:

```
# Token lifetimes (seconds).
JWT_ACCESS_TTL_SECS=900
JWT_REFRESH_TTL_SECS=2592000
```

  In `# --- CORS / URLs ---`, at the end of the block:

```
# Ports the services bind INSIDE their containers. The host-published ports above
# are remapped to these by docker-compose.yml's `ports:` entries.
API_PORT=8080
INGEST_PORT=8081
```

  A new block immediately after `# --- CORS / URLs ---`:

```
# --- ingest tuning (sauron-ingest) ---
# Envelopes accepted per app per minute.
INGEST_RATE_LIMIT_PER_MIN=6000
# Largest accepted envelope body, in bytes.
INGEST_MAX_BODY_BYTES=1048576
# Listen on a Unix socket instead of TCP. Unset means TCP.
# INGEST_UDS_PATH=
# TCP listen() backlog. Ignored when INGEST_UDS_PATH is set.
INGEST_BACKLOG=4096
# Co-located pipeline workers draining the Redis stream.
WORKER_CONCURRENCY=4
```

  In `# --- alerting (sauron-alerts) ---`, after `ALERTS_ALLOW_PRIVATE=false`:

```
# alert_events records EVERY evaluation, including suppressed ones, so it needs
# a reaper.
ALERT_EVENT_RETENTION_DAYS=90
```

  A new block after the alerting block (before the new transactional-email block):

```
# --- symbolication / source maps (sauron-api, sauron-ingest) ---
# In-process parsed-index cache budget.
SYMBOLS_CACHE_MB=256
# Warm-blob Redis for symbol artifacts. Point it at a SEPARATE INSTANCE, not just
# a different DB index: maxmemory is instance-wide, so symbol blobs would evict
# ingest stream state.
# SYMBOLS_REDIS_URL=
# Blobs larger than this are never cached in Redis.
SYMBOLS_REDIS_MAX_BLOB_MB=8
# Reject uploads whose raw file exceeds this.
SYMBOLS_MAX_ARTIFACT_MB=128
# Decompression-bomb guard: cap on a blob's uncompressed size.
SYMBOLS_MAX_UNCOMPRESSED_MB=512
# Ingest-path symbolication time box; on timeout the raw frame is stored pending.
SYMBOLS_INGEST_TIMEOUT_MS=150
```

- [ ] **Step 6: Run the assertion and watch it pass.**
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-core --test config_keys_documented`
  Expected: `test result: ok. 2 passed`. If a key is still reported, its `.env.example` line does not begin with `KEY=` or `# KEY=` after trimming — the matcher is deliberately strict so a key mentioned only in prose does not count.

- [ ] **Step 7: Add the compose passthrough.**
  In `docker-compose.yml`, in the `api:` service's `environment:` block, after the `API_TRUST_FORWARDED_HEADERS:` entry and before `TIER_COLD_PATH:`, insert:

```yaml
      # Browser-facing origin of the dashboard, used to build links inside emails.
      # DELIBERATELY NO FALLBACK. Mirroring CORS_ALLOWED_ORIGINS' `:-http://localhost:10002`
      # is the obvious move and is exactly the behaviour this design rejects: the
      # URL would render, the message would send, the row would reach 'sent' —
      # every server-side signal reporting success while the recipient's browser
      # hits their own machine. Unset must produce the loud error naming the variable.
      DASHBOARD_URL: ${DASHBOARD_URL:-}
      # Deployment-level SMTP relay. Unset disables password reset; it does not
      # break the API.
      SMTP_HOST: ${SMTP_HOST:-}
      SMTP_PORT: ${SMTP_PORT:-587}
      SMTP_USERNAME: ${SMTP_USERNAME:-}
      SMTP_PASSWORD: ${SMTP_PASSWORD:-}
      SMTP_FROM: ${SMTP_FROM:-}
      SMTP_FROM_NAME: ${SMTP_FROM_NAME:-Sauron}
      SMTP_TLS: ${SMTP_TLS:-}
      SMTP_ALLOW_PRIVATE: ${SMTP_ALLOW_PRIVATE:-false}
      SMTP_TIMEOUT_MS: ${SMTP_TIMEOUT_MS:-10000}
      SMTP_SINK: ${SMTP_SINK:-false}
      MAIL_DRAIN_TICK_SECS: ${MAIL_DRAIN_TICK_SECS:-60}
      MAIL_OUTBOX_RETENTION_DAYS: ${MAIL_OUTBOX_RETENTION_DAYS:-30}
```

- [ ] **Step 8: Validate the compose file.**
  `cd /home/splimter/projects/freelance/sauron && docker compose config --quiet`
  Expected: exit 0 and no output. (If `docker` is unavailable on this machine, `python3 -c "import yaml,sys; yaml.safe_load(open('docker-compose.yml'))"` is an acceptable substitute — it catches the indentation error this edit can plausibly introduce.)

- [ ] **Step 9: Add the README rows.**
  In `README.md`, in the `### Dashboard API` table, after the `CORS_ALLOWED_ORIGINS` row, add:

```
| `DASHBOARD_URL` | Browser-facing origin of the **dashboard**, used to build links inside emails (`https://host/#/reset-password?token=...`). In the shipped nginx topology this is **not** the API's origin — nginx serves the SPA and does not proxy the API — so nothing can derive it. Unset means any email containing a link refuses to render, with an error naming this variable; it does not break anything else. **No default anywhere**, deliberately: a plausible-looking fallback would send mail whose links point at the recipient's own machine while every server-side signal reported success. | unset | api |
```

  And after the `### Alerting & notifications` section's closing note (the `> Monitor up/down alerts fire inline...` blockquote at lines 209-210) and before `### Hot/cold tiering`, add:

```
### Transactional email

Deployment-level mail addressed to a **person** — password resets today, digests
later. Separate from the notification channels above: those carry an org's own
SMTP credentials, so routing a user's reset link through one would tell that org's
admin the user asked for a reset, and would strand a user who belongs to no org
entirely. Leaving `SMTP_HOST` unset **disables password reset** and degrades
nothing else — the API boots and serves normally, and logs one INFO line saying
why. Only `sauron-api` reads these; it is also the only process that drains the
queue.

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `SMTP_HOST` | Relay hostname. Unset ⇒ transactional email is disabled. | unset | api |
| `SMTP_PORT` | Relay port. Also picks the default TLS mode. | `587` | api |
| `SMTP_USERNAME` / `SMTP_PASSWORD` | AUTH credentials. On an RPM install the password belongs in `/etc/sauron/secret.env`, not `api.env`. | unset | api |
| `SMTP_FROM` | Envelope From. **Required** once `SMTP_HOST` is set; a bare address, exactly one `@`, no display name. | unset | api |
| `SMTP_FROM_NAME` | Display name lettre encodes into the From header. | `Sauron` | api |
| `SMTP_TLS` | `implicit`/`smtps`, `starttls`/`required`, or `none`/`plain`. Unset follows the port. `none` sends the password and every reset link in cleartext and is accepted **only** when the relay resolves to loopback — checked at boot against the configured name and again at connect against the resolved address. | `implicit` at port 465, else `starttls` | api |
| `SMTP_ALLOW_PRIVATE` | Allow a relay on a private/LAN address past the SSRF guard. Read on its own; it does **not** inherit `ALERTS_ALLOW_PRIVATE`, which unlocks private delivery for *user-supplied* webhook URLs — a strictly larger surface. | `false` | api |
| `SMTP_TIMEOUT_MS` | Per socket operation. The whole send, DNS included, is bounded at 3× this and capped at 60s. Clamped to `1000`–`60000`. | `10000` | api |
| `SMTP_SINK` | Write mail to the log instead of sending it. Rows are recorded `status='sink'`, never `'sent'`. Read on its own; it does **not** inherit `SAURON_DEV`. The **body** is logged only when `SAURON_DEV=1` as well — a logged body is a working account-takeover URL in your log aggregator. | `false` | api |
| `MAIL_DRAIN_TICK_SECS` | Outbox drain cadence. Clamped to `10`–`3600`. | `60` | api |
| `MAIL_OUTBOX_RETENTION_DAYS` | How long delivered/failed outbox rows are kept before the reaper deletes them. | `30` | api |

> Emails containing a link also need [`DASHBOARD_URL`](#dashboard-api). Without it
> the message refuses to render rather than sending a link to nowhere.
```

- [ ] **Step 10: Gate.**
  `cargo fmt --all --check` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: both exit 0.

---

## Task 15: RPM packaging — `api.env`, SETUP.md §11 "Upgrading", `%changelog`

**Files:**
- Modify `packaging/rpm/config/api.env` (append a commented transactional-email block)
- Modify `packaging/rpm/SETUP.md` (§4 secret.env gains `SMTP_PASSWORD`; a new §11 after §10 Troubleshooting)
- Modify `packaging/rpm/sauron.spec` (bump `Release:` on line 12; a `%changelog` entry at the top of the changelog, ~line 260)

**Interfaces:**
- Consumes: the variable names from Task 14.
- Produces: `packaging/rpm/SETUP.md` §11 "Upgrading", a shared foundation every later slice in this programme appends a row to.

**No binary is added**, so `packaging/rpm/binaries.txt`, the spec's `%install` loop, `%files`, `%post`, `%preun`, `%postun`, `packaging/rpm/systemd/` and `build-rpm.sh` are **all untouched**. That is the main packaging benefit of putting the drain inside `sauron-api` rather than in a new worker. `sauron-api.service` already loads `/etc/sauron/secret.env` non-optionally, so no unit file changes either.

- [ ] **Step 1: Extend `packaging/rpm/config/api.env`.**
  Append to `packaging/rpm/config/api.env`:

```
# --- Transactional email (password resets) -------------------------------
# Deployment-level mail addressed to a PERSON. Separate from the per-org
# notification channels configured in the UI: routing a user's reset link
# through an org's SMTP channel would tell that org's admin the user asked for a
# reset, and would strand a user who belongs to no org.
#
# Leaving SMTP_HOST unset DISABLES password reset. It breaks nothing else: the
# API boots and serves normally, and logs one INFO line saying why.
# SMTP_HOST=smtp.example.com
# SMTP_PORT=587
# SMTP_USERNAME=sauron@example.com
#
# SMTP_PASSWORD belongs in /etc/sauron/secret.env, not here. Both files are
# 0640 root:sauron, so this is not about who can read them: secret.env is
# generated by the package at first install rather than shipped in the RPM
# payload, so an upgrade never rewrites it and never leaves a .rpmnew beside it;
# it is already where JWT_SECRET lives; and sauron-api.service already loads it.
# One file holds every credential an operator has to protect and back up.
#
# Required once SMTP_HOST is set. A bare address, exactly one '@', no display name.
# SMTP_FROM=sauron@example.com
# SMTP_FROM_NAME=Sauron
#
# implicit (or smtps) | starttls (or required) | none (or plain).
# Unset follows SMTP_PORT: implicit at 465, starttls everywhere else.
# `none` puts the SMTP password AND every password-reset link on the wire in
# cleartext. It is accepted only when the relay resolves to loopback — checked
# once at boot against the name you wrote and again at connect against what it
# actually resolved to.
# SMTP_TLS=starttls
#
# A relay on a private/LAN address is blocked by the SSRF guard unless this is
# true. It is read on its own and does NOT inherit ALERTS_ALLOW_PRIVATE: that
# flag unlocks private delivery for user-supplied webhook URLs, which is a
# strictly larger surface than declaring where your own relay lives.
# SMTP_ALLOW_PRIVATE=false
#
# Per socket operation. The whole send, DNS included, is bounded at 3x this and
# capped at 60s, so a tarpitting relay cannot hold a drain slot indefinitely.
# SMTP_TIMEOUT_MS=10000
#
# Write mail to the journal instead of sending it. Rows are recorded
# status='sink', never 'sent'. Read on its own; it does NOT inherit SAURON_DEV.
# The BODY is written only when SAURON_DEV=1 as well, because a logged body is a
# working account-takeover URL sitting in your log aggregator.
# SMTP_SINK=false
#
# Outbox drain cadence (clamped 10..3600) and retention for delivered/failed rows.
# MAIL_DRAIN_TICK_SECS=60
# MAIL_OUTBOX_RETENTION_DAYS=30
#
# Browser-facing origin of the DASHBOARD — where a link inside an email points.
# In this packaging nginx serves the SPA and does NOT proxy the API, so this is
# not the API's origin and nothing can derive it. Left commented rather than
# given a working-looking localhost value: a wrong-but-plausible value renders,
# sends, and reaches status='sent' while the recipient's browser hits their own
# machine.
# DASHBOARD_URL=https://sauron.example.com
```

- [ ] **Step 2: Add `SMTP_PASSWORD` to the secret.env section of SETUP.md.**
  In `packaging/rpm/SETUP.md` §4 "JWT secret", after the rotation code block, append:

```markdown
`/etc/sauron/secret.env` is the file for **every** credential the services read
from the environment, not just `JWT_SECRET`. If you enable transactional email,
put the relay password there rather than in `/etc/sauron/api.env`. Both files are
`0640 root:sauron`, so this is not a permissions difference: `secret.env` is
generated at first install instead of being shipped in the package, so an upgrade
never rewrites it and never leaves an `.rpmnew` beside it, and it is the one file
you already back up and protect:

```bash
sudo sh -c 'umask 077; printf "SMTP_PASSWORD=%s\n" "your-relay-password" >> /etc/sauron/secret.env'
sudo chgrp sauron /etc/sauron/secret.env && sudo chmod 0640 /etc/sauron/secret.env
sudo systemctl restart sauron-api
```

Everything else about the relay — host, port, username, TLS mode — goes in
`/etc/sauron/api.env`, which ships with the whole block commented out. See
section 6 for enabling it.
```

- [ ] **Step 3: Add the "enabling transactional email" note to §6.**
  In `packaging/rpm/SETUP.md`, at the end of §6 "Enable and start the services", append:

```markdown
### Enabling transactional email (optional)

Password reset needs a relay and a link base. Without them `sauron-api` boots and
serves normally and logs one INFO line explaining what is missing, so this can be
done later.

1. Uncomment and set `SMTP_HOST`, `SMTP_PORT`, `SMTP_FROM` and `SMTP_FROM_NAME`
   in `/etc/sauron/api.env`.
2. Put `SMTP_PASSWORD` in `/etc/sauron/secret.env` (section 4), not in `api.env`.
3. Set `DASHBOARD_URL` in `/etc/sauron/api.env` to the origin a **browser** uses
   to reach the dashboard — the nginx vhost, not `http://localhost:8080`. Every
   link in every email is built from it, and it has no default: an email whose
   links point at the recipient's own machine reports success everywhere on the
   server side.
4. `sudo systemctl restart sauron-api`, then confirm with
   `journalctl -u sauron-api -e | grep -i "transactional email"` that it is **not**
   reporting the feature disabled.
```

- [ ] **Step 4: Create §11 "Upgrading".**
  Append to `packaging/rpm/SETUP.md`:

```markdown
## 11. Upgrading

**Run the migrator by hand after every upgrade.** `dnf upgrade` does not do it:
`sauron-migrate.service` has no `[Install]` section and `%post` never starts it,
so a new binary meets whatever schema was there before. The symptom is not a
crash — it is scattered 500s, or a feature that silently does nothing.

```bash
sudo systemctl stop sauron-api sauron-ingest
sudo systemctl start sauron-migrate
sudo systemctl start sauron-api sauron-ingest
```

Stop first: `sauron-api` and `sauron-ingest` must not be serving against a schema
that is halfway through changing.

Then diff the shipped config against yours. `/etc/sauron/*.env` are
`%config(noreplace)`, so a release that adds new settings leaves them in
`api.env.rpmnew` and your actual file never sees them:

```bash
ls /etc/sauron/*.rpmnew 2>/dev/null && diff -u /etc/sauron/api.env /etc/sauron/api.env.rpmnew
```

### What breaks if a migration is skipped

| Migration | Skipping it means |
|---|---|
| `2026-08-01-000034_mail_outbox` | `sauron-api` queries a `mail_outbox` relation that does not exist. Password reset silently does nothing, because the enqueue error is swallowed behind a fixed 200. The drain logs one ERROR naming this section and then stays quiet. |
```

- [ ] **Step 5: Bump `Release` so the build is actually installable.**
  In `packaging/rpm/sauron.spec` line 12, change:

```
Release:        1%{?dist}
```

  to:

```
Release:        2%{?dist}
```

  `Version` stays `1.1.0` — this slice adds no new binaries and no new units, so
  it is a packaging revision of the same upstream version. Without the bump the
  new package has the same NEVR as the shipped `1.1.0-1`, `dnf upgrade` refuses
  to install it, and the "RUN sauron-migrate AFTER UPGRADING" instruction that
  Step 6 adds never reaches the operator it exists for.

- [ ] **Step 6: Add the `%changelog` entry.**
  In `packaging/rpm/sauron.spec`, immediately after the `%changelog` line and before the existing `* Thu Jul 30 2026` entry, insert:

```
* Sat Aug 01 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.1.0-2
- Transactional email foundation: a deployment-level SMTP relay, an HTML/plain
  email template engine, and a durable outbox drained by sauron-api.
- New `mail_outbox` table; RUN sauron-migrate AFTER UPGRADING (see SETUP.md
  section 11). Without it sauron-api queries a relation that does not exist and
  transactional email silently does nothing.
- New settings in /etc/sauron/api.env (shipped commented out, so they land in
  api.env.rpmnew on upgrade): SMTP_HOST/PORT/USERNAME/FROM/FROM_NAME/TLS/
  ALLOW_PRIVATE/TIMEOUT_MS/SINK, MAIL_DRAIN_TICK_SECS, MAIL_OUTBOX_RETENTION_DAYS
  and DASHBOARD_URL. SMTP_PASSWORD belongs in /etc/sauron/secret.env.
- No new binaries, no new units.
```

- [ ] **Step 7: Verify the spec still parses and that the release matches the changelog.**
  A parse check alone passes on a spec whose `Release` was never bumped, which is
  exactly the failure that makes the upgrade instruction undeliverable, so assert
  both:

```bash
cd /home/splimter/projects/freelance/sauron && \
  spec_vr="$(rpmspec -q --qf '%{version}-%{release}\n' packaging/rpm/sauron.spec | head -1)" && \
  chg_vr="$(grep -m1 '^\* ' packaging/rpm/sauron.spec | awk '{print $NF}')" && \
  echo "spec=$spec_vr changelog=$chg_vr" && \
  case "$spec_vr" in "$chg_vr".*|"$chg_vr") echo MATCH ;; *) echo "MISMATCH: bump Release in sauron.spec"; exit 1 ;; esac
```

  Expected: `spec=1.1.0-2.fc44 changelog=1.1.0-2` (the `.fc44` is `%{?dist}` and
  varies by build host) followed by `MATCH`, and no `line N: ...` parse error on
  stderr. (If `rpmspec` is not installed, the weaker check is
  `grep -n '^Release:' packaging/rpm/sauron.spec` printing `2%{?dist}` and
  `grep -c '^\* ' packaging/rpm/sauron.spec` returning one more than before the edit.)

- [ ] **Step 8: Confirm no packaging manifest needed changing.**
  `cd /home/splimter/projects/freelance/sauron && git status --short packaging/`
  Expected: exactly three modified files — `packaging/rpm/config/api.env`, `packaging/rpm/SETUP.md`, `packaging/rpm/sauron.spec`. If `packaging/rpm/binaries.txt` or anything under `packaging/rpm/systemd/` shows up, something added a binary that this slice does not have.

---

## Task 16: End-to-end verification

**Files:** none — this task changes no code. It is the gate everything DB- and network-dependent actually passes through, because the unit and integration tests above deliberately never open a socket to a relay.

**Interfaces:** Consumes the whole slice.

- [ ] **Step 1: Full workspace green.**
  `cargo fmt --all --check`
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test --workspace`
  Expected: all three exit 0, and no test reports "skipping" — a skip here means the database variables were not exported and nothing DB-backed actually ran.

- [ ] **Step 2: Dev sink, bodies on.**
  Run the API with the sink and dev mode both on:

```bash
cd /home/splimter/projects/freelance/sauron/backend
DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu \
DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron \
REDIS_URL=redis://172.20.0.3:6379 \
JWT_SECRET=local-dev-secret-000000000000000000000000 \
SMTP_SINK=1 SAURON_DEV=1 DASHBOARD_URL=http://localhost:3000 \
RUST_LOG=info,sauron=debug \
cargo run --bin sauron-api
```

  Confirm the boot log carries the `SMTP_SINK=1: transactional email is written to the log and NEVER transmitted` warning with `log_bodies=true`, and that `curl -s localhost:8080/health | python3 -m json.tool` lists both `mail_drain` and `mail_hygiene`.
  Then insert one row by hand and watch it drain (S0 ships no sender, so a hand-written row is the only way to exercise this until S1):

```bash
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c \
"INSERT INTO mail_outbox (kind, recipient, recipient_key, subject, body_text, body_html, expires_at)
 VALUES ('password_reset','you@example.test','you@example.test','Reset your password',
         E'Reset your password\n\nChoose a new password:\nhttp://localhost:3000/#/reset-password?token=DEMO\n',
         '<p>html</p>', now() + interval '10 minutes');"
```

  Within one `MAIL_DRAIN_TICK_SECS` the log must show the header line (`mail_id`, `kind`, `to`, `subject`) **and** the plain-text body including the clickable `http://localhost:3000/#/reset-password?token=DEMO`. Then:

```bash
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c \
"SELECT status, body_text = '' AS text_blanked, body_html = '' AS html_blanked, last_error
   FROM mail_outbox ORDER BY created_at DESC LIMIT 1;"
```

  Expected: `status = sink` (**not** `sent`), both blanked columns `t`, `last_error = delivered to log sink (SMTP_SINK=1)`.

- [ ] **Step 3: Dev sink, bodies off.**
  Repeat Step 2 with `SAURON_DEV` **unset**. Expected: the boot warning now says `log_bodies=false`, the header line still logs, and the body line does **not** appear anywhere in the journal. This is the assertion that the two-variable gate is really two variables.

- [ ] **Step 4: Cleartext refusal — the boot half.**
  Start the API with `SMTP_HOST=192.168.1.20 SMTP_TLS=none SMTP_FROM=a@b.test` (and the sink off). Expected: one INFO line `transactional email disabled` whose `reason` contains `SMTP_TLS=none sends the SMTP password and password-reset links in cleartext` and names `192.168.1.20`; `/health` lists `mail_hygiene` but not `mail_drain`.

- [ ] **Step 5: Cleartext refusal — the connect half.**
  The boot check only inspects the *name* an operator wrote, so reaching the connect check needs a name that passes it and resolves elsewhere. Run the API in a container whose `/etc/hosts` maps `localhost` to a public address, leaving the host machine untouched:

```bash
cd /home/splimter/projects/freelance/sauron/backend
DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo build --bin sauron-api
docker run --rm -it \
  --add-host localhost:93.184.216.34 \
  -v "$PWD/target/debug/sauron-api:/sauron-api:ro" \
  -e DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron \
  -e REDIS_URL=redis://172.20.0.3:6379 \
  -e JWT_SECRET=local-dev-secret-000000000000000000000000 \
  -e SMTP_HOST=localhost -e SMTP_TLS=none -e SMTP_FROM=a@b.test \
  -e RUST_LOG=info,sauron=debug \
  debian:stable-slim /sauron-api
```

  Enqueue a row as in Step 2. Expected: the drain logs `SMTP_TLS=none requires SMTP_HOST to resolve to loopback; localhost resolves to 93.184.216.34`, and the row goes to `failed` after one attempt because `Blocked` is permanent. These are two genuinely different checks — the boot one validates the string an operator wrote, the connect one validates what it resolved to — and conflating them makes the second unreachable.

- [ ] **Step 6: SSRF block on the path that actually reaches the resolver.**
  Start with `SMTP_HOST=127.0.0.1 SMTP_PORT=587 SMTP_TLS=starttls SMTP_FROM=a@b.test` and `SMTP_ALLOW_PRIVATE` **unset**. Enqueue a row. Expected: the drain logs `target 127.0.0.1 resolves to a blocked address; set SMTP_ALLOW_PRIVATE=true only if the relay is deliberately on a private network`, and the row goes to `failed` after one attempt (`Blocked` is permanent), body intact.

- [ ] **Step 7: A real relay, no TLS.**
  `docker run --rm -p 1025:1025 -p 8025:8025 mailhog/mailhog`, then start with
  `SMTP_HOST=127.0.0.1 SMTP_PORT=1025 SMTP_TLS=none SMTP_ALLOW_PRIVATE=true SMTP_FROM=sauron@localhost DASHBOARD_URL=http://localhost:3000`.
  Enqueue a row. In the MailHog UI at `http://localhost:8025` confirm: a `multipart/alternative` message; the HTML part renders as a 600px card with a blue button; the text part is readable on its own with the URL on its own line; the inbox preview shows the preheader rather than raw markup. Confirm the row reaches `status = 'sent'` with both bodies blanked.

- [ ] **Step 8: Backlog drains in one tick.**
  Against the same MailHog, insert 100 rows:

```bash
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c \
"INSERT INTO mail_outbox (kind, recipient, recipient_key, subject, body_text, body_html, expires_at)
 SELECT 'smtp_test', 'bulk'||i||'@example.test', 'bulk'||i||'@example.test',
        'Bulk '||i, 'text '||i, '<p>'||i||'</p>', now() + interval '30 minutes'
   FROM generate_series(1,100) AS i;"
```

  Expected: all 100 reach `sent` within one drain tick, not 16 per minute. Confirm in MailHog that the batch was handled over a small number of connections rather than 100 — the point of `SmtpClient` existing.

- [ ] **Step 9: A real relay, real TLS, real inboxes.**
  MailHog speaks cleartext to a loopback port and renders nothing, so it exercises
  neither STARTTLS+AUTH nor a single one of the layout decisions in Task 6. This
  is the only step that does. Use a genuine relay on `:587` with credentials
  (Postmark, SES, Fastmail, a corporate postfix — anything real):

```bash
cd /home/splimter/projects/freelance/sauron/backend
DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu \
DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron \
REDIS_URL=redis://172.20.0.3:6379 \
JWT_SECRET=local-dev-secret-000000000000000000000000 \
SMTP_HOST=smtp.your-relay.test SMTP_PORT=587 SMTP_TLS=starttls \
SMTP_USERNAME=your-relay-user SMTP_PASSWORD=your-relay-password \
SMTP_FROM=sauron@your-domain.test SMTP_FROM_NAME=Sauron \
DASHBOARD_URL=https://sauron.your-domain.test \
RUST_LOG=info,sauron=debug \
cargo run --bin sauron-api
```

  The rows this step enqueues must carry the **real** rendered bodies, not the
  `'<p>html</p>'` stand-in of Step 2 — the whole point here is the layout, and S0
  ships no sender that renders one, so render it out of band. Create this
  throwaway integration test (public API only; it is deleted at the end of the
  step) at `backend/crates/sauron-mail/tests/dump_sample.rs`, replacing the two
  addresses with inboxes you control:

```rust
// TEMPORARY: renders one real message pair and writes the INSERT for it. Deleted
// at the end of this step; it exists because S0 has no runtime path that renders.
use sauron_mail::{render, Branding, Cta, MailContent};

#[test]
fn dump_sample_sql() {
    let branding = Branding {
        product_name: "Sauron".into(),
        dashboard_url: Some("https://sauron.your-domain.test".into()),
        footer: "You received this because someone asked to reset the password for this address."
            .into(),
    };
    let content = MailContent {
        subject: "Reset your password".into(),
        heading: "Reset your password".into(),
        paragraphs: vec![
            "Someone asked to reset the password for your Sauron account.".into(),
            "If that was not you, ignore this email — nothing has changed.".into(),
        ],
        cta: Some(
            Cta::new(
                "Choose a new password",
                branding.link("/reset-password?token=DEMO").unwrap(),
            )
            .unwrap(),
        ),
        footnotes: vec!["This link expires in 30 minutes.".into()],
    };
    let out = render(&branding, &content).unwrap();

    // Dollar-quoted so neither the HTML's apostrophes nor its newlines need
    // escaping on the way through psql.
    let mut sql = String::new();
    for to in ["you@gmail.com", "you@outlook.com"] {
        sql.push_str(&format!(
            "INSERT INTO mail_outbox \
             (kind, recipient, recipient_key, subject, body_text, body_html, expires_at) VALUES \
             ('password_reset', '{to}', '{to}', $subj${}$subj$, $txt${}$txt$, $html${}$html$, \
             now() + interval '30 minutes');\n",
            out.subject, out.text, out.html
        ));
    }
    std::fs::write("/tmp/mail_sample.sql", sql).unwrap();
}
```

```bash
cd /home/splimter/projects/freelance/sauron/backend
DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu \
  cargo test -p sauron-mail --test dump_sample
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -f /tmp/mail_sample.sql
rm -r /home/splimter/projects/freelance/sauron/backend/crates/sauron-mail/tests
```

  Expected: `test result: ok. 1 passed`, then `INSERT 0 1` twice, and
  `ls backend/crates/sauron-mail/tests 2>&1` reporting no such directory — the
  scratch test must not survive the step.
  Expected, on the server side: `status = 'sent'` for both rows, one attempt each,
  both bodies blanked, and no `Tls`, `Send` or `Rejected` error in the log — that
  is STARTTLS plus AUTH working, which no other step in this plan reaches.
  Then open all three clients and eyeball them, because this is the only evidence
  that Task 6's tables-not-divs, `width="600"`-for-Word, no-`<img>`, bulletproof-button
  choices were worth making:
  - **Gmail** (web) and **Outlook.com** (web), plus **one mobile client** (the
    Gmail or Outlook app).
  - The card is capped at 600px and does not run edge to edge on desktop, and does
    not overflow horizontally on the phone.
  - The CTA renders as a filled button with readable label text in all three — not
    as a bare link, and not as a button with invisible text in dark mode.
  - The **inbox preview list** shows the preheader sentence, not raw markup and
    not the beginning of the CSS. A preheader failure is only ever visible here,
    never in the opened message.
  If any client fails, fix Task 6 and re-run — this step is the reason that task's
  markup is shaped the way it is.

- [ ] **Step 10: Alerting regression — success path.**
  Configure a notification channel of kind `email` pointing at the same MailHog (`ALERTS_ALLOW_PRIVATE=true`), then `POST /v1/notification-channels/{id}/test`. Expected: `{"ok":true,"attempts":1}`, and the delivered message is byte-identical to before this slice — `text/plain`, subject `[Sauron/info] Test alert from Sauron (...)`, body ending `— Sauron alerting`. Diff it against a message captured before the refactor if one is available.

- [ ] **Step 11: Alerting regression — failure path.**
  Point the same channel at a dead host and `POST .../test` again. Expected: `{"ok":false,...,"error":"smtp send failed: ..."}` — the same string shape as before, because `test_channel` surfaces it verbatim and persists it to `alert_events`. The one deliberate behaviour change to note in the release: a tarpitting relay now fails at the total deadline (`smtp send failed: deadline exceeded after Nms`) instead of hanging.

- [ ] **Step 12: Migration reversibility.**
  ```bash
  cd /home/splimter/projects/freelance/sauron/backend
  psql postgres://sauron:sauron@172.20.0.2:5432/sauron -f migrations/2026-08-01-000034_mail_outbox/down.sql
  psql postgres://sauron:sauron@172.20.0.2:5432/sauron -f migrations/2026-08-01-000034_mail_outbox/up.sql
  ```
  Expected: both apply cleanly. Then re-run `DATABASE_URL=... cargo run --bin sauron-migrate` and confirm it reports nothing pending — the `__diesel_schema_migrations` row was never removed, so this is checking the SQL, not the harness.

- [ ] **Step 13: Missing-table diagnosis.**
  With the API running against a database where `mail_outbox` has been dropped (`DROP TABLE mail_outbox;`), confirm the drain logs the one-shot `mail_outbox does not exist — this deployment was upgraded without running sauron-migrate` ERROR **once**, not once per tick, and that `/health` still returns 200. Recreate the table with `up.sql` afterwards.

---

## Definition of done

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` (with `TEST_DATABASE_URL` and `TEST_REDIS_URL` set) all pass.
- `sauron-alerts` no longer lists `lettre` in its `Cargo.toml`, and `POST /v1/notification-channels/{id}/test` produces a byte-identical message and a byte-identical error string.
- A hand-written `mail_outbox` row reaches `status='sent'` against a real relay and `status='sink'` against the log sink, with both body columns empty in each case.
- A rendered message delivered over STARTTLS+AUTH by a real relay on `:587` has been opened in Gmail, in Outlook.com and in one mobile client: 600px cap, a real button, and the preheader — not markup — in the inbox preview list (Task 16 Step 9). Nothing else in this plan renders the template in a mail client.
- `packaging/rpm/sauron.spec` carries a `Release` that is higher than the shipped one and matches its topmost `%changelog` entry, so `dnf upgrade` will actually install the build that tells the operator to run `sauron-migrate`.
- `sauron-api` boots and serves on a deployment with no SMTP configuration at all, and `/health` lists `mail_hygiene` there.
- `perm::ALL` is unchanged, `dashboard/` is unchanged, and `git status --short dashboard/` is empty.
- **Nothing is committed.** The repository owner commits manually.
