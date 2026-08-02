# Per-user Notification Preferences (S3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let any authenticated user subscribe *themselves* to uptime and error notifications for a project or app (optionally narrowed to environments), delivered to their own `users.email`, with per-recipient throttling, digests, quiet hours and a one-click unsubscribe.

**Architecture:** A `notification_subscriptions` table (one row per `(user, scope, kind)`) plus a catalogue-environment child table drives a second evaluation pass inside the existing `sauron-alerts` tick. Producers (that pass, and `sauron-monitor`'s inline transition handler) only INSERT into a channel-agnostic `notification_queue`; a separate drain pass in the same binary claims rows with `FOR UPDATE SKIP LOCKED`, re-checks authorization against freshly loaded RBAC grants, groups by user, renders one email and hands it to S0's `mail_outbox`. All pure decision logic (kind parsing, spike predicate, quiet-hours wrap-around, probe coalescing, the coverage predicate) lives in a new I/O-free `sauron-alerts/src/subscription.rs` so it is unit-testable without a database.

**Tech Stack:** Rust 1.82 (MSRV), diesel 2 + diesel-async + Postgres, `sauron-redis` (`SET NX EX` throttle), `sauron-mail` (S0), `sauron-auth` RBAC (`Reach`/`reach_for`), axum 0.8, Svelte 5 runes + vitest.

---

## Global Constraints

- **NEVER run the diesel CLI.** `backend/crates/sauron-db/src/schema.rs` is hand-maintained. A new table means three hand edits: a `diesel::table!` block, a `diesel::joinable!` line per FK, and the name appended to `allow_tables_to_appear_in_same_query!`.
- **NEVER use `conn.transaction(...)`.** The MSRV blocks it. Multi-statement atomicity is one data-modifying CTE issued through `diesel::sql_query` with `.bind()`.
- **All SQL lives in `backend/crates/sauron-db/src/repo.rs`** as free `pub async fn name(conn: &mut AsyncPgConnection, ...) -> QueryResult<T>`. Handlers never build queries inline.
- **Insertable-only structs must NOT derive `Queryable`.** `Queryable` decodes positionally and would silently bind fields to the wrong columns.
- **Never hold a pooled `PgConn` across network I/O.** The API pool is 16 connections for the whole process; `sauron-alerts` uses 8. `drop(conn)` before any fan-out.
- **Enum-like columns are `TEXT` + `CHECK`**, never a custom SQL type.
- **Migrations** are `backend/migrations/YYYY-MM-DD-0000NN_slug/{up,down}.sql`. **Both files are required.** `up.sql` opens with a prose comment explaining WHY. A migration runs in ONE transaction, `CONCURRENTLY` is unavailable, and an index build on a partitioned parent locks every child.
- **Dashboard:** house UI components only. There is NO `Select`, `Toggle`, `Tabs` or `Menu` primitive — a dropdown is a raw `<select class="sel">`. A new page needs three edits (page file, `src/routes.ts`, `Sidebar.svelte` groups array); this slice adds exactly one new page (`/unsubscribe`) and it deliberately gets **no** Sidebar entry.
- **Pure decision logic goes in `dashboard/src/lib/models/*.ts` with a colocated `*.test.ts`** — there is NO DOM test environment, so `.svelte` files render only.
- **Svelte 5 runes.** `$state` deep-proxies values so `===` never matches a raw value; use `$state.raw` when identity matters. `Set`s and `Record`s in `$state` are **replaced**, never mutated in place.
- **Comments explain the failure mode that motivated the code**, not what the code does. Match that register.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` are hard gates.
- **Never commit, never `git add`, never create a branch.** The repository owner commits manually.

### Fixed values copied from the design (do not re-derive)

| Name | Value |
|---|---|
| Migration | `2026-08-01-000037_notification_subscriptions` |
| `MAX_SUBSCRIPTIONS_PER_USER` | `50` (compile-time const, 409 at the limit) |
| `UNSUB_TOKEN_TTL_DAYS` | `90` (compile-time const) |
| `STUCK_CLAIM_SECS` | `900` (15 minutes) |
| `MAX_QUEUE_ATTEMPTS` | `3` |
| `NOTIFY_SUBS_TICK_SECS` | default `120`, clamp `30..3600` |
| `NOTIFY_SUBS_BATCH` | default `200`, clamp `1..5000` |
| `NOTIFY_SUBS_MAX_PROBES_PER_ORG` | default `50`, clamp `1..1000` |
| `NOTIFY_DRAIN_BUDGET_MS` | default `10000`, clamp `500..60000` |
| `NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR` | default `20`, clamp `1..1000` |
| `NOTIFY_QUEUE_RETENTION_DAYS` | default `14`, clamp `1..365` |
| `error_spike` defaults | `window_seconds` 900 (clamp 300..86400), `factor` 3.0 (clamp 1.5..100), `min_count` 10 (clamp 1..100000), `level` `null` |
| `error_new_issue` / `error_regression` default | `level` `"error"` |
| Default `throttle_seconds` | `900` (CHECK 0..604800) |
| Uptime permission | `perm::MONITOR_READ` |
| Error-kind permission | `perm::ISSUE_READ` |

---

## Preconditions (verify before Task 1)

S3 is the **fourth** slice in the programme. It consumes foundations built by S0 and S2. Run these checks first; if any fails, stop — the missing slice must land first.

```
cd /home/splimter/projects/freelance/sauron
grep -rn "PersonalNotification" backend/crates/sauron-mail/src/kind.rs
grep -rn "pub async fn enqueue_mail" backend/crates/sauron-db/src/repo.rs
grep -rn "pub fn render" backend/crates/sauron-mail/src/template.rs
grep -rn "pub(crate) async fn rate_limit" backend/bins/sauron-api/src/routes/auth.rs
grep -rn "pub(crate) fn client_addr" backend/bins/sauron-api/src/routes/auth.rs
ls dashboard/src/pages/Account.svelte dashboard/src/lib/api/account.ts
ls backend/migrations | tail -5
```

What each gives S3:

| Name | Owner | S3 uses it for |
|---|---|---|
| `sauron_mail::MailKind::PersonalNotification` (`dedup_window()` is zero) | S0 | the `kind` on every outbox row S3 writes |
| `sauron_mail::{Branding, MailContent, Cta, render, RenderedMail}` | S0 | rendering the notification and unsubscribe-confirmation bodies |
| `sauron_mail::text::{html_escape, substitute}` | S0 | escaping user-supplied issue titles before they enter HTML |
| `repo::enqueue_mail(conn, NewMailOutbox<'_>, ttl_secs, dedup_secs, commit)` and `models::NewMailOutbox<'a>` | S0 | the drain's single write into `mail_outbox` |
| `Config::require_dashboard_url()` / `Config::dashboard_url` | S0 | building `{DASHBOARD_URL}/#/unsubscribe?token=…` |
| `packaging/rpm/SETUP.md` §11 "Upgrading" with its per-migration table | S0 | Task 26 appends one row |
| CI assertion that every `parse("KEY"` literal in `config.rs` appears in `.env.example` | S0 | Task 8's six keys must satisfy it |
| `pub(crate) rate_limit` / `pub(crate) client_addr` in `routes/auth.rs` | S2 | the unauthenticated unsubscribe limiter |
| `dashboard/src/pages/Account.svelte` as a **card container** | S2 | Task 24 adds a Notifications card; no `routes.ts` edit |

**Migration date prefix.** The directory is `2026-08-01-000037_notification_subscriptions` per the programme allocation. Diesel orders by the full `YYYY-MM-DD-0000NN` string, **date first**, so if S0/S1/S2 landed with a date prefix **later** than `2026-08-01`, use a date greater than or equal to the highest already on disk (from the `ls backend/migrations | tail -5` above) while keeping `000037`. Never backdate.

**`schema.rs` delta is +4 tables.** Never assert an absolute table count anywhere.

---

## File Structure

### Created

| Path | Responsibility |
|---|---|
| `backend/migrations/2026-08-01-000037_notification_subscriptions/up.sql` | `notification_subscriptions`, `notification_subscription_envs`, `notification_queue`, `notification_queue_envs` + indexes + autovacuum settings + `COMMENT ON COLUMN` marking which id space each env column holds |
| `backend/migrations/2026-08-01-000037_notification_subscriptions/down.sql` | Drops the four tables in FK order |
| `backend/crates/sauron-alerts/src/subscription.rs` | The whole pure decision surface: `SubKind`, `SubConditions`, `spike_fires`, `in_quiet_hours`, `CondBucket`, `ProbeKey`, `SubInput`, `Probe`, `coalesce`, `QueueTarget`, `covers`. No diesel, no axum, no network |
| `backend/crates/sauron-alerts/src/sweep.rs` | `sweep_user_subscriptions` + the daily `sweep_revoked_subscriptions`. In the library crate because both `sauron-api` (synchronous, from the grant handlers) and `sauron-alerts`' tick (the daily backstop) call it |
| `backend/bins/sauron-alerts/src/subs.rs` | The subscription evaluation pass: load, coalesce, probe, fan out by app id, throttle, enqueue |
| `backend/bins/sauron-alerts/src/drain.rs` | The drain: claim, re-check reach, group by user, render, hand to `mail_outbox` |
| `backend/crates/sauron-db/tests/notifications.rs` | DB-backed integration tests: the env-resolution regression, claim exclusivity, requeue, the upsert CTE, `deliver_after`, prune, dedup, the delivery-time re-check |
| `backend/bins/sauron-api/src/routes/notification_prefs.rs` | The six `/v1/me/notification-subscriptions*`, `/v1/me/notifications` and `/v1/notifications/unsubscribe` handlers plus their write-time authorization |
| `dashboard/src/lib/models/notification-prefs.ts` | DOM-free decision logic: `selectionToSubscriptionScope`, `kindSupportsEnvFilter`, `kindScopeTypes`, `clampConditions`, `describeSubscription`, `quietHoursLabel`, `validateSubscription` |
| `dashboard/src/lib/models/notification-prefs.test.ts` | vitest over the above |
| `dashboard/src/lib/api/notification-prefs.ts` | One thin wrapper per endpoint returning `data` |
| `dashboard/src/lib/components/account/NotificationSubscriptions.svelte` | The Notifications card: `DataTable`, per-row enable/disable, delete `ConfirmDialog`, "New subscription" button |
| `dashboard/src/lib/components/account/SubscriptionDialog.svelte` | `Modal(size='lg')` create/edit form: kind `<select>`, `ScopeTree`, catalogue-env chip row, per-kind condition inputs, delivery, throttle, quiet hours |
| `dashboard/src/pages/Unsubscribe.svelte` | No-conditions SPA route that POSTs the token and reports the result |
| `wiki/Notifications.md` | User-facing guide to personal subscriptions |

### Modified

| Path | Change |
|---|---|
| `backend/crates/sauron-db/src/schema.rs` | +4 `table!` blocks, +5 `joinable!` lines, +4 names in `allow_tables_to_appear_in_same_query!` |
| `backend/crates/sauron-db/src/models.rs` | `NotificationSubscription`, `NewNotificationSubscription<'a>`, `NotificationQueueItem`, `NewNotificationQueueItem<'a>`, `NotificationSubscriptionEnv`, `NotificationQueueEnv` |
| `backend/crates/sauron-db/src/repo.rs` | Two env resolvers, the `alert_count_*` signature change, `alert_count_errors_by_app`, `alert_new_issues_env`/`alert_regressed_issues_env`, and ~25 subscription/queue functions |
| `backend/crates/sauron-alerts/src/lib.rs` | `pub mod subscription;`, `pub mod sweep;` + module-map doc lines |
| `backend/crates/sauron-alerts/src/crypto.rs` | `derive_unsub_key`, `unsubscribe_token`, `verify_unsubscribe_token`, `ct_eq` |
| `backend/crates/sauron-alerts/Cargo.toml` | `sauron-auth.workspace = true` (needed by `covers` and by `sweep`) |
| `backend/crates/sauron-db/Cargo.toml` | `sauron-alerts.workspace = true` under `[dev-dependencies]` — the `deliver_after` test asserts the SQL `CASE` and Rust's `in_quiet_hours` agree. **Dev only**: `sauron-alerts` depends on `sauron-db`, so a normal dependency is a cycle |
| `backend/bins/sauron-monitor/Cargo.toml` | `sauron-auth.workspace = true` — `notify_transition` runs `covers()` against freshly loaded grants |
| `backend/crates/sauron-core/src/config.rs` | Six `notify_*` fields |
| `backend/bins/sauron-alerts/src/main.rs` | Legacy env-name resolution for `alert_rules`, plus five new in-tick sub-jobs |
| `backend/bins/sauron-alerts/Cargo.toml` | `sauron-mail.workspace = true`, `sauron-auth.workspace = true` |
| `backend/bins/sauron-monitor/src/main.rs` | `notify_transition` enqueues personal uptime rows before the rule check, and `rules.is_empty()` stops being a function return |
| `backend/bins/sauron-api/src/main.rs` | Six route registrations |
| `backend/bins/sauron-api/src/error.rs` | `ApiError::Unprocessable(String)` → 422 |
| `backend/bins/sauron-api/src/routes/mod.rs` | `pub mod notification_prefs;` |
| `backend/bins/sauron-api/src/routes/notifications.rs` | `subscription_kinds` key on `/v1/alert-meta` |
| `backend/bins/sauron-api/src/routes/orgs.rs` | Synchronous revocation sweep from `delete_grant`, `update_grant_handler`, `set_member_active` |
| `dashboard/src/lib/components/members/ScopeTree.svelte` | Additive `allowOrg?: boolean = true`, `allowEnv?: boolean = true` |
| `dashboard/src/lib/models/index.ts` | `NotificationSubscription`, `SubscriptionKind`, `SubscriptionDelivery`, `SubscriptionConditions`, `NotificationQueueItem`, `subscription_kinds` on `AlertMeta` |
| `dashboard/src/pages/Account.svelte` | One `<NotificationSubscriptions />` card |
| `dashboard/src/routes.ts` | `'/unsubscribe': wrap({ component: Unsubscribe, conditions: [] })` |
| `README.md`, `.env.example`, `docker-compose.yml`, `packaging/rpm/config/alerts.env`, `packaging/rpm/SETUP.md`, `wiki/_Sidebar.md`, `wiki/Home.md` | Six config keys documented four times; upgrade-table row; wiki registration |

---

## Task 1: Migration 000037, `schema.rs`, `models.rs`

**Files:**
- Create `backend/migrations/2026-08-01-000037_notification_subscriptions/up.sql`
- Create `backend/migrations/2026-08-01-000037_notification_subscriptions/down.sql`
- Modify `backend/crates/sauron-db/src/schema.rs` (append blocks after the `workflows` block near line 452; `joinable!` lines after line 501; names inside `allow_tables_to_appear_in_same_query!` at lines 503-533)
- Modify `backend/crates/sauron-db/src/models.rs` (append at the end, before `#[cfg(test)] mod tests` at line 881)
- Create `backend/crates/sauron-db/tests/notifications.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: tables `notification_subscriptions`, `notification_subscription_envs`, `notification_queue`, `notification_queue_envs`; structs `NotificationSubscription`, `NewNotificationSubscription<'a>`, `NotificationQueueItem`, `NewNotificationQueueItem<'a>`, `NotificationSubscriptionEnv`, `NotificationQueueEnv`.

- [ ] **Step 1: Write the failing round-trip test.** Create `backend/crates/sauron-db/tests/notifications.rs`:

```rust
//! S3 personal notification subscriptions, against a real Postgres database.
//!
//! Skips (rather than fails) when `TEST_DATABASE_URL` is unset, matching
//! `env_scoping.rs` and `workflows.rs` — CI has no Postgres service.

mod common;

use common::TestDb;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use sauron_db::models::{NewNotificationSubscription, NotificationSubscription};
use sauron_db::schema::notification_subscriptions;
use serde_json::json;

/// `Queryable` decodes positionally, so a struct whose field order drifts from
/// the `table!` block binds `disabled_reason` into `scope_type` and compiles
/// silently. Reading a known row back through `as_select()` and asserting each
/// value is the only thing that catches it.
#[tokio::test]
async fn subscription_row_round_trips_in_declared_column_order() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let user_id = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .expect("load harness owner")
        .expect("harness owner exists")
        .id;

    let conditions = json!({ "window_seconds": 900, "factor": 3.0, "min_count": 10 });
    let inserted: NotificationSubscription = diesel::insert_into(notification_subscriptions::table)
        .values(NewNotificationSubscription {
            user_id,
            org_id: ids.org_id,
            scope_type: "project",
            scope_id: ids.project_id,
            kind: "error_spike",
            conditions: &conditions,
            delivery: "immediate",
            throttle_seconds: 900,
            quiet_start_min: Some(1320),
            quiet_end_min: Some(360),
            quiet_tz: "Europe/Paris",
        })
        .returning(NotificationSubscription::as_returning())
        .get_result(&mut conn)
        .await
        .expect("insert subscription");

    let read: NotificationSubscription = notification_subscriptions::table
        .find(inserted.id)
        .select(NotificationSubscription::as_select())
        .first(&mut conn)
        .await
        .expect("read subscription back");

    assert_eq!(read.user_id, user_id);
    assert_eq!(read.org_id, ids.org_id);
    assert_eq!(read.scope_type, "project");
    assert_eq!(read.scope_id, ids.project_id);
    assert_eq!(read.kind, "error_spike");
    assert!(read.enabled);
    assert_eq!(read.disabled_reason, None);
    assert_eq!(read.disabled_at, None);
    assert_eq!(read.conditions, conditions);
    assert_eq!(read.delivery, "immediate");
    assert_eq!(read.throttle_seconds, 900);
    assert_eq!(read.quiet_start_min, Some(1320));
    assert_eq!(read.quiet_end_min, Some(360));
    assert_eq!(read.quiet_tz, "Europe/Paris");

    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and watch it fail to compile.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test notifications`
  Expected: `error[E0432]: unresolved import sauron_db::models::NewNotificationSubscription` and `unresolved import sauron_db::schema::notification_subscriptions`.

- [ ] **Step 3: Write `up.sql`.** Create `backend/migrations/2026-08-01-000037_notification_subscriptions/up.sql`:

```sql
-- Per-user notification subscriptions. Every notification this product could
-- send before this migration was org-owned and admin-typed: an `alert_rules`
-- row belongs to an organization and its `notification_channels` carry a
-- static recipient list somebody pasted into a dialog. A developer who wanted
-- to know when their own app broke had to ask an admin, and everyone else on
-- that channel got it too.
--
-- READ THIS BEFORE TOUCHING EITHER `environment_id` COLUMN. There are two
-- environment id spaces in this schema and getting them backwards produces a
-- subscription that looks right in the database and matches nothing, silently:
--
--   * `environments`      -- the PROJECT-LEVEL CATALOGUE (since migration 33).
--                            One row means "prod, everywhere in this project".
--   * `app_environments`  -- the PER-APP ENROLLMENT. This is what
--                            `error_events.environment_id`,
--                            `analytics_events.environment_id` and
--                            `role_grants.scope_id` (scope_type='env') hold.
--
-- `notification_subscription_envs.environment_id` stores CATALOGUE ids on
-- purpose: the catalogue is exactly the wildcard RBAC lacks, and it stays
-- correct when a new app is created and auto-enrolled. Storing enrollment ids
-- would freeze the set at creation time.
-- `notification_queue_envs.environment_id` stores ENROLLMENT ids, because that
-- is what the event rows the body was computed from actually carry.
--
-- `notification_queue` exists rather than enqueueing straight into
-- `mail_outbox` so that producers never send mail. That split is what lets
-- `sauron-monitor` participate in personal uptime notifications without ever
-- learning about SMTP, and it is what makes delivery exclusive across
-- replicas via a `claimed` status.

CREATE TABLE notification_subscriptions (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id           UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id            UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    scope_type        TEXT NOT NULL CHECK (scope_type IN ('project','app')),
    -- Polymorphic with no FK, exactly like `role_grants.scope_id`: a row can
    -- outlive its target, so every read path must tolerate an unresolvable id.
    scope_id          UUID NOT NULL,
    kind              TEXT NOT NULL CHECK (kind IN
                          ('uptime','error_spike','error_new_issue','error_regression')),
    enabled           BOOLEAN NOT NULL DEFAULT true,
    disabled_reason   TEXT CHECK (disabled_reason IN ('unsubscribed','access_revoked')),
    disabled_at       TIMESTAMPTZ,
    conditions        JSONB NOT NULL DEFAULT '{}'::jsonb,
    delivery          TEXT NOT NULL DEFAULT 'immediate'
                          CHECK (delivery IN ('immediate','hourly','daily')),
    throttle_seconds  INT NOT NULL DEFAULT 900 CHECK (throttle_seconds BETWEEN 0 AND 604800),
    quiet_start_min   SMALLINT CHECK (quiet_start_min BETWEEN 0 AND 1439),
    quiet_end_min     SMALLINT CHECK (quiet_end_min BETWEEN 0 AND 1439),
    -- An IANA name, validated at write time against `pg_timezone_names`. The
    -- enqueue re-checks it: a zone that validated at write time can vanish with
    -- an OS tzdata update, and `now() AT TIME ZONE 'Missing/Zone'` raises.
    quiet_tz          TEXT NOT NULL DEFAULT 'UTC',
    -- Seeded to now() at INSERT so a brand-new subscription cannot retro-storm
    -- over whatever backlog already exists.
    last_evaluated_at TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((quiet_start_min IS NULL) = (quiet_end_min IS NULL))
);

CREATE UNIQUE INDEX notification_subscriptions_user_scope_kind_key
    ON notification_subscriptions (user_id, scope_type, scope_id, kind);
CREATE INDEX notification_subscriptions_kind_idx
    ON notification_subscriptions (kind) WHERE enabled;
CREATE INDEX notification_subscriptions_user_idx ON notification_subscriptions (user_id);
CREATE INDEX notification_subscriptions_org_idx  ON notification_subscriptions (org_id);

-- Composite PK with no surrogate id, mirroring `alert_rule_channels`.
-- An EMPTY set means all environments, including unattributed events.
CREATE TABLE notification_subscription_envs (
    subscription_id UUID NOT NULL REFERENCES notification_subscriptions(id) ON DELETE CASCADE,
    environment_id  UUID NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    PRIMARY KEY (subscription_id, environment_id)
);
CREATE INDEX notification_subscription_envs_env_idx
    ON notification_subscription_envs (environment_id);

COMMENT ON COLUMN notification_subscription_envs.environment_id IS
    'CATALOGUE id (environments.id), NOT an app_environments enrollment id.';

CREATE TABLE notification_queue (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id       UUID NOT NULL REFERENCES notification_subscriptions(id) ON DELETE CASCADE,
    user_id               UUID NOT NULL,
    org_id                UUID NOT NULL,
    project_id            UUID NOT NULL,
    app_id                UUID,
    includes_unattributed BOOLEAN NOT NULL DEFAULT false,
    kind                  TEXT NOT NULL,
    dedup_key             TEXT NOT NULL,
    severity              TEXT NOT NULL DEFAULT 'warning'
                              CHECK (severity IN ('info','warning','critical')),
    -- Nullable because the drain blanks all three in the same UPDATE that marks
    -- a row `dropped_no_access`: the content has no further purpose and must not
    -- sit at rest for the retention window outside the reader's authorization.
    title                 TEXT,
    body                  TEXT,
    link                  TEXT,
    occurred_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    deliver_after         TIMESTAMPTZ NOT NULL,
    status                TEXT NOT NULL DEFAULT 'pending' CHECK (status IN
                              ('pending','claimed','sent','dropped_no_access',
                               'dropped_inactive','dropped_unsubscribed','failed')),
    attempts              SMALLINT NOT NULL DEFAULT 0,
    message_id            UUID,
    claimed_at            TIMESTAMPTZ,
    sent_at               TIMESTAMPTZ,
    finished_at           TIMESTAMPTZ,
    error                 TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- DELIBERATELY NO FOREIGN KEY on environment_id. A cascade delete would
-- silently SHRINK a row's environment list, and an empty list is read as "the
-- body spans everything" -- so a deleted enrollment would WIDEN a queue row's
-- implied scope instead of narrowing it. An unresolvable enrollment id is
-- simply unreachable at drain time, which fails closed.
CREATE TABLE notification_queue_envs (
    queue_id       UUID NOT NULL REFERENCES notification_queue(id) ON DELETE CASCADE,
    environment_id UUID NOT NULL,
    PRIMARY KEY (queue_id, environment_id)
);

COMMENT ON COLUMN notification_queue_envs.environment_id IS
    'ENROLLMENT id (app_environments.id), NOT a catalogue environments.id. No FK by design.';

CREATE INDEX notification_queue_due_idx
    ON notification_queue (deliver_after) WHERE status = 'pending';

-- The explicit ON CONFLICT target for the enqueue. Without a unique constraint
-- `ON CONFLICT DO NOTHING` can only ever fire on the id PK -- i.e. never -- and
-- the clause would read as idempotency while providing none. Scoped to LIVE
-- rows so a row that already sent does not block the next legitimate one.
CREATE UNIQUE INDEX notification_queue_live_dedup_key
    ON notification_queue (subscription_id, dedup_key) WHERE status IN ('pending','claimed');

CREATE INDEX notification_queue_user_created_idx ON notification_queue (user_id, created_at DESC);
CREATE INDEX notification_queue_user_sent_idx
    ON notification_queue (user_id, sent_at DESC) WHERE status = 'sent';
CREATE INDEX notification_queue_finished_idx
    ON notification_queue (finished_at) WHERE finished_at IS NOT NULL;

-- Unlike `alert_events`, this is a work queue: every notification costs one
-- INSERT plus two UPDATEs, `status` appears in a partial index predicate so
-- neither update is HOT-eligible, and three heap versions per row against
-- default autovacuum thresholds leaves a bloated heap the prune must scan.
ALTER TABLE notification_queue
    SET (autovacuum_vacuum_scale_factor = 0.02, autovacuum_analyze_scale_factor = 0.01);
```

- [ ] **Step 4: Write `down.sql`.** Create `backend/migrations/2026-08-01-000037_notification_subscriptions/down.sql`:

```sql
-- Reverses 2026-08-01-000037 exactly, in foreign-key order (children first).
DROP TABLE IF EXISTS notification_queue_envs;
DROP TABLE IF EXISTS notification_queue;
DROP TABLE IF EXISTS notification_subscription_envs;
DROP TABLE IF EXISTS notification_subscriptions;
```

- [ ] **Step 5: Apply the migration.**
  `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`
  Expected: it prints the new migration version and exits 0.

- [ ] **Step 6: Hand-edit `schema.rs` — the four `table!` blocks.** In `backend/crates/sauron-db/src/schema.rs`, immediately after the `workflows` `table!` block (before the `joinable!` lines starting at line 469), insert:

```rust
diesel::table! {
    notification_subscriptions (id) {
        id -> Uuid,
        user_id -> Uuid,
        org_id -> Uuid,
        scope_type -> Text,
        scope_id -> Uuid,
        kind -> Text,
        enabled -> Bool,
        disabled_reason -> Nullable<Text>,
        disabled_at -> Nullable<Timestamptz>,
        conditions -> Jsonb,
        delivery -> Text,
        throttle_seconds -> Int4,
        quiet_start_min -> Nullable<Int2>,
        quiet_end_min -> Nullable<Int2>,
        quiet_tz -> Text,
        last_evaluated_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    notification_subscription_envs (subscription_id, environment_id) {
        subscription_id -> Uuid,
        environment_id -> Uuid,
    }
}

diesel::table! {
    notification_queue (id) {
        id -> Uuid,
        subscription_id -> Uuid,
        user_id -> Uuid,
        org_id -> Uuid,
        project_id -> Uuid,
        app_id -> Nullable<Uuid>,
        includes_unattributed -> Bool,
        kind -> Text,
        dedup_key -> Text,
        severity -> Text,
        title -> Nullable<Text>,
        body -> Nullable<Text>,
        link -> Nullable<Text>,
        occurred_at -> Timestamptz,
        deliver_after -> Timestamptz,
        status -> Text,
        attempts -> Int2,
        message_id -> Nullable<Uuid>,
        claimed_at -> Nullable<Timestamptz>,
        sent_at -> Nullable<Timestamptz>,
        finished_at -> Nullable<Timestamptz>,
        error -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    notification_queue_envs (queue_id, environment_id) {
        queue_id -> Uuid,
        environment_id -> Uuid,
    }
}
```

- [ ] **Step 7: Hand-edit `schema.rs` — the five `joinable!` lines.** After `diesel::joinable!(workflows -> app_environments (environment_id));` (line 501), append:

```rust
diesel::joinable!(notification_subscriptions -> users (user_id));
diesel::joinable!(notification_subscriptions -> organizations (org_id));
diesel::joinable!(notification_subscription_envs -> notification_subscriptions (subscription_id));
diesel::joinable!(notification_queue -> notification_subscriptions (subscription_id));
diesel::joinable!(notification_queue_envs -> notification_queue (queue_id));
```

- [ ] **Step 8: Hand-edit `schema.rs` — the allow-list.** Inside `diesel::allow_tables_to_appear_in_same_query!( … );`, replace the final `    workflows,` line with:

```rust
    workflows,
    notification_subscriptions,
    notification_subscription_envs,
    notification_queue,
    notification_queue_envs,
```

- [ ] **Step 9: Add the model structs.** In `backend/crates/sauron-db/src/models.rs`, immediately before `#[cfg(test)]` (line 881), append:

```rust
// ---------------------------------------------------------------------------
// Personal notification subscriptions (S3)
// ---------------------------------------------------------------------------

/// One row per `(user, scope, kind)`. `scope_id` is polymorphic with no FK, so
/// a row can outlive its target and every read path must tolerate an
/// unresolvable id.
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = notification_subscriptions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NotificationSubscription {
    pub id: Uuid,
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
    pub kind: String,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    pub disabled_at: Option<DateTime<Utc>>,
    pub conditions: Value,
    pub delivery: String,
    pub throttle_seconds: i32,
    pub quiet_start_min: Option<i16>,
    pub quiet_end_min: Option<i16>,
    pub quiet_tz: String,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Insertable only. Deliberately NOT `Queryable`: that derive decodes
/// positionally, so a field order that drifts from the `table!` block would
/// bind values to the wrong columns without a compile error.
#[derive(Debug, Insertable)]
#[diesel(table_name = notification_subscriptions)]
pub struct NewNotificationSubscription<'a> {
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub scope_type: &'a str,
    pub scope_id: Uuid,
    pub kind: &'a str,
    pub conditions: &'a Value,
    pub delivery: &'a str,
    pub throttle_seconds: i32,
    pub quiet_start_min: Option<i16>,
    pub quiet_end_min: Option<i16>,
    pub quiet_tz: &'a str,
}

/// `environment_id` here is a **catalogue** `environments.id`, never an
/// `app_environments` enrollment id.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, Serialize)]
#[diesel(table_name = notification_subscription_envs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NotificationSubscriptionEnv {
    pub subscription_id: Uuid,
    pub environment_id: Uuid,
}

/// `QueryableByName` as well as `Queryable`, because the drain's claim is a
/// `sql_query ... RETURNING *`.
#[derive(Debug, Clone, Queryable, Selectable, QueryableByName, Serialize)]
#[diesel(table_name = notification_queue)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NotificationQueueItem {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub project_id: Uuid,
    pub app_id: Option<Uuid>,
    pub includes_unattributed: bool,
    pub kind: String,
    pub dedup_key: String,
    pub severity: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub link: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub deliver_after: DateTime<Utc>,
    pub status: String,
    pub attempts: i16,
    pub message_id: Option<Uuid>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Insertable only, for the tests that seed the queue directly. The production
/// enqueue path is `repo::enqueue_notifications`, one data-modifying CTE.
#[derive(Debug, Insertable)]
#[diesel(table_name = notification_queue)]
pub struct NewNotificationQueueItem<'a> {
    pub subscription_id: Uuid,
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub project_id: Uuid,
    pub app_id: Option<Uuid>,
    pub includes_unattributed: bool,
    pub kind: &'a str,
    pub dedup_key: &'a str,
    pub severity: &'a str,
    pub title: Option<&'a str>,
    pub body: Option<&'a str>,
    pub link: Option<&'a str>,
    pub deliver_after: DateTime<Utc>,
}

/// `environment_id` here is an **enrollment** `app_environments.id`.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, Serialize)]
#[diesel(table_name = notification_queue_envs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NotificationQueueEnv {
    pub queue_id: Uuid,
    pub environment_id: Uuid,
}
```

- [ ] **Step 10: Run the test against a real database and watch it pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p sauron-db --test notifications`
  Expected: `test subscription_row_round_trips_in_declared_column_order ... ok`.

- [ ] **Step 11: Confirm the whole workspace still checks and is formatted.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets && cargo fmt --all --check`
  Expected: no output beyond the `Finished` line.

---

## Task 2: Live-enrollment resolvers and the `alert_count_*` environment bug fix

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (`alert_count_errors` at 7270, `alert_count_events` at 7305; new fns beside them)
- Modify `backend/bins/sauron-alerts/src/main.rs` (the four `alert_count_*` call sites in `evaluate_rule`)
- Modify `backend/crates/sauron-db/tests/notifications.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `pub async fn live_enrollments_for_apps(conn: &mut AsyncPgConnection, app_ids: &[Uuid]) -> QueryResult<Vec<(Uuid, Uuid, Uuid)>>` — `(enrollment_id, app_id, catalogue_env_id)`
  - `pub async fn enrollment_ids_for_env_name(conn: &mut AsyncPgConnection, app_ids: &[Uuid], name: &str) -> QueryResult<Vec<Uuid>>`
  - `alert_count_errors(conn, app_ids, from, to, level: Option<&str>, env_ids: Option<&[Uuid]>, tag: Option<&Value>) -> QueryResult<i64>`
  - `alert_count_events(conn, app_ids, from, to, name: Option<&str>, env_ids: Option<&[Uuid]>, tag: Option<&Value>) -> QueryResult<i64>`
  - `pub async fn alert_count_errors_by_app(conn, app_ids, from, to, level: Option<&str>, env_ids: Option<&[Uuid]>) -> QueryResult<Vec<(Uuid, i64)>>`

- [ ] **Step 1: Write the failing regression test.** Append to `backend/crates/sauron-db/tests/notifications.rs`:

```rust
/// The live bug this slice fixes. Since migration 33, `environments` is the
/// project-level catalogue and `error_events.environment_id` holds an
/// `app_environments` ENROLLMENT id, so `alert_count_errors`'s old subquery
/// (`environment_id IN (SELECT id FROM environments WHERE name = $5)`) compared
/// two disjoint id spaces and was always false: every environment-filtered
/// alert rule in the product counted zero and had never fired.
#[tokio::test]
async fn alert_count_errors_narrows_by_enrollment_id_not_catalogue_id() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let from = chrono::Utc::now() - chrono::Duration::days(2);
    let to = chrono::Utc::now() + chrono::Duration::days(1);

    let all = sauron_db::repo::alert_count_errors(
        &mut conn, &[ids.app_id], from, to, None, None, None,
    )
    .await
    .expect("unfiltered count");
    assert_eq!(all, 7, "seed_two_envs inserts 7 error_events");

    let enrollments =
        sauron_db::repo::enrollment_ids_for_env_name(&mut conn, &[ids.app_id], "env_a")
            .await
            .expect("resolve env_a");
    assert_eq!(enrollments, vec![ids.env_a], "env_a resolves to its enrollment id");

    let narrowed = sauron_db::repo::alert_count_errors(
        &mut conn, &[ids.app_id], from, to, None, Some(&enrollments), None,
    )
    .await
    .expect("narrowed count");
    assert_eq!(narrowed, 4, "env_a holds 4 of the 7 error_events");

    // The old shape, spelled out, so the regression is pinned rather than
    // described: a CATALOGUE id can never equal an enrollment id.
    let catalogue: Vec<uuid::Uuid> =
        sauron_db::repo::live_enrollments_for_apps(&mut conn, &[ids.app_id])
            .await
            .expect("live enrollments")
            .into_iter()
            .filter(|(enrollment, _, _)| *enrollment == ids.env_a)
            .map(|(_, _, catalogue_env)| catalogue_env)
            .collect();
    assert_eq!(catalogue.len(), 1);
    let wrong = sauron_db::repo::alert_count_errors(
        &mut conn, &[ids.app_id], from, to, None, Some(&catalogue), None,
    )
    .await
    .expect("catalogue-id count");
    assert_eq!(wrong, 0, "catalogue ids match nothing — that WAS the bug");

    db.cleanup().await;
}

/// Both `retired_at IS NULL` filters are load-bearing and only a DB test can
/// prove it: `(app_id, name)` is unique only among LIVE rows, so retiring
/// `staging` and creating a fresh `staging` leaves two rows with that name.
#[tokio::test]
async fn enrollment_ids_for_env_name_ignores_retired_rows() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let live = common::seed_env(
        &mut conn, ids.project_id, ids.app_id, "staging", "pk-staging-live", false,
    )
    .await;

    let found = sauron_db::repo::enrollment_ids_for_env_name(&mut conn, &[ids.app_id], "staging")
        .await
        .expect("resolve staging");
    assert_eq!(found, vec![live]);

    diesel::sql_query("UPDATE app_environments SET retired_at = now() WHERE id = $1")
        .bind::<diesel::sql_types::Uuid, _>(live)
        .execute(&mut conn)
        .await
        .expect("retire the enrollment");

    let found = sauron_db::repo::enrollment_ids_for_env_name(&mut conn, &[ids.app_id], "staging")
        .await
        .expect("resolve staging after retirement");
    assert!(found.is_empty(), "a retired enrollment must contribute nothing");

    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications`
  Expected: `error[E0425]: cannot find function 'enrollment_ids_for_env_name' in module 'sauron_db::repo'` and `this function takes 7 arguments but ...` mismatches on `alert_count_errors`.

- [ ] **Step 3: Add the two resolvers.** In `backend/crates/sauron-db/src/repo.rs`, immediately **above** `pub async fn alert_count_errors` (line 7270), insert:

```rust
/// `(enrollment_id, app_id, catalogue_environment_id)` for every LIVE
/// enrollment of `app_ids`.
///
/// This is one of exactly two sanctioned bridges between the two environment
/// id spaces. A subscription stores CATALOGUE ids (they are the wildcard RBAC
/// lacks, and stay correct when a new app is auto-enrolled); everything
/// downstream — event rows, `role_grants.scope_id`, `Reach.envs` — is
/// ENROLLMENT ids. Mixing them produces a filter that matches nothing, and the
/// failure is silent at every layer.
pub async fn live_enrollments_for_apps(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid, Uuid)>> {
    if app_ids.is_empty() {
        return Ok(Vec::new());
    }
    app_environments::table
        .filter(app_environments::app_id.eq_any(app_ids.to_vec()))
        .filter(app_environments::retired_at.is_null())
        .select((
            app_environments::id,
            app_environments::app_id,
            app_environments::environment_id,
        ))
        .load(conn)
        .await
}

/// The ENROLLMENT ids of the live environment named `name` across `app_ids`.
///
/// `retired_at IS NULL` is load-bearing on BOTH tables: `(app_id, name)` is
/// only unique among LIVE environments, so retiring `staging` and creating a
/// fresh `staging` leaves two rows with that name. Without these filters the
/// resolver returns both ids and the count silently includes the retired
/// environment's events too. The partial unique index guarantees at most one
/// live match per name, so this is deterministic.
pub async fn enrollment_ids_for_env_name(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    name: &str,
) -> QueryResult<Vec<Uuid>> {
    if app_ids.is_empty() {
        return Ok(Vec::new());
    }
    app_environments::table
        .inner_join(environments::table.on(environments::id.eq(app_environments::environment_id)))
        .filter(app_environments::app_id.eq_any(app_ids.to_vec()))
        .filter(app_environments::retired_at.is_null())
        .filter(environments::retired_at.is_null())
        .filter(environments::name.eq(name))
        .select(app_environments::id)
        .load(conn)
        .await
}
```

- [ ] **Step 4: Rewrite `alert_count_errors`.** Replace the whole existing `pub async fn alert_count_errors` body (repo.rs 7268-7303, including the `retired_at` doc comment that now lives on `enrollment_ids_for_env_name`) with:

```rust
/// Count error events across `app_ids` in `(from, to]`, with optional
/// level/environment/tag filters. All values are bound parameters.
///
/// `env_ids` are **enrollment** ids (`app_environments.id`), because that is
/// what `error_events.environment_id` holds. Callers that start from an
/// environment *name* resolve it through [`enrollment_ids_for_env_name`]
/// first. `Some(&[])` short-circuits to zero explicitly rather than by
/// accident through an empty `ANY()`.
pub async fn alert_count_errors(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    env_ids: Option<&[Uuid]>,
    tag: Option<&Value>,
) -> QueryResult<i64> {
    if env_ids.is_some_and(|e| e.is_empty()) {
        return Ok(0);
    }
    let row: AlertCountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM error_events \
         WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
           AND ($4::text IS NULL OR level = $4) \
           AND ($5::uuid[] IS NULL OR environment_id = ANY($5)) \
           AND ($6::jsonb IS NULL OR tags @> $6)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<Nullable<diesel::sql_types::Array<SqlUuid>>, _>(env_ids.map(|e| e.to_vec()))
    .bind::<Nullable<Jsonb>, _>(tag)
    .get_result(conn)
    .await?;
    Ok(row.n)
}
```

- [ ] **Step 5: Rewrite `alert_count_events` the same way.** Replace the whole existing `pub async fn alert_count_events` (repo.rs 7303-7339) with:

```rust
/// Count analytics events across `app_ids` in `(from, to]`, with optional
/// name/environment/tag filters. `env_ids` are **enrollment** ids; see
/// [`alert_count_errors`].
pub async fn alert_count_events(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    name: Option<&str>,
    env_ids: Option<&[Uuid]>,
    tag: Option<&Value>,
) -> QueryResult<i64> {
    if env_ids.is_some_and(|e| e.is_empty()) {
        return Ok(0);
    }
    let row: AlertCountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM analytics_events \
         WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
           AND ($4::text IS NULL OR name = $4) \
           AND ($5::uuid[] IS NULL OR environment_id = ANY($5)) \
           AND ($6::jsonb IS NULL OR tags @> $6)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(name)
    .bind::<Nullable<diesel::sql_types::Array<SqlUuid>>, _>(env_ids.map(|e| e.to_vec()))
    .bind::<Nullable<Jsonb>, _>(tag)
    .get_result(conn)
    .await?;
    Ok(row.n)
}
```

- [ ] **Step 6: Add the grouped variant.** Immediately after `alert_count_events`, insert:

```rust
#[derive(Debug, QueryableByName)]
pub struct AlertAppCountRow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = BigInt)]
    pub n: i64,
}

/// Per-app error counts over one window — the grouped form the personal
/// subscription evaluator needs.
///
/// A probe deliberately spans every app of every subscription that shares its
/// condition bucket (keying on a single app id would turn one query over a
/// 200-app project into 200), so the result has to come back attributed by
/// app id. Fanning out positionally instead would let a key-collision bug
/// attribute one app's counts to another user's subscription — a telemetry
/// leak inside an email.
pub async fn alert_count_errors_by_app(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    env_ids: Option<&[Uuid]>,
) -> QueryResult<Vec<(Uuid, i64)>> {
    if app_ids.is_empty() || env_ids.is_some_and(|e| e.is_empty()) {
        return Ok(Vec::new());
    }
    let rows: Vec<AlertAppCountRow> = diesel::sql_query(
        "SELECT app_id, count(*) AS n FROM error_events \
         WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
           AND ($4::text IS NULL OR level = $4) \
           AND ($5::uuid[] IS NULL OR environment_id = ANY($5)) \
         GROUP BY app_id",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<Nullable<diesel::sql_types::Array<SqlUuid>>, _>(env_ids.map(|e| e.to_vec()))
    .load(conn)
    .await?;
    Ok(rows.into_iter().map(|r| (r.app_id, r.n)).collect())
}
```

- [ ] **Step 7: Move name resolution into the legacy caller.** In `backend/bins/sauron-alerts/src/main.rs`, inside `evaluate_rule`, insert immediately after the `let tag = match (…) { … };` block:

```rust
    // The admin-facing input is an environment NAME, which is the right thing
    // to type into a rule dialog — but `error_events.environment_id` holds an
    // `app_environments` ENROLLMENT id, and before this the count compared it
    // against the project-level catalogue, so every environment-filtered rule
    // in the product had been counting zero since migration 33. Resolve here,
    // once, and pass ids down. A misspelled name resolves to an empty set and
    // keeps counting zero — now deliberately, and visibly, rather than by
    // accident.
    let env_ids: Option<Vec<uuid::Uuid>> = match cond.filters.environment.as_deref() {
        Some(name) => {
            Some(repo::enrollment_ids_for_env_name(&mut conn, &app_ids, name).await?)
        }
        None => None,
    };
    let env_ids_ref = env_ids.as_deref();
```

- [ ] **Step 8: Update the four `alert_count_*` call sites.** In the same function, replace every occurrence of the argument `cond.filters.environment.as_deref(),` inside an `alert_count_errors(` or `alert_count_events(` call with `env_ids_ref,`. There are exactly four: one in `TriggerType::ErrorThreshold`, two in `TriggerType::ErrorSpike` (`current` and `previous`), one in `TriggerType::EventThreshold`.

- [ ] **Step 9: Run the DB tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications`
  Expected: `alert_count_errors_narrows_by_enrollment_id_not_catalogue_id ... ok` and `enrollment_ids_for_env_name_ignores_retired_rows ... ok`.

- [ ] **Step 10: Check the workspace, clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 3: Environment-narrowed issue queries

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (`alert_new_issues` at 7410, `alert_regressed_issues` at 7440)
- Modify `backend/bins/sauron-alerts/src/main.rs` (the two call sites in `TriggerType::IssueNew | TriggerType::IssueRegression`)
- Modify `backend/crates/sauron-db/tests/notifications.rs`

**Interfaces:**
- Consumes: `AlertIssueBrief` (existing, repo.rs:7396 — fields `id`, `app_id`, `title`, `level`, `times_seen`).
- Produces:
  - `alert_new_issues(conn, app_ids, from, to, level: Option<&str>, limit: i64) -> QueryResult<Vec<AlertIssueBrief>>`
  - `alert_regressed_issues(conn, app_ids, from, to, level: Option<&str>, limit: i64) -> QueryResult<Vec<AlertIssueBrief>>`
  - `pub async fn alert_new_issues_env(conn, app_ids, from, to, level: Option<&str>, env_ids: &[Uuid], limit: i64) -> QueryResult<Vec<AlertIssueBrief>>`
  - `pub async fn alert_regressed_issues_env(conn, app_ids, from, to, level: Option<&str>, env_ids: &[Uuid], limit: i64) -> QueryResult<Vec<AlertIssueBrief>>`

- [ ] **Step 1: Write the failing test.** Append to `backend/crates/sauron-db/tests/notifications.rs`:

```rust
/// `issues` has no `environment_id`, so narrowing must go through
/// `error_events`. Bounding that EXISTS by the tick window would mix two
/// clocks — the window comes from the server-clock watermark while
/// `occurred_at` is SDK-supplied — and a backdated batch would create an issue
/// whose `created_at` is inside the window while every one of its events sits
/// outside it. So the EXISTS is bounded by the issue's OWN ingest-side
/// timestamps instead.
#[tokio::test]
async fn issue_env_narrowing_uses_the_issues_own_timestamps() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let from = chrono::Utc::now() - chrono::Duration::days(2);
    let to = chrono::Utc::now() + chrono::Duration::days(1);

    let all = sauron_db::repo::alert_new_issues(&mut conn, &[ids.app_id], from, to, None, 21)
        .await
        .expect("unfiltered new issues");
    assert_eq!(all.len(), 2, "seed_two_envs creates issue_id and issue_env_b_only");

    // `issue_env_b_only`'s single error event lives in env_b alone, so an
    // env_a-narrowed probe must not see it.
    let only_a = sauron_db::repo::alert_new_issues_env(
        &mut conn, &[ids.app_id], from, to, None, &[ids.env_a], 21,
    )
    .await
    .expect("env_a new issues");
    let a_ids: Vec<uuid::Uuid> = only_a.iter().map(|i| i.id).collect();
    assert!(a_ids.contains(&ids.issue_id));
    assert!(
        !a_ids.contains(&ids.issue_env_b_only),
        "an issue with no events in env_a must not appear under an env_a filter"
    );

    // The limit is the truncation sentinel: ask for 2 and get exactly 2 back.
    let capped =
        sauron_db::repo::alert_new_issues(&mut conn, &[ids.app_id], from, to, None, 1)
            .await
            .expect("limited new issues");
    assert_eq!(capped.len(), 1, "LIMIT is a bound parameter, not a literal 20");

    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications issue_env_narrowing`
  Expected: `error[E0061]: this function takes 5 arguments but 6 arguments were supplied` on `alert_new_issues`, plus `cannot find function alert_new_issues_env`.

- [ ] **Step 3: Add `limit` to the two shipped fns.** In `backend/crates/sauron-db/src/repo.rs`, change `alert_new_issues`'s signature to end `level: Option<&str>, limit: i64,`, change its SQL tail from `ORDER BY created_at DESC LIMIT 20` to `ORDER BY created_at DESC LIMIT $5`, and append `.bind::<BigInt, _>(limit.clamp(1, 201))` after the `level` bind. Do exactly the same for `alert_regressed_issues` (its tail is `ORDER BY last_event_at DESC LIMIT 20`). Keep both existing doc comments verbatim — they record why `created_at` and `last_event_at` are the right clocks — and append to each:

```rust
    /// `limit` is a bound parameter rather than a literal 20 because a personal
    /// subscription's probe spans several apps, and a fixed 20 lets one noisy
    /// app starve the rest. Callers pass `n + 1` and treat the extra row as a
    /// truncation sentinel.
```

- [ ] **Step 4: Add the two env-narrowed variants.** Immediately after `alert_regressed_issues`, insert:

```rust
/// [`alert_new_issues`], narrowed to a set of **enrollment** environment ids.
///
/// The EXISTS is bounded by the issue's own `first_seen`/`last_event_at`, NOT
/// by the caller's tick window. Those are two different clocks: the window
/// comes from the server-clock watermark, while `error_events.occurred_at` is
/// SDK-supplied. A backdated or offline batch creates an issue whose
/// `created_at` is inside the window while every one of its events sits
/// outside it — the window-bounded form returns false, the subscription never
/// fires, and nothing is logged. The `- interval '1 hour'` absorbs client clock
/// skew in the direction that matters. Served by
/// `error_events_issue_env_time_idx (issue_id, environment_id, occurred_at DESC)`
/// from migration 31, and the `occurred_at` bounds still prune partitions.
pub async fn alert_new_issues_env(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    env_ids: &[Uuid],
    limit: i64,
) -> QueryResult<Vec<AlertIssueBrief>> {
    if env_ids.is_empty() {
        return Ok(Vec::new());
    }
    diesel::sql_query(
        "SELECT i.id, i.app_id, i.title, i.level, i.times_seen FROM issues i \
         WHERE i.app_id = ANY($1) AND i.created_at > $2 AND i.created_at <= $3 \
           AND ($4::text IS NULL OR i.level = $4) \
           AND EXISTS ( \
                 SELECT 1 FROM error_events e \
                  WHERE e.issue_id = i.id \
                    AND e.environment_id = ANY($5) \
                    AND e.occurred_at >  i.first_seen - interval '1 hour' \
                    AND e.occurred_at <= i.last_event_at \
           ) \
         ORDER BY i.created_at DESC LIMIT $6",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(env_ids)
    .bind::<BigInt, _>(limit.clamp(1, 201))
    .load(conn)
    .await
}

/// [`alert_regressed_issues`], narrowed to a set of **enrollment** environment
/// ids. See [`alert_new_issues_env`] for why the EXISTS uses the issue's own
/// timestamps rather than the tick window.
pub async fn alert_regressed_issues_env(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    env_ids: &[Uuid],
    limit: i64,
) -> QueryResult<Vec<AlertIssueBrief>> {
    if env_ids.is_empty() {
        return Ok(Vec::new());
    }
    diesel::sql_query(
        "SELECT i.id, i.app_id, i.title, i.level, i.times_seen FROM issues i \
         WHERE i.app_id = ANY($1) AND i.status IN ('resolved','ignored') \
           AND i.last_event_at > $2 AND i.last_event_at <= $3 \
           AND ($4::text IS NULL OR i.level = $4) \
           AND EXISTS ( \
                 SELECT 1 FROM error_events e \
                  WHERE e.issue_id = i.id \
                    AND e.environment_id = ANY($5) \
                    AND e.occurred_at >  i.first_seen - interval '1 hour' \
                    AND e.occurred_at <= i.last_event_at \
           ) \
         ORDER BY i.last_event_at DESC LIMIT $6",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(env_ids)
    .bind::<BigInt, _>(limit.clamp(1, 201))
    .load(conn)
    .await
}
```

- [ ] **Step 5: Update the legacy call sites.** In `backend/bins/sauron-alerts/src/main.rs`, add `20,` as the final argument to both `repo::alert_new_issues(` and `repo::alert_regressed_issues(` inside the `TriggerType::IssueNew | TriggerType::IssueRegression` arm, preserving the shipped bound.

- [ ] **Step 6: Run the test and watch it pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications issue_env_narrowing`
  Expected: `test issue_env_narrowing_uses_the_issues_own_timestamps ... ok`.

- [ ] **Step 7: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 4: `subscription.rs` — kinds, conditions, the spike predicate, quiet hours

**Files:**
- Create `backend/crates/sauron-alerts/src/subscription.rs`
- Modify `backend/crates/sauron-alerts/src/lib.rs` (module list at lines 8-16, `pub mod` block at 21-27)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum SubKind { Uptime, ErrorSpike, ErrorNewIssue, ErrorRegression }` with `parse(&str) -> Option<SubKind>`, `as_str(self) -> &'static str`, `const ALL: [SubKind; 4]`, `allows_app_scope(self) -> bool`, `supports_env_filter(self) -> bool`, `permission(self) -> &'static str`
  - `pub struct SubConditions { pub window_seconds: u32, pub factor: f64, pub min_count: i64, pub level: Option<String> }` with `from_value(SubKind, &serde_json::Value) -> SubConditions` and `to_value(&self, SubKind) -> serde_json::Value`
  - `pub fn spike_fires(current: i64, baseline: i64, min_count: i64, factor: f64) -> bool`
  - `pub fn in_quiet_hours(local_minute: i32, start: i32, end: i32) -> bool`

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-alerts/src/subscription.rs` containing **only** the test module:

```rust
//! Personal notification subscriptions: the entire pure decision surface.
//!
//! No diesel, no axum, no network. CI runs `cargo test --workspace` with no
//! Postgres service, so everything that can be decided without a database is
//! decided here and unit-tested unconditionally — the same split `guard.rs`
//! already uses.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spike_fires_on_zero_to_flood() {
        // The shipped org-engine predicate is `previous > 0 && …`, which makes
        // an app that was silent and is now on fire the ONE case that can never
        // fire. That is the case this whole kind exists for.
        assert!(spike_fires(10, 0, 10, 3.0));
        assert!(!spike_fires(9, 0, 10, 3.0), "the floor still applies at B = 0");
    }

    #[test]
    fn spike_needs_an_absolute_floor_as_well_as_a_ratio() {
        // 1 -> 3 is a 3x spike and would page someone at 04:00 without a floor.
        assert!(!spike_fires(3, 1, 10, 3.0));
        assert!(spike_fires(30, 10, 10, 3.0));
        assert!(!spike_fires(29, 10, 10, 3.0));
    }

    #[test]
    fn conditions_clamp_to_their_documented_bounds() {
        let v = serde_json::json!({
            "window_seconds": 5, "factor": 900.0, "min_count": 0, "level": "warning"
        });
        let c = SubConditions::from_value(SubKind::ErrorSpike, &v);
        assert_eq!(c.window_seconds, 300);
        assert_eq!(c.factor, 100.0);
        assert_eq!(c.min_count, 1);
        assert_eq!(c.level.as_deref(), Some("warning"));

        let v = serde_json::json!({ "window_seconds": 999_999, "factor": 0.1, "min_count": 9_999_999 });
        let c = SubConditions::from_value(SubKind::ErrorSpike, &v);
        assert_eq!(c.window_seconds, 86_400);
        assert_eq!(c.factor, 1.5);
        assert_eq!(c.min_count, 100_000);
    }

    #[test]
    fn a_non_finite_factor_never_survives_parsing() {
        // A NaN factor would poison a BTreeMap key ordering; an infinite one
        // would make every comparison false. Both are rejected before either
        // can happen.
        let v = serde_json::json!({ "factor": f64::NAN });
        assert_eq!(SubConditions::from_value(SubKind::ErrorSpike, &v).factor, 3.0);
        let v = serde_json::json!({ "factor": "not a number" });
        assert_eq!(SubConditions::from_value(SubKind::ErrorSpike, &v).factor, 3.0);
    }

    #[test]
    fn issue_kinds_default_to_error_level() {
        let empty = serde_json::json!({});
        assert_eq!(
            SubConditions::from_value(SubKind::ErrorNewIssue, &empty).level.as_deref(),
            Some("error")
        );
        assert_eq!(
            SubConditions::from_value(SubKind::ErrorRegression, &empty).level.as_deref(),
            Some("error")
        );
        assert_eq!(
            SubConditions::from_value(SubKind::ErrorSpike, &empty).level.as_deref(),
            None
        );
    }

    #[test]
    fn quiet_hours_wrap_around_midnight() {
        // 22:00 -> 06:00
        let (start, end) = (22 * 60, 6 * 60);
        assert!(in_quiet_hours(23 * 60, start, end));
        assert!(in_quiet_hours(3 * 60, start, end));
        assert!(!in_quiet_hours(7 * 60, start, end));
        assert!(!in_quiet_hours(21 * 60 + 59, start, end));
        assert!(in_quiet_hours(start, start, end), "the start minute is inside");
        assert!(!in_quiet_hours(end, start, end), "the end minute is outside");
    }

    #[test]
    fn quiet_hours_same_day_window() {
        let (start, end) = (60, 5 * 60); // 01:00 -> 05:00
        assert!(in_quiet_hours(2 * 60, start, end));
        assert!(!in_quiet_hours(6 * 60, start, end));
        assert!(!in_quiet_hours(0, start, end));
    }

    #[test]
    fn quiet_hours_with_equal_bounds_is_never_quiet() {
        // A zero-width window must not silence everything forever.
        assert!(!in_quiet_hours(0, 300, 300));
        assert!(!in_quiet_hours(300, 300, 300));
        assert!(!in_quiet_hours(1439, 300, 300));
    }

    #[test]
    fn uptime_refuses_app_scope_and_the_environment_filter() {
        assert!(!SubKind::Uptime.allows_app_scope());
        assert!(!SubKind::Uptime.supports_env_filter());
        for k in [SubKind::ErrorSpike, SubKind::ErrorNewIssue, SubKind::ErrorRegression] {
            assert!(k.allows_app_scope());
            assert!(k.supports_env_filter());
        }
    }

    #[test]
    fn every_kind_round_trips_through_its_wire_string() {
        for k in SubKind::ALL {
            assert_eq!(SubKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(SubKind::parse("event_threshold"), None);
    }
}
```

- [ ] **Step 2: Register the module.** In `backend/crates/sauron-alerts/src/lib.rs`, add `pub mod subscription;` after `pub mod rule;` (line 27), and add to the module map doc block after the `- [`rule`]` line:

```rust
//! - [`subscription`] — personal subscriptions: kinds, conditions, probe
//!   coalescing, quiet hours, and the delivery-time coverage predicate.
```

- [ ] **Step 3: Run the tests and watch them fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts subscription`
  Expected: `error[E0433]: failed to resolve: use of undeclared type 'SubKind'` (and the same for `SubConditions`, `spike_fires`, `in_quiet_hours`).

- [ ] **Step 4: Implement `SubKind`.** Insert into `subscription.rs` between the module doc comment and `#[cfg(test)]`:

```rust
use serde_json::Value;

/// What a personal subscription notifies on. Shaped like
/// [`crate::rule::TriggerType`], deliberately: same `parse`/`as_str`/`ALL`
/// surface so the two enums read the same way at call sites.
///
/// There is no `event_threshold` and no `perf_degradation` here. Analytics
/// volume and latency percentiles are team dashboards, not personal inboxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubKind {
    /// A monitor transitioned. Project scope only.
    Uptime,
    /// Error volume in a window jumped relative to the previous window.
    ErrorSpike,
    /// A brand-new issue was first seen.
    ErrorNewIssue,
    /// A resolved/ignored issue started erroring again.
    ErrorRegression,
}

impl SubKind {
    pub fn parse(s: &str) -> Option<SubKind> {
        Some(match s {
            "uptime" => SubKind::Uptime,
            "error_spike" => SubKind::ErrorSpike,
            "error_new_issue" => SubKind::ErrorNewIssue,
            "error_regression" => SubKind::ErrorRegression,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SubKind::Uptime => "uptime",
            SubKind::ErrorSpike => "error_spike",
            SubKind::ErrorNewIssue => "error_new_issue",
            SubKind::ErrorRegression => "error_regression",
        }
    }

    /// `monitors` carries only `project_id` — no `app_id`, no
    /// `environment_id` — so there is nothing below project for an uptime
    /// subscription to narrow on. Accepting an app-scoped uptime subscription
    /// that can never fire is worse than refusing it.
    pub fn allows_app_scope(self) -> bool {
        !matches!(self, SubKind::Uptime)
    }

    /// Same reason as [`Self::allows_app_scope`]: an uptime subscription's
    /// environment set is meaningless, so the dialog says so and the evaluator
    /// ignores it.
    pub fn supports_env_filter(self) -> bool {
        !matches!(self, SubKind::Uptime)
    }

    /// The permission a subscriber must hold over the scope.
    ///
    /// No new permission is minted for subscriptions: a subscription delivers
    /// only telemetry the user can already read, so it confers nothing.
    /// Gating on `alert:read` would be wrong — Viewer lacks it entirely.
    ///
    /// String literals here are a TEMPORARY placeholder: `sauron-alerts` does
    /// not depend on `sauron-auth` until Task 6 Step 1 adds it. **Task 6 Step 2
    /// replaces both arms with `sauron_auth::rbac::perm::MONITOR_READ` /
    /// `perm::ISSUE_READ`** — leaving them as literals means a rename of those
    /// constants silently stops matching any grant and every subscription
    /// resolves to an empty `Reach`, i.e. nobody is ever mailed and nothing
    /// fails loudly. Do not skip that step.
    pub fn permission(self) -> &'static str {
        match self {
            SubKind::Uptime => "monitor:read",
            _ => "issue:read",
        }
    }

    pub const ALL: [SubKind; 4] = [
        SubKind::Uptime,
        SubKind::ErrorSpike,
        SubKind::ErrorNewIssue,
        SubKind::ErrorRegression,
    ];
}
```

- [ ] **Step 5: Implement `SubConditions`.** Append below `impl SubKind`:

```rust
/// A subscription's `conditions` bag, parsed and clamped.
///
/// Every field is clamped at parse time rather than trusted, because a
/// subscription is created by any authenticated user — `POST /v1/auth/register`
/// is open and every registrant becomes an org Owner — and an unclamped window
/// or factor is both a cost lever and a coalescing-defeat vector.
#[derive(Debug, Clone, PartialEq)]
pub struct SubConditions {
    pub window_seconds: u32,
    pub factor: f64,
    pub min_count: i64,
    pub level: Option<String>,
}

impl SubConditions {
    pub const DEFAULT_WINDOW_SECONDS: u32 = 900;
    pub const MIN_WINDOW_SECONDS: u32 = 300;
    pub const MAX_WINDOW_SECONDS: u32 = 86_400;
    pub const DEFAULT_FACTOR: f64 = 3.0;
    pub const MIN_FACTOR: f64 = 1.5;
    pub const MAX_FACTOR: f64 = 100.0;
    pub const DEFAULT_MIN_COUNT: i64 = 10;
    pub const MIN_MIN_COUNT: i64 = 1;
    pub const MAX_MIN_COUNT: i64 = 100_000;

    pub fn from_value(kind: SubKind, v: &Value) -> SubConditions {
        let window_seconds = v
            .get("window_seconds")
            .and_then(Value::as_u64)
            .map(|n| n.min(u32::MAX as u64) as u32)
            .unwrap_or(Self::DEFAULT_WINDOW_SECONDS)
            .clamp(Self::MIN_WINDOW_SECONDS, Self::MAX_WINDOW_SECONDS);

        // A NaN would poison the `BTreeMap<ProbeKey, _>` ordering that probe
        // coalescing depends on, and an infinity would make every ratio
        // comparison false. Neither is representable after this line.
        let factor = v
            .get("factor")
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite())
            .unwrap_or(Self::DEFAULT_FACTOR)
            .clamp(Self::MIN_FACTOR, Self::MAX_FACTOR);

        let min_count = v
            .get("min_count")
            .and_then(Value::as_i64)
            .unwrap_or(Self::DEFAULT_MIN_COUNT)
            .clamp(Self::MIN_MIN_COUNT, Self::MAX_MIN_COUNT);

        let level = match v.get("level") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Null) => None,
            // The issue kinds default to `error`; the spike kind counts every
            // level unless told otherwise.
            None => match kind {
                SubKind::ErrorNewIssue | SubKind::ErrorRegression => Some("error".to_string()),
                _ => None,
            },
            _ => None,
        };

        SubConditions {
            window_seconds,
            factor,
            min_count,
            level,
        }
    }

    /// The clamped bag, back on the wire — what the API stores after
    /// validation, so the dashboard renders the effective values rather than
    /// what was submitted.
    pub fn to_value(&self, kind: SubKind) -> Value {
        match kind {
            SubKind::Uptime => serde_json::json!({}),
            SubKind::ErrorSpike => serde_json::json!({
                "window_seconds": self.window_seconds,
                "factor": self.factor,
                "min_count": self.min_count,
                "level": self.level,
            }),
            _ => serde_json::json!({ "level": self.level }),
        }
    }
}

/// Fire when the current window carries real volume AND either the previous
/// window was empty or the jump is at least `factor`.
///
/// The `baseline == 0` disjunct is the whole point: the shipped org-engine
/// predicate guards on `previous > 0`, so the zero-to-flood case — an app that
/// was silent and is now on fire — is the one case it can never report.
/// `min_count` is equally deliberate: without a floor a 1 -> 3 movement is a 3x
/// spike and pages someone at 04:00.
pub fn spike_fires(current: i64, baseline: i64, min_count: i64, factor: f64) -> bool {
    current >= min_count && (baseline == 0 || current as f64 >= baseline as f64 * factor)
}

/// Whether `local_minute` (minute-of-day, 0..1439, in the subscription's own
/// zone) falls inside `[start, end)`, wrap-around aware.
///
/// The enqueue does not call this — `deliver_after` is computed entirely in SQL
/// because the workspace has no `chrono-tz` and nothing in Rust can produce a
/// subscription's local wall-clock time. This exists because it is the only
/// form a unit test can reach, and a DB test asserts the SQL and this function
/// agree over a shared table of cases.
pub fn in_quiet_hours(local_minute: i32, start: i32, end: i32) -> bool {
    if start == end {
        // A zero-width window must not silence everything forever.
        return false;
    }
    if start < end {
        local_minute >= start && local_minute < end
    } else {
        local_minute >= start || local_minute < end
    }
}
```

- [ ] **Step 6: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts subscription`
  Expected: 10 passing tests, 0 failed.

- [ ] **Step 7: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 5: `subscription.rs` — probe coalescing

**Files:**
- Modify `backend/crates/sauron-alerts/src/subscription.rs`

**Interfaces:**
- Consumes: `SubKind`, `SubConditions` (Task 4).
- Produces:
  - `pub struct CondBucket { pub window_seconds: u32, pub min_count: i64, pub level: Option<String>, pub factor_milli: u32 }` with `quantize(&SubConditions) -> CondBucket`
  - `pub struct ProbeKey { pub org_id: Uuid, pub kind: SubKind, pub cond: CondBucket, pub catalogue_envs: Vec<Uuid> }`
  - `pub struct SubInput { pub index: usize, pub org_id: Uuid, pub kind: SubKind, pub cond: SubConditions, pub catalogue_envs: Vec<Uuid>, pub app_ids: Vec<Uuid> }`
  - `pub struct Probe { pub key: ProbeKey, pub subs: Vec<usize>, pub app_ids: Vec<Uuid> }`
  - `pub fn coalesce(inputs: &[SubInput]) -> Vec<Probe>`

- [ ] **Step 1: Write the failing tests.** Append inside `mod tests` in `backend/crates/sauron-alerts/src/subscription.rs`:

```rust
    fn sub(index: usize, org: u128, kind: SubKind, factor: f64, envs: &[u128], apps: &[u128]) -> SubInput {
        SubInput {
            index,
            org_id: uuid::Uuid::from_u128(org),
            kind,
            cond: SubConditions {
                window_seconds: 900,
                factor,
                min_count: 10,
                level: None,
            },
            catalogue_envs: envs.iter().map(|n| uuid::Uuid::from_u128(*n)).collect(),
            app_ids: apps.iter().map(|n| uuid::Uuid::from_u128(*n)).collect(),
        }
    }

    #[test]
    fn quantization_collapses_float_noise_but_not_real_differences() {
        // `f64` is not `Ord`, so a raw factor cannot be a BTreeMap key at all;
        // and distinct float values would defeat coalescing entirely, which is
        // a cheap denial of service given that registration is open.
        let a = SubConditions { window_seconds: 900, factor: 3.0, min_count: 10, level: None };
        let b = SubConditions { window_seconds: 900, factor: 3.0000001, min_count: 10, level: None };
        let c = SubConditions { window_seconds: 900, factor: 3.5, min_count: 10, level: None };
        assert_eq!(CondBucket::quantize(&a), CondBucket::quantize(&b));
        assert_ne!(CondBucket::quantize(&a), CondBucket::quantize(&c));
        // Snapped to the nearest 0.25.
        let d = SubConditions { window_seconds: 900, factor: 3.13, min_count: 10, level: None };
        assert_eq!(CondBucket::quantize(&d).factor_milli, 3_250);
    }

    #[test]
    fn every_subscription_lands_in_exactly_one_probe() {
        let inputs = vec![
            sub(0, 1, SubKind::ErrorSpike, 3.0, &[], &[10, 11]),
            sub(1, 1, SubKind::ErrorSpike, 3.0, &[], &[11, 12]),
            sub(2, 1, SubKind::ErrorSpike, 3.5, &[], &[10]),
        ];
        let probes = coalesce(&inputs);
        let mut seen: Vec<usize> = probes.iter().flat_map(|p| p.subs.clone()).collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2]);
        assert_eq!(probes.len(), 2, "two distinct factor buckets");
    }

    #[test]
    fn a_probes_app_array_is_exactly_the_union_of_its_subscriptions_scopes() {
        let inputs = vec![
            sub(0, 1, SubKind::ErrorSpike, 3.0, &[], &[10, 11]),
            sub(1, 1, SubKind::ErrorSpike, 3.0, &[], &[11, 12]),
        ];
        let probes = coalesce(&inputs);
        assert_eq!(probes.len(), 1);
        let mut apps = probes[0].app_ids.clone();
        apps.sort_unstable();
        assert_eq!(
            apps,
            vec![
                uuid::Uuid::from_u128(10),
                uuid::Uuid::from_u128(11),
                uuid::Uuid::from_u128(12)
            ]
        );
    }

    #[test]
    fn a_probe_never_spans_two_organizations() {
        // `org_id` is in the key so a cross-tenant mix-up is structurally
        // impossible: no probe's app array can span organizations.
        let inputs = vec![
            sub(0, 1, SubKind::ErrorSpike, 3.0, &[], &[10]),
            sub(1, 2, SubKind::ErrorSpike, 3.0, &[], &[20]),
        ];
        let probes = coalesce(&inputs);
        assert_eq!(probes.len(), 2);
        for p in &probes {
            assert_eq!(p.subs.len(), 1);
        }
    }

    #[test]
    fn environment_sets_are_order_insensitive_but_membership_sensitive() {
        let inputs = vec![
            sub(0, 1, SubKind::ErrorSpike, 3.0, &[7, 8], &[10]),
            sub(1, 1, SubKind::ErrorSpike, 3.0, &[8, 7], &[11]),
            sub(2, 1, SubKind::ErrorSpike, 3.0, &[7], &[12]),
        ];
        let probes = coalesce(&inputs);
        assert_eq!(probes.len(), 2, "{{7,8}} coalesces with {{8,7}}, not with {{7}}");
    }

    #[test]
    fn probe_count_is_bounded_by_orgs_times_kinds_times_buckets_times_env_sets() {
        // 200 subscriptions, one org, one kind, all defaults: one probe. This
        // is the property the whole design exists for — cost is independent of
        // both user count and app count.
        let inputs: Vec<SubInput> = (0..200)
            .map(|i| sub(i, 1, SubKind::ErrorSpike, 3.0, &[], &[(i as u128) + 100]))
            .collect();
        let probes = coalesce(&inputs);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].subs.len(), 200);
        assert_eq!(probes[0].app_ids.len(), 200);
    }
```

- [ ] **Step 2: Run them and watch them fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts subscription`
  Expected: `error[E0433]: failed to resolve: use of undeclared type 'CondBucket'` and `SubInput`, `coalesce`.

- [ ] **Step 3: Implement the bucket and key.** Append to `subscription.rs` (above `#[cfg(test)]`), after adding `use std::collections::BTreeMap;` and `use uuid::Uuid;` to the imports at the top:

```rust
/// A [`SubConditions`] quantized into something that can be a map key.
///
/// `f64` is not `Ord`, so a raw factor cannot key a `BTreeMap` at all — and
/// even if it could, distinct float values would defeat coalescing entirely,
/// which is a cheap denial of service against an evaluator whose whole cost
/// model is "one probe per condition bucket".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CondBucket {
    pub window_seconds: u32,
    pub min_count: i64,
    pub level: Option<String>,
    /// The clamped factor snapped to the nearest 0.25, in thousandths.
    pub factor_milli: u32,
}

impl CondBucket {
    pub fn quantize(c: &SubConditions) -> CondBucket {
        let snapped = (c.factor * 4.0).round() / 4.0;
        CondBucket {
            window_seconds: c.window_seconds,
            min_count: c.min_count,
            level: c.level.clone(),
            factor_milli: (snapped * 1000.0).round() as u32,
        }
    }
}

/// What one database probe is keyed on.
///
/// Deliberately does NOT contain an app id. `alert_count_errors`,
/// `alert_new_issues` and `alert_regressed_issues` all take `app_ids: &[Uuid]`
/// and filter `app_id = ANY($1)`, so a rule over a 200-app project costs ONE
/// query today. Keying a probe on a single app would turn that into 200 —
/// worse than what already ships.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProbeKey {
    pub org_id: Uuid,
    pub kind: SubKind,
    pub cond: CondBucket,
    /// Sorted and deduped catalogue environment ids. Empty means "all
    /// environments, including unattributed rows".
    pub catalogue_envs: Vec<Uuid>,
}

/// One subscription, with its scope already resolved to app ids.
#[derive(Debug, Clone)]
pub struct SubInput {
    /// The caller's index into its own subscription vector. Probes carry these
    /// back so the caller never has to match on anything but position in a
    /// slice it owns.
    pub index: usize,
    pub org_id: Uuid,
    pub kind: SubKind,
    pub cond: SubConditions,
    /// Catalogue environment ids. Empty means all environments.
    pub catalogue_envs: Vec<Uuid>,
    /// The apps this subscription's scope resolves to.
    pub app_ids: Vec<Uuid>,
}

/// One database probe and the subscriptions it answers for.
#[derive(Debug, Clone)]
pub struct Probe {
    pub key: ProbeKey,
    /// `SubInput::index` values, in ascending order.
    pub subs: Vec<usize>,
    /// The union of the in-scope apps of every subscription in `subs`, sorted
    /// and deduped.
    pub app_ids: Vec<Uuid>,
}

/// Group subscriptions into the smallest set of probes that answers all of
/// them.
///
/// Cost is `O(orgs × kinds × distinct condition buckets × distinct env sets)`
/// — independent of both user count and app count, and never worse than the
/// existing org engine. Since almost every subscription uses defaults, this
/// collapses hard in practice.
pub fn coalesce(inputs: &[SubInput]) -> Vec<Probe> {
    let mut grouped: BTreeMap<ProbeKey, (Vec<usize>, Vec<Uuid>)> = BTreeMap::new();
    for s in inputs {
        let mut envs = s.catalogue_envs.clone();
        envs.sort_unstable();
        envs.dedup();
        let key = ProbeKey {
            org_id: s.org_id,
            kind: s.kind,
            cond: CondBucket::quantize(&s.cond),
            catalogue_envs: envs,
        };
        let entry = grouped.entry(key).or_default();
        entry.0.push(s.index);
        entry.1.extend_from_slice(&s.app_ids);
    }
    grouped
        .into_iter()
        .map(|(key, (mut subs, mut app_ids))| {
            subs.sort_unstable();
            app_ids.sort_unstable();
            app_ids.dedup();
            Probe { key, subs, app_ids }
        })
        .collect()
}
```

- [ ] **Step 4: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts subscription`
  Expected: 16 passing tests (10 from Task 4, 6 from this task).

- [ ] **Step 5: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 6: `subscription.rs` — the coverage predicate

**Files:**
- Modify `backend/crates/sauron-alerts/Cargo.toml`
- Modify `backend/crates/sauron-alerts/src/subscription.rs`

**Interfaces:**
- Consumes: `sauron_auth::rbac::Reach` (existing: `{ org: bool, projects: Vec<Uuid>, apps: Vec<Uuid>, envs: Vec<Uuid> }`).
- Produces:
  - `pub struct QueueTarget<'a> { pub project_id: Uuid, pub app_id: Option<Uuid>, pub env_enrollments: &'a [Uuid], pub includes_unattributed: bool }`
  - `pub fn covers(reach: &Reach, t: &QueueTarget<'_>) -> bool`

- [ ] **Step 1: Add the dependency.** In `backend/crates/sauron-alerts/Cargo.toml`, add `sauron-auth = { workspace = true }` immediately after the `sauron-monitor-core` line. (`sauron-auth` depends on `sauron-core` and `sauron-db` only, so there is no cycle.)

- [ ] **Step 2: Point `SubKind::permission` at the real constants.** Task 4 shipped it with string literals because the dependency did not exist yet. Now it does. In `backend/crates/sauron-alerts/src/subscription.rs`, replace the body of `permission` and delete the placeholder paragraph from its doc comment:

```rust
    /// The permission a subscriber must hold over the scope.
    ///
    /// No new permission is minted for subscriptions: a subscription delivers
    /// only telemetry the user can already read, so it confers nothing.
    /// Gating on `alert:read` would be wrong — Viewer lacks it entirely.
    ///
    /// Returned from `sauron_auth::rbac::perm` rather than as a literal: these
    /// strings are matched against stored grants, so a rename in `rbac.rs` that
    /// left a literal behind here would produce an empty `Reach` for every
    /// subscription — no mail, no error, nothing to notice.
    pub fn permission(self) -> &'static str {
        match self {
            SubKind::Uptime => sauron_auth::rbac::perm::MONITOR_READ,
            _ => sauron_auth::rbac::perm::ISSUE_READ,
        }
    }
```

  Add a test alongside it inside `mod tests` so the wiring is asserted, not assumed:

```rust
    #[test]
    fn permissions_come_from_the_rbac_constants() {
        assert_eq!(SubKind::Uptime.permission(), sauron_auth::rbac::perm::MONITOR_READ);
        assert_eq!(SubKind::ErrorSpike.permission(), sauron_auth::rbac::perm::ISSUE_READ);
        assert_eq!(SubKind::ErrorNewIssue.permission(), sauron_auth::rbac::perm::ISSUE_READ);
        assert_eq!(SubKind::ErrorRegression.permission(), sauron_auth::rbac::perm::ISSUE_READ);
    }
```

- [ ] **Step 3: Write the failing tests.** Append inside `mod tests`:

```rust
    use sauron_auth::rbac::Reach;

    fn reach(org: bool, projects: &[u128], apps: &[u128], envs: &[u128]) -> Reach {
        Reach {
            org,
            projects: projects.iter().map(|n| uuid::Uuid::from_u128(*n)).collect(),
            apps: apps.iter().map(|n| uuid::Uuid::from_u128(*n)).collect(),
            envs: envs.iter().map(|n| uuid::Uuid::from_u128(*n)).collect(),
        }
    }

    fn target(project: u128, app: Option<u128>, envs: &'static [uuid::Uuid], unattributed: bool)
        -> QueueTarget<'static>
    {
        QueueTarget {
            project_id: uuid::Uuid::from_u128(project),
            app_id: app.map(uuid::Uuid::from_u128),
            env_enrollments: envs,
            includes_unattributed: unattributed,
        }
    }

    #[test]
    fn org_reach_covers_everything() {
        assert!(covers(&reach(true, &[], &[], &[]), &target(1, Some(2), &[], true)));
        assert!(covers(&reach(true, &[], &[], &[]), &target(1, None, &[], false)));
    }

    #[test]
    fn a_project_grant_covers_its_apps_and_its_monitors() {
        let r = reach(false, &[1], &[], &[]);
        assert!(covers(&r, &target(1, Some(2), &[], true)));
        assert!(covers(&r, &target(1, None, &[], false)), "uptime needs project reach");
        assert!(!covers(&r, &target(9, Some(2), &[], true)));
    }

    #[test]
    fn uptime_is_refused_to_app_and_env_scoped_members() {
        // Every monitor read in the product is
        // `authorize_project(user, project, monitor:read)`, which resolves with
        // `app: None, env: None`, and `grant_applies` never lets a `Scope::App`
        // or `Scope::Env` grant satisfy that. An app-scoped member gets 403 from
        // every monitor endpoint — so mailing them monitor names, targets,
        // causes and incident ids would hand over exactly what the API refuses.
        assert!(!covers(&reach(false, &[], &[2], &[]), &target(1, None, &[], false)));
        assert!(!covers(&reach(false, &[], &[], &[3]), &target(1, None, &[], false)));
    }

    #[test]
    fn an_app_grant_covers_its_own_app_only() {
        let r = reach(false, &[], &[2], &[]);
        assert!(covers(&r, &target(1, Some(2), &[], true)));
        assert!(!covers(&r, &target(1, Some(99), &[], true)));
    }

    #[test]
    fn an_env_grant_needs_every_listed_enrollment() {
        const E3: uuid::Uuid = uuid::Uuid::from_u128(3);
        const E4: uuid::Uuid = uuid::Uuid::from_u128(4);
        static BOTH: [uuid::Uuid; 2] = [E3, E4];
        static ONE: [uuid::Uuid; 1] = [E3];
        static SIBLING: [uuid::Uuid; 1] = [E4];

        let holds_both = reach(false, &[], &[], &[3, 4]);
        let holds_one = reach(false, &[], &[], &[3]);

        assert!(covers(&holds_both, &target(1, Some(2), &BOTH, false)));
        assert!(covers(&holds_one, &target(1, Some(2), &ONE, false)));
        assert!(
            !covers(&holds_one, &target(1, Some(2), &BOTH, false)),
            "partial coverage of the listed enrollments must be refused"
        );
        assert!(!covers(&holds_one, &target(1, Some(2), &SIBLING, false)));
    }

    #[test]
    fn an_empty_environment_list_is_never_read_as_unconstrained() {
        // A probe with no environment predicate counts across every enrollment
        // AND unattributed rows, so it needs app-level reach. Reading NULL as
        // "unconstrained" leaks; reading it as "nothing" starves an env-scoped
        // subscriber silently.
        let env_only = reach(false, &[], &[], &[3]);
        assert!(!covers(&env_only, &target(1, Some(2), &[], false)));
        assert!(!covers(&env_only, &target(1, Some(2), &[], true)));
    }

    #[test]
    fn includes_unattributed_is_refused_to_an_env_grant() {
        const E3: uuid::Uuid = uuid::Uuid::from_u128(3);
        static ONE: [uuid::Uuid; 1] = [E3];
        let env_only = reach(false, &[], &[], &[3]);
        assert!(covers(&env_only, &target(1, Some(2), &ONE, false)));
        assert!(
            !covers(&env_only, &target(1, Some(2), &ONE, true)),
            "unattributed rows belong to no single environment"
        );
    }
```

- [ ] **Step 4: Run them and watch them fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts subscription`
  Expected: `error[E0433]: failed to resolve: use of undeclared type 'QueueTarget'` and `cannot find function 'covers'`.

- [ ] **Step 5: Implement.** Append to `subscription.rs` above `#[cfg(test)]`, adding `use sauron_auth::rbac::Reach;` to the top-of-file imports:

```rust
/// What a queued notification is *about*, in the terms `covers` decides on.
pub struct QueueTarget<'a> {
    pub project_id: Uuid,
    /// `None` for uptime — `monitors` carries no app dimension.
    pub app_id: Option<Uuid>,
    /// **Enrollment** ids (`app_environments.id`). Empty means the body spans
    /// every environment of the app, including unattributed rows.
    pub env_enrollments: &'a [Uuid],
    pub includes_unattributed: bool,
}

/// Whether `reach` releases `t`'s content to its holder.
///
/// Callers MUST pass a `Reach` built from grants already filtered to a single
/// organization (as `repo::user_grants_in_org` does) — `reach_for`'s org arm is
/// `Scope::Org(_) => reach.org = true` and never compares the org id, so an
/// unfiltered grant list would leak another org's visibility.
pub fn covers(reach: &Reach, t: &QueueTarget<'_>) -> bool {
    if reach.org {
        return true;
    }
    if reach.projects.contains(&t.project_id) {
        return true;
    }
    let Some(app_id) = t.app_id else {
        // Uptime stops here. Every monitor read in the product resolves with
        // `app: None, env: None`, so an app- or env-scoped member gets 403 from
        // every monitor endpoint; authorizing an uptime notification with the
        // per-app coverage test below would mail them monitor names, targets,
        // causes and incident ids the API itself refuses them.
        return false;
    };
    if reach.apps.contains(&app_id) {
        return true;
    }
    // An env grant is released only when EVERY enrollment behind the body is
    // one the holder reaches. An empty list is never "unconstrained": it means
    // the probe counted across all environments and unattributed rows, which
    // needs app-level reach.
    !t.includes_unattributed
        && !t.env_enrollments.is_empty()
        && t.env_enrollments.iter().all(|e| reach.envs.contains(e))
}
```

- [ ] **Step 6: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts subscription`
  Expected: 24 passing tests (10 from Task 4, 6 from Task 5, 8 from this task).

- [ ] **Step 7: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 7: Unsubscribe tokens

**Files:**
- Modify `backend/crates/sauron-alerts/src/crypto.rs` (append after `hmac_sha256_hex`, which ends around line 110)

**Interfaces:**
- Consumes: `pub fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String` (existing, crypto.rs:85).
- Produces:
  - `pub const UNSUB_TOKEN_TTL_DAYS: i64 = 90;`
  - `pub fn derive_unsub_key(notify_key: &[u8]) -> String`
  - `pub fn unsubscribe_token(key: &[u8], sub_id: Uuid, user_id: Uuid, issued_day: i64) -> String`
  - `pub fn verify_unsubscribe_token(key: &[u8], token: &str, today: i64, owner_of: impl FnOnce(Uuid) -> Option<Uuid>) -> Option<Uuid>` — `today` is `days_since_epoch(Utc::now())` at the call site, threaded in so the 90-day expiry branch is testable without a clock; `owner_of` is a closure so this function stays free of `sauron-db` (see Step 3)
  - `pub fn days_since_epoch(now: DateTime<Utc>) -> i64`

- [ ] **Step 1: Write the failing tests.** Append inside `mod tests` in `backend/crates/sauron-alerts/src/crypto.rs`:

```rust
    #[test]
    fn unsubscribe_token_round_trips_for_its_own_subscription() {
        let key = derive_unsub_key(b"notify-secret");
        let sub = uuid::Uuid::from_u128(1);
        let user = uuid::Uuid::from_u128(2);
        let day = 20_000;
        let token = unsubscribe_token(key.as_bytes(), sub, user, day);
        assert_eq!(
            verify_unsubscribe_token(key.as_bytes(), &token, day, |id| {
                (id == sub).then_some(user)
            }),
            Some(sub)
        );
    }

    #[test]
    fn a_token_signed_with_another_key_never_verifies() {
        let a = derive_unsub_key(b"key-a");
        let b = derive_unsub_key(b"key-b");
        let sub = uuid::Uuid::from_u128(1);
        let user = uuid::Uuid::from_u128(2);
        let token = unsubscribe_token(a.as_bytes(), sub, user, 20_000);
        assert_eq!(
            verify_unsubscribe_token(b.as_bytes(), &token, 20_000, |_| Some(user)),
            None
        );
    }

    #[test]
    fn a_token_for_one_subscription_does_not_verify_against_another() {
        let key = derive_unsub_key(b"notify-secret");
        let user = uuid::Uuid::from_u128(2);
        let token = unsubscribe_token(
            key.as_bytes(),
            uuid::Uuid::from_u128(1),
            user,
            20_000,
        );
        // The stored token names subscription 1, but the row it points at is
        // owned by a different user — the HMAC covers the pair, so the swap is
        // detected rather than silently accepted.
        assert_eq!(
            verify_unsubscribe_token(key.as_bytes(), &token, 20_000, |_| Some(
                uuid::Uuid::from_u128(999)
            )),
            None
        );
    }

    #[test]
    fn tokens_expire_and_are_not_accepted_from_the_future() {
        let key = derive_unsub_key(b"notify-secret");
        let sub = uuid::Uuid::from_u128(1);
        let user = uuid::Uuid::from_u128(2);
        let old = unsubscribe_token(key.as_bytes(), sub, user, 20_000);
        assert_eq!(
            verify_unsubscribe_token(key.as_bytes(), &old, 20_000 + 91, |_| Some(user)),
            None,
            "91 days old is past UNSUB_TOKEN_TTL_DAYS"
        );
        assert_eq!(
            verify_unsubscribe_token(key.as_bytes(), &old, 20_000 + 90, |_| Some(user)),
            Some(sub),
            "exactly 90 days old still works"
        );
        let future = unsubscribe_token(key.as_bytes(), sub, user, 20_050);
        assert_eq!(
            verify_unsubscribe_token(key.as_bytes(), &future, 20_000, |_| Some(user)),
            None,
            "a token dated in the future is a forged issued_day"
        );
    }

    #[test]
    fn malformed_tokens_are_rejected_without_panicking() {
        let key = derive_unsub_key(b"notify-secret");
        for bad in ["", ".", "a.b", "a.b.c", "....", "zzz.20000.deadbeef", &"x".repeat(4096)] {
            assert_eq!(
                verify_unsubscribe_token(key.as_bytes(), bad, 20_000, |_| Some(
                    uuid::Uuid::from_u128(2)
                )),
                None
            );
        }
    }

    #[test]
    fn ct_eq_compares_the_whole_buffer_and_refuses_length_mismatches() {
        // The design requires the signature comparison to be constant-time, and
        // the only way that property can regress silently is someone replacing
        // the loop with `a == b`. These assertions pin the two observable
        // consequences: a length mismatch is refused without indexing past the
        // end, and a difference in the LAST byte is still caught (a
        // short-circuiting prefix compare would pass every earlier byte and is
        // exactly what leaks the length of a correct prefix to a timing
        // attacker).
        assert!(ct_eq(b"", b""));
        assert!(ct_eq(b"abcdef", b"abcdef"));
        assert!(!ct_eq(b"abcdef", b"abcde"), "a prefix is not a match");
        assert!(!ct_eq(b"abcde", b"abcdef"));
        assert!(!ct_eq(b"abcdef", b"abcdeg"), "the last byte still decides");
        assert!(!ct_eq(b"abcdef", b"zbcdef"), "the first byte still decides");
    }

    #[test]
    fn the_derived_key_is_domain_separated_from_the_notify_key() {
        // NOTIFY_SECRET_KEY is documented as the AES-GCM key that encrypts
        // stored channel secrets, so "rotate it to invalidate outstanding
        // links" is not available — rotating it makes every stored Slack
        // webhook URL and SMTP password undecryptable. Domain separation at
        // least keeps the two uses independent.
        let raw = b"notify-secret";
        assert_ne!(derive_unsub_key(raw), String::from_utf8_lossy(raw));
        assert_eq!(derive_unsub_key(raw).len(), 64, "hex sha256");
    }
```

- [ ] **Step 2: Run them and watch them fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts crypto`
  Expected: `error[E0425]: cannot find function 'derive_unsub_key' in this scope`.

- [ ] **Step 3: Implement.** Append to `backend/crates/sauron-alerts/src/crypto.rs` immediately after `hmac_sha256_hex` (adding `use chrono::{DateTime, Utc};` and `use uuid::Uuid;` to the file's imports):

```rust
/// How long an unsubscribe link stays valid.
///
/// A compile-time constant, not an env var: every send mints a fresh token so
/// links in live mail always work, and the only thing this bounds is a token
/// forwarded into an archive becoming a permanent silencer of someone else's
/// uptime alerts.
pub const UNSUB_TOKEN_TTL_DAYS: i64 = 90;

const UNSUB_KEY_DOMAIN: &[u8] = b"sauron-unsub-key-v1";
const UNSUB_MSG_PREFIX: &str = "sauron-unsub-v1";
/// Half of a SHA-256 in hex. Enough to make forgery infeasible without making
/// the URL unwieldy in a mail client that wraps long lines.
const UNSUB_SIG_HEX_LEN: usize = 32;

/// Days since the Unix epoch, the unit `issued_day` is measured in.
pub fn days_since_epoch(now: DateTime<Utc>) -> i64 {
    now.timestamp().div_euclid(86_400)
}

/// Derive the unsubscribe signing key from the notification secret.
///
/// Never sign with `notify_key` directly: it is the AES-GCM key that encrypts
/// stored channel secrets, so it cannot be rotated to invalidate outstanding
/// links without making every stored webhook URL and relay password
/// undecryptable.
pub fn derive_unsub_key(notify_key: &[u8]) -> String {
    hmac_sha256_hex(notify_key, UNSUB_KEY_DOMAIN)
}

/// `{subscription_id}.{issued_day}.{first 32 hex chars of the HMAC}`.
pub fn unsubscribe_token(key: &[u8], sub_id: Uuid, user_id: Uuid, issued_day: i64) -> String {
    let msg = format!("{UNSUB_MSG_PREFIX}:{sub_id}:{user_id}:{issued_day}");
    let sig = hmac_sha256_hex(key, msg.as_bytes());
    format!("{sub_id}.{issued_day}.{}", &sig[..UNSUB_SIG_HEX_LEN])
}

/// Verify a token and return the subscription it names.
///
/// `owner_of` resolves a subscription id to its owner's user id — the owner is
/// inside the signed message, so verification cannot be completed without it,
/// and passing it as a closure keeps this function free of `sauron-db`. It is
/// called at most once, after the token's shape has already been validated, so
/// a garbage token costs no database round trip.
///
/// `today` is `days_since_epoch(Utc::now())` at the call site, threaded in so
/// the expiry branch is testable without a clock.
pub fn verify_unsubscribe_token(
    key: &[u8],
    token: &str,
    today: i64,
    owner_of: impl FnOnce(Uuid) -> Option<Uuid>,
) -> Option<Uuid> {
    let mut parts = token.split('.');
    let sub_id: Uuid = parts.next()?.parse().ok()?;
    let issued_day: i64 = parts.next()?.parse().ok()?;
    let sig = parts.next()?;
    if parts.next().is_some() || sig.len() != UNSUB_SIG_HEX_LEN {
        return None;
    }
    // A future-dated token means a forged `issued_day`; an old one means a
    // link that has been sitting in an archive.
    if issued_day > today || today - issued_day > UNSUB_TOKEN_TTL_DAYS {
        return None;
    }
    let user_id = owner_of(sub_id)?;
    let msg = format!("{UNSUB_MSG_PREFIX}:{sub_id}:{user_id}:{issued_day}");
    let expected = hmac_sha256_hex(key, msg.as_bytes());
    ct_eq(sig.as_bytes(), expected[..UNSUB_SIG_HEX_LEN].as_bytes()).then_some(sub_id)
}

/// Constant-time byte comparison. `==` on `&[u8]` short-circuits on the first
/// differing byte, which leaks the length of a correct prefix to anyone who can
/// time the endpoint.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
```

- [ ] **Step 4: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts crypto`
  Expected: 7 new tests pass alongside the existing `hmac_rfc4231_case2`.

- [ ] **Step 5: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 8: Config keys and their four documentation homes

**Files:**
- Modify `backend/crates/sauron-core/src/config.rs` (struct fields after `alert_event_retention_days` at line 94; initializers after line 214)
- Modify `.env.example` (the `# --- alerting (sauron-alerts) ---` block, around line 47)
- Modify `README.md` (the `### Alerting & notifications` table, around line 204)
- Modify `docker-compose.yml` (the `alerts:` service `environment:` map, around line 109)
- Modify `packaging/rpm/config/alerts.env`

**Interfaces:**
- Consumes: the private `fn parse<T: FromStr>(key: &str, default: T) -> T` (config.rs:108).
- Produces: `Config` fields `notify_subs_tick_secs: u64`, `notify_subs_batch: i64`, `notify_subs_max_probes_per_org: usize`, `notify_drain_budget_ms: u64`, `notify_max_emails_per_user_per_hour: i64`, `notify_queue_retention_days: i64`.

- [ ] **Step 1: Write the failing test.** Append to `backend/crates/sauron-core/src/config.rs`, inside its existing `#[cfg(test)] mod tests` block (or create one at the end of the file if none exists):

```rust
    /// Every one of the six personal-notification knobs is clamped at point of
    /// use, following `alerts_tick_secs`. This pins the defaults so a typo in a
    /// `parse(...)` default cannot ship silently.
    #[test]
    fn personal_notification_defaults() {
        // `from_env` reads the process environment, which other tests share, so
        // assert on the documented constants rather than constructing a Config.
        assert_eq!(NOTIFY_SUBS_TICK_SECS_DEFAULT, 120);
        assert_eq!(NOTIFY_SUBS_BATCH_DEFAULT, 200);
        assert_eq!(NOTIFY_SUBS_MAX_PROBES_PER_ORG_DEFAULT, 50);
        assert_eq!(NOTIFY_DRAIN_BUDGET_MS_DEFAULT, 10_000);
        assert_eq!(NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR_DEFAULT, 20);
        assert_eq!(NOTIFY_QUEUE_RETENTION_DAYS_DEFAULT, 14);
    }
```

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-core personal_notification_defaults`
  Expected: `error[E0425]: cannot find value 'NOTIFY_SUBS_TICK_SECS_DEFAULT' in this scope`.

- [ ] **Step 3: Add the constants and struct fields.** In `backend/crates/sauron-core/src/config.rs`, add the constants beside `MIN_JWT_SECRET_LEN`:

```rust
pub const NOTIFY_SUBS_TICK_SECS_DEFAULT: u64 = 120;
pub const NOTIFY_SUBS_BATCH_DEFAULT: i64 = 200;
pub const NOTIFY_SUBS_MAX_PROBES_PER_ORG_DEFAULT: usize = 50;
pub const NOTIFY_DRAIN_BUDGET_MS_DEFAULT: u64 = 10_000;
pub const NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR_DEFAULT: i64 = 20;
pub const NOTIFY_QUEUE_RETENTION_DAYS_DEFAULT: i64 = 14;
```

and the fields immediately after `pub alert_event_retention_days: i64,`:

```rust
    // --- personal notifications ---
    /// Personal-subscription evaluation cadence. Deliberately slower than the
    /// 30s org tick: personal email does not need 30s latency, and cadence is
    /// the single largest cost lever in this subsystem. Clamped 30..3600.
    pub notify_subs_tick_secs: u64,
    /// Rows one drain claim takes. Unclamped, the claim's `RETURNING *` is
    /// unbounded. Clamped 1..5000.
    pub notify_subs_batch: i64,
    /// Per-org probe ceiling. A single GLOBAL ceiling would be a cross-tenant
    /// starvation vector: self-registered accounts saturating it would silently
    /// stop evaluating a paying tenant's subscriptions. Clamped 1..1000.
    pub notify_subs_max_probes_per_org: usize,
    /// Wall-clock budget for one drain pass, so a backlog cannot stall the
    /// tick. Clamped 500..60000.
    pub notify_drain_budget_ms: u64,
    /// Above this, a user's surviving rows are merged into ONE digest rather
    /// than dropped. Clamped 1..1000.
    pub notify_max_emails_per_user_per_hour: i64,
    /// `notification_queue` retention. `0` would evaluate to
    /// `now() - '0 days'` and wipe the table, hence the clamp. Clamped 1..365.
    pub notify_queue_retention_days: i64,
```

- [ ] **Step 4: Add the initializers.** In `Config::from_env`'s struct literal, immediately after the `alerts_allow_private: …` entry:

```rust
            notify_subs_tick_secs: parse("NOTIFY_SUBS_TICK_SECS", NOTIFY_SUBS_TICK_SECS_DEFAULT),
            notify_subs_batch: parse("NOTIFY_SUBS_BATCH", NOTIFY_SUBS_BATCH_DEFAULT),
            notify_subs_max_probes_per_org: parse(
                "NOTIFY_SUBS_MAX_PROBES_PER_ORG",
                NOTIFY_SUBS_MAX_PROBES_PER_ORG_DEFAULT,
            ),
            notify_drain_budget_ms: parse("NOTIFY_DRAIN_BUDGET_MS", NOTIFY_DRAIN_BUDGET_MS_DEFAULT),
            notify_max_emails_per_user_per_hour: parse(
                "NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR",
                NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR_DEFAULT,
            ),
            notify_queue_retention_days: parse(
                "NOTIFY_QUEUE_RETENTION_DAYS",
                NOTIFY_QUEUE_RETENTION_DAYS_DEFAULT,
            ),
```

- [ ] **Step 5: Document in `.env.example`.** Append to the `# --- alerting (sauron-alerts) ---` block:

```sh
# --- personal notification subscriptions (sauron-alerts) ---
# How often personal subscriptions are evaluated. Clamped 30-3600.
NOTIFY_SUBS_TICK_SECS=120
# Rows one drain pass claims at a time. Clamped 1-5000.
NOTIFY_SUBS_BATCH=200
# Per-ORG probe ceiling per tick. Over it, some of that org's subscriptions are
# skipped for the tick and a warning names the org. Clamped 1-1000.
NOTIFY_SUBS_MAX_PROBES_PER_ORG=50
# Wall-clock budget for one drain pass, in ms. Clamped 500-60000.
NOTIFY_DRAIN_BUDGET_MS=10000
# Above this many messages per user per hour, delivery degrades to one digest
# rather than dropping anything. Clamped 1-1000.
NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR=20
# How long terminal notification_queue rows are kept. Clamped 1-365.
NOTIFY_QUEUE_RETENTION_DAYS=14
```

- [ ] **Step 6: Document in `README.md`.** Append six rows to the `### Alerting & notifications` table, matching the existing `| var | description | default | service |` column shape:

```markdown
| `NOTIFY_SUBS_TICK_SECS` | How often per-user notification subscriptions are evaluated. Clamped to `30`–`3600`. | `120` | alerts |
| `NOTIFY_SUBS_BATCH` | Rows one notification-drain pass claims at a time. Clamped to `1`–`5000`. | `200` | alerts |
| `NOTIFY_SUBS_MAX_PROBES_PER_ORG` | Per-organization probe ceiling per tick; orgs are processed in rotating order so a clip moves around. Clamped to `1`–`1000`. | `50` | alerts |
| `NOTIFY_DRAIN_BUDGET_MS` | Wall-clock budget for one drain pass, so a backlog cannot stall the tick. Clamped to `500`–`60000`. | `10000` | alerts |
| `NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR` | Above this, a user's notifications are merged into one digest instead of being dropped. Clamped to `1`–`1000`. | `20` | alerts |
| `NOTIFY_QUEUE_RETENTION_DAYS` | How long finished `notification_queue` rows are kept. Pending and claimed rows are never pruned. Clamped to `1`–`365`. | `14` | alerts |
```

- [ ] **Step 7: Document in `docker-compose.yml`.** In the `alerts:` service's `environment:` map, after `ALERTS_TICK_SECS: ${ALERTS_TICK_SECS:-30}`:

```yaml
      NOTIFY_SUBS_TICK_SECS: ${NOTIFY_SUBS_TICK_SECS:-120}
      NOTIFY_SUBS_BATCH: ${NOTIFY_SUBS_BATCH:-200}
      NOTIFY_SUBS_MAX_PROBES_PER_ORG: ${NOTIFY_SUBS_MAX_PROBES_PER_ORG:-50}
      NOTIFY_DRAIN_BUDGET_MS: ${NOTIFY_DRAIN_BUDGET_MS:-10000}
      NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR: ${NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR:-20}
      NOTIFY_QUEUE_RETENTION_DAYS: ${NOTIFY_QUEUE_RETENTION_DAYS:-14}
```

- [ ] **Step 8: Document in `packaging/rpm/config/alerts.env`.** Append, matching that file's convention of stating the operational consequence:

```sh
# --- personal notification subscriptions ---

# Evaluation cadence for per-user subscriptions. This is the dominant cost
# lever for this subsystem: halving it doubles the query load. Raising it only
# delays personal email, which is not a paging channel.
NOTIFY_SUBS_TICK_SECS=120

# Rows one drain pass claims. The claim is `RETURNING *`, so an unclamped value
# would pull the whole backlog into memory in one round trip.
NOTIFY_SUBS_BATCH=200

# Per-ORGANIZATION probe ceiling. Over it, that org's remaining subscriptions
# are skipped for the tick and a WARN line names the org and the skipped count.
# Orgs are processed in rotating order so a clip does not always land on the
# same tenant.
NOTIFY_SUBS_MAX_PROBES_PER_ORG=50

# Wall-clock budget for one drain pass, in milliseconds. A drain that exceeds it
# stops and resumes next tick, so a backlog delays mail instead of stalling
# evaluation.
NOTIFY_DRAIN_BUDGET_MS=10000

# Messages per user per hour before delivery degrades to a single digest.
# Nothing is dropped at any value.
NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR=20

# Retention for FINISHED notification_queue rows. Pending and claimed rows are
# never pruned -- they are the evidence of whatever outage made them pile up.
NOTIFY_QUEUE_RETENTION_DAYS=14
```

- [ ] **Step 9: Run the test and watch it pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-core personal_notification_defaults`
  Expected: `test config::tests::personal_notification_defaults ... ok`.

- [ ] **Step 10: Verify the config-documentation CI gate is satisfied.**
  `cd /home/splimter/projects/freelance/sauron && for k in NOTIFY_SUBS_TICK_SECS NOTIFY_SUBS_BATCH NOTIFY_SUBS_MAX_PROBES_PER_ORG NOTIFY_DRAIN_BUDGET_MS NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR NOTIFY_QUEUE_RETENTION_DAYS; do grep -q "^$k=" .env.example || echo "MISSING $k"; done`
  Expected: no output.

- [ ] **Step 11: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 9: `repo.rs` — subscription reads and the upsert CTE

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (append a new `// === Personal notification subscriptions ===` section at the end of the file)
- Modify `backend/crates/sauron-db/tests/notifications.rs`

**Interfaces:**
- Consumes: `models::NotificationSubscription` (Task 1).
- Produces:
  - `pub async fn upsert_subscription(conn, user_id: Uuid, org_id: Uuid, scope_type: &str, scope_id: Uuid, kind: &str, conditions: &Value, delivery: &str, throttle_seconds: i32, quiet_start_min: Option<i16>, quiet_end_min: Option<i16>, quiet_tz: &str, env_ids: &[Uuid]) -> QueryResult<NotificationSubscription>`
  - `pub async fn list_subscriptions_for_user(conn, user_id: Uuid) -> QueryResult<Vec<NotificationSubscription>>`
  - `pub async fn get_subscription(conn, id: Uuid) -> QueryResult<Option<NotificationSubscription>>`
  - `pub async fn delete_subscription(conn, id: Uuid, user_id: Uuid) -> QueryResult<usize>`
  - `pub async fn set_subscription_enabled(conn, id: Uuid, user_id: Uuid, enabled: bool) -> QueryResult<usize>`
  - `pub async fn disable_subscription(conn, id: Uuid, reason: &str) -> QueryResult<usize>`
  - `pub async fn subscription_envs_for(conn, subscription_ids: &[Uuid]) -> QueryResult<Vec<(Uuid, Uuid)>>`
  - `pub async fn live_catalogue_envs_for_project(conn, project_id: Uuid) -> QueryResult<Vec<Uuid>>`
  - `pub async fn timezone_exists(conn, tz: &str) -> QueryResult<bool>`
  - `pub async fn enabled_subscriptions_by_kinds(conn, kinds: &[&str]) -> QueryResult<Vec<NotificationSubscription>>`
  - `pub async fn uptime_subscriptions_for_project(conn, project_id: Uuid) -> QueryResult<Vec<NotificationSubscription>>`
  - `pub async fn subscriptions_for_user_in_org(conn, user_id: Uuid, org_id: Uuid) -> QueryResult<Vec<NotificationSubscription>>`
  - `pub async fn apps_for_projects(conn, project_ids: &[Uuid]) -> QueryResult<Vec<(Uuid, Uuid)>>` — `(project_id, app_id)`; the batched `list_apps_for_project` the evaluation pass needs
  - `pub async fn touch_subscriptions_evaluated(conn, ids: &[Uuid], at: DateTime<Utc>) -> QueryResult<usize>`

- [ ] **Step 1: Write the failing test for the CTE's atomicity.** Append to `backend/crates/sauron-db/tests/notifications.rs`:

```rust
/// `upsert_subscription` writes the parent and REPLACES the env child rows in a
/// single data-modifying CTE — one statement, therefore atomic, without
/// `conn.transaction` (MSRV 1.82 blocks it).
#[tokio::test]
async fn upsert_subscription_replaces_the_env_set_in_one_statement() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let user_id = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .unwrap()
        .unwrap()
        .id;

    // The subscription stores CATALOGUE ids; `seed_two_envs` hands back
    // ENROLLMENT ids, so resolve across.
    let catalogue: Vec<uuid::Uuid> =
        sauron_db::repo::live_catalogue_envs_for_project(&mut conn, ids.project_id)
            .await
            .expect("catalogue envs");
    assert_eq!(catalogue.len(), 2);

    let conditions = serde_json::json!({ "level": "error" });
    let sub = sauron_db::repo::upsert_subscription(
        &mut conn, user_id, ids.org_id, "project", ids.project_id, "error_new_issue",
        &conditions, "immediate", 900, None, None, "UTC", &catalogue,
    )
    .await
    .expect("first upsert");
    let created_at = sub.created_at;

    let envs = sauron_db::repo::subscription_envs_for(&mut conn, &[sub.id])
        .await
        .expect("child rows");
    assert_eq!(envs.len(), 2);

    // Narrow the set to one environment.
    let again = sauron_db::repo::upsert_subscription(
        &mut conn, user_id, ids.org_id, "project", ids.project_id, "error_new_issue",
        &conditions, "daily", 1800, Some(1320), Some(360), "Europe/Paris", &catalogue[..1],
    )
    .await
    .expect("second upsert");

    assert_eq!(again.id, sub.id, "the unique key made this an update, not an insert");
    assert_eq!(again.created_at, created_at, "created_at must survive the upsert");
    assert_eq!(again.delivery, "daily");
    assert_eq!(again.throttle_seconds, 1800);
    assert_eq!(again.quiet_tz, "Europe/Paris");

    let envs = sauron_db::repo::subscription_envs_for(&mut conn, &[sub.id])
        .await
        .expect("child rows after narrowing");
    assert_eq!(envs.len(), 1, "the removed environment's child row is gone");
    assert_eq!(envs[0].1, catalogue[0]);

    // A rejected insert leaves no orphaned child rows — which is what proves
    // the CTE is really one statement.
    let bad = sauron_db::repo::upsert_subscription(
        &mut conn, user_id, ids.org_id, "org", ids.project_id, "error_new_issue",
        &conditions, "immediate", 900, None, None, "UTC", &catalogue,
    )
    .await;
    assert!(bad.is_err(), "scope_type='org' violates the CHECK");
    let envs = sauron_db::repo::subscription_envs_for(&mut conn, &[sub.id])
        .await
        .expect("child rows after the failed insert");
    assert_eq!(envs.len(), 1, "the failed statement wrote nothing at all");

    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications upsert_subscription`
  Expected: `error[E0425]: cannot find function 'upsert_subscription' in module 'sauron_db::repo'`.

- [ ] **Step 3: Add the section header and the upsert.** Append at the end of `backend/crates/sauron-db/src/repo.rs`:

```rust
// ===========================================================================
// Personal notification subscriptions (S3)
//
// Two environment id spaces meet in this section and confusing them produces a
// subscription that matches nothing, silently:
//   * `notification_subscription_envs.environment_id` is a CATALOGUE id
//     (`environments.id`, project-level since migration 33).
//   * `notification_queue_envs.environment_id`, `error_events.environment_id`
//     and `role_grants.scope_id` for `scope_type='env'` are ENROLLMENT ids
//     (`app_environments.id`).
// `live_enrollments_for_apps` is the only sanctioned bridge.
// ===========================================================================

/// Create or update a subscription and replace its environment set, in ONE
/// data-modifying CTE.
///
/// One statement means atomicity without `conn.transaction`, which the MSRV
/// blocks. A two-statement version could leave the parent updated and the child
/// rows stale — and a stale-empty child set is read everywhere downstream as
/// "all environments", which WIDENS the subscription rather than narrowing it.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_subscription(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
    scope_type: &str,
    scope_id: Uuid,
    kind: &str,
    conditions: &Value,
    delivery: &str,
    throttle_seconds: i32,
    quiet_start_min: Option<i16>,
    quiet_end_min: Option<i16>,
    quiet_tz: &str,
    env_ids: &[Uuid],
) -> QueryResult<NotificationSubscription> {
    diesel::sql_query(
        "WITH up AS ( \
             INSERT INTO notification_subscriptions \
                 (user_id, org_id, scope_type, scope_id, kind, conditions, delivery, \
                  throttle_seconds, quiet_start_min, quiet_end_min, quiet_tz, \
                  enabled, disabled_reason, disabled_at, last_evaluated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, true, NULL, NULL, now()) \
             ON CONFLICT (user_id, scope_type, scope_id, kind) DO UPDATE SET \
                 org_id = EXCLUDED.org_id, \
                 conditions = EXCLUDED.conditions, \
                 delivery = EXCLUDED.delivery, \
                 throttle_seconds = EXCLUDED.throttle_seconds, \
                 quiet_start_min = EXCLUDED.quiet_start_min, \
                 quiet_end_min = EXCLUDED.quiet_end_min, \
                 quiet_tz = EXCLUDED.quiet_tz, \
                 enabled = true, \
                 disabled_reason = NULL, \
                 disabled_at = NULL, \
                 updated_at = now() \
             RETURNING * \
         ), del AS ( \
             DELETE FROM notification_subscription_envs \
              WHERE subscription_id = (SELECT id FROM up) \
                AND environment_id <> ALL($12) \
         ), ins AS ( \
             INSERT INTO notification_subscription_envs (subscription_id, environment_id) \
             SELECT (SELECT id FROM up), e FROM unnest($12::uuid[]) AS e \
             ON CONFLICT DO NOTHING \
         ) \
         SELECT * FROM up",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<SqlUuid, _>(org_id)
    .bind::<Text, _>(scope_type)
    .bind::<SqlUuid, _>(scope_id)
    .bind::<Text, _>(kind)
    .bind::<Jsonb, _>(conditions)
    .bind::<Text, _>(delivery)
    .bind::<Integer, _>(throttle_seconds)
    .bind::<Nullable<SmallInt>, _>(quiet_start_min)
    .bind::<Nullable<SmallInt>, _>(quiet_end_min)
    .bind::<Text, _>(quiet_tz)
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(env_ids)
    .get_result(conn)
    .await
}
```

If `Integer` / `SmallInt` are not already in the file's `use diesel::sql_types::{…}` list, add them there.

- [ ] **Step 4: Add the remaining subscription reads and writes.** Append below `upsert_subscription`:

```rust
pub async fn list_subscriptions_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<Vec<NotificationSubscription>> {
    notification_subscriptions::table
        .filter(notification_subscriptions::user_id.eq(user_id))
        .order(notification_subscriptions::created_at.asc())
        .select(NotificationSubscription::as_select())
        .load(conn)
        .await
}

pub async fn get_subscription(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<NotificationSubscription>> {
    notification_subscriptions::table
        .find(id)
        .select(NotificationSubscription::as_select())
        .first(conn)
        .await
        .optional()
}

/// Owner-scoped delete. `user_id` is part of the predicate rather than checked
/// by the caller so a missing check cannot delete someone else's row; the
/// handler turns a zero row count into 404, never 403, so a non-owner learns
/// nothing about whether the id exists.
pub async fn delete_subscription(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    user_id: Uuid,
) -> QueryResult<usize> {
    diesel::delete(
        notification_subscriptions::table
            .filter(notification_subscriptions::id.eq(id))
            .filter(notification_subscriptions::user_id.eq(user_id)),
    )
    .execute(conn)
    .await
}

/// Owner-driven enable/disable. Re-enabling always clears `disabled_reason`:
/// re-granting access does not silently resurrect a subscription, the user
/// turns it back on themselves, and at that moment the reason is stale.
pub async fn set_subscription_enabled(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    user_id: Uuid,
    enabled: bool,
) -> QueryResult<usize> {
    diesel::update(
        notification_subscriptions::table
            .filter(notification_subscriptions::id.eq(id))
            .filter(notification_subscriptions::user_id.eq(user_id)),
    )
    .set((
        notification_subscriptions::enabled.eq(enabled),
        notification_subscriptions::disabled_reason
            .eq::<Option<String>>(if enabled { None } else { Some("unsubscribed".into()) }),
        notification_subscriptions::disabled_at
            .eq::<Option<DateTime<Utc>>>(if enabled { None } else { Some(Utc::now()) }),
        notification_subscriptions::updated_at.eq(Utc::now()),
    ))
    .execute(conn)
    .await
}

/// System-driven disable: the unsubscribe link (`'unsubscribed'`) and the
/// revocation sweep (`'access_revoked'`). Not owner-scoped, because neither
/// caller is the owner acting through the UI.
pub async fn disable_subscription(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    reason: &str,
) -> QueryResult<usize> {
    diesel::update(notification_subscriptions::table.find(id))
        .set((
            notification_subscriptions::enabled.eq(false),
            notification_subscriptions::disabled_reason.eq(Some(reason.to_string())),
            notification_subscriptions::disabled_at.eq(Some(Utc::now())),
            notification_subscriptions::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}

/// `(subscription_id, catalogue_environment_id)` for many subscriptions at
/// once — the evaluator resolves every subscription's environment set in one
/// query, never one per subscription.
pub async fn subscription_envs_for(
    conn: &mut AsyncPgConnection,
    subscription_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    if subscription_ids.is_empty() {
        return Ok(Vec::new());
    }
    notification_subscription_envs::table
        .filter(notification_subscription_envs::subscription_id.eq_any(subscription_ids.to_vec()))
        .select((
            notification_subscription_envs::subscription_id,
            notification_subscription_envs::environment_id,
        ))
        .load(conn)
        .await
}

/// Live CATALOGUE environment ids of a project — what a subscription's
/// `environment_ids` are validated against, and what the dashboard's chip row
/// offers.
pub async fn live_catalogue_envs_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    environments::table
        .filter(environments::project_id.eq(project_id))
        .filter(environments::retired_at.is_null())
        .order(environments::name.asc())
        .select(environments::id)
        .load(conn)
        .await
}

#[derive(Debug, QueryableByName)]
struct BoolRow {
    #[diesel(sql_type = Bool)]
    ok: bool,
}

/// Whether `tz` is a zone this Postgres knows.
///
/// Validated at write time so a typo is a 400 rather than a row the enqueue
/// then has to defend against. The enqueue re-checks anyway: a zone that
/// validated here can vanish with an OS tzdata update, and
/// `now() AT TIME ZONE 'Missing/Zone'` raises, which would kill the whole
/// batch over one bad row.
pub async fn timezone_exists(conn: &mut AsyncPgConnection, tz: &str) -> QueryResult<bool> {
    let row: BoolRow =
        diesel::sql_query("SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name = $1) AS ok")
            .bind::<Text, _>(tz)
            .get_result(conn)
            .await?;
    Ok(row.ok)
}

/// Every enabled subscription of the given kinds, in one query, served by
/// `notification_subscriptions_kind_idx (kind) WHERE enabled`.
pub async fn enabled_subscriptions_by_kinds(
    conn: &mut AsyncPgConnection,
    kinds: &[&str],
) -> QueryResult<Vec<NotificationSubscription>> {
    if kinds.is_empty() {
        return Ok(Vec::new());
    }
    notification_subscriptions::table
        .filter(notification_subscriptions::enabled.eq(true))
        .filter(notification_subscriptions::kind.eq_any(kinds.iter().map(|k| k.to_string()).collect::<Vec<_>>()))
        .select(NotificationSubscription::as_select())
        .load(conn)
        .await
}

/// Enabled uptime subscriptions on exactly this project.
///
/// The prober calls this from `notify_transition`; the caller still runs the
/// coverage predicate against freshly loaded grants, because a subscription's
/// owner may have lost project reach since it was created.
pub async fn uptime_subscriptions_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<NotificationSubscription>> {
    notification_subscriptions::table
        .filter(notification_subscriptions::enabled.eq(true))
        .filter(notification_subscriptions::kind.eq("uptime"))
        .filter(notification_subscriptions::scope_type.eq("project"))
        .filter(notification_subscriptions::scope_id.eq(project_id))
        .select(NotificationSubscription::as_select())
        .load(conn)
        .await
}

/// Every subscription a user holds inside one org — what the synchronous
/// revocation sweep re-evaluates after a grant change commits.
pub async fn subscriptions_for_user_in_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> QueryResult<Vec<NotificationSubscription>> {
    notification_subscriptions::table
        .filter(notification_subscriptions::user_id.eq(user_id))
        .filter(notification_subscriptions::org_id.eq(org_id))
        .filter(notification_subscriptions::enabled.eq(true))
        .select(NotificationSubscription::as_select())
        .load(conn)
        .await
}

/// `(project_id, app_id)` for every app under any of `project_ids` — the
/// batched `list_apps_for_project`.
///
/// The evaluation pass resolves N project-scoped subscriptions per tick. Calling
/// `list_apps_for_project` once each is N round trips against a pool of 8 shared
/// with the drain, which is precisely the per-subscription blow-up the probe
/// coalescing exists to prevent; doing it in the resolution loop would put the
/// cost back one layer down.
pub async fn apps_for_projects(
    conn: &mut AsyncPgConnection,
    project_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    apps::table
        .filter(apps::project_id.eq_any(project_ids.to_vec()))
        .select((apps::project_id, apps::id))
        .load(conn)
        .await
}

/// Advance the watermark on a batch of subscriptions in one statement.
pub async fn touch_subscriptions_evaluated(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
    at: DateTime<Utc>,
) -> QueryResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    diesel::update(
        notification_subscriptions::table
            .filter(notification_subscriptions::id.eq_any(ids.to_vec())),
    )
    .set(notification_subscriptions::last_evaluated_at.eq(at))
    .execute(conn)
    .await
}
```

Add `NotificationSubscription` to the file's `use crate::models::{…}` list and the four table names to its `use crate::schema::{…}` list (or confirm the file already does `use crate::schema::*;`).

- [ ] **Step 5: Run the test and watch it pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications upsert_subscription`
  Expected: `test upsert_subscription_replaces_the_env_set_in_one_statement ... ok`.

- [ ] **Step 6: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 10: `repo.rs` — the enqueue, `deliver_after`, and the dedup index

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs`
- Modify `backend/crates/sauron-db/tests/notifications.rs`

**Interfaces:**
- Consumes: `upsert_subscription` (Task 9), `in_quiet_hours` (Task 4, for the agreement test).
- Produces:
  - `pub struct QueueInsert<'a> { pub subscription_id: Uuid, pub project_id: Uuid, pub app_id: Option<Uuid>, pub includes_unattributed: bool, pub kind: &'a str, pub dedup_key: &'a str, pub severity: &'a str, pub title: &'a str, pub body: &'a str, pub link: Option<&'a str>, pub env_enrollments: Vec<Uuid> }`
  - `pub async fn enqueue_notifications(conn, rows: &[QueueInsert<'_>]) -> QueryResult<i64>`
  - `pub async fn notification_recently_queued(conn, subscription_id: Uuid, dedup_key: &str, within_seconds: i32) -> QueryResult<bool>`

- [ ] **Step 1: Write the failing tests.** Append to `backend/crates/sauron-db/tests/notifications.rs`:

```rust
use sauron_db::repo::QueueInsert;

async fn seed_subscription(
    conn: &mut sauron_db::PgConn,
    ids: &common::SeedIds,
    kind: &str,
    delivery: &str,
    quiet: Option<(i16, i16)>,
    tz: &str,
) -> sauron_db::models::NotificationSubscription {
    let user_id = sauron_db::repo::find_user_by_email(conn, &ids.owner_email)
        .await
        .unwrap()
        .unwrap()
        .id;
    sauron_db::repo::upsert_subscription(
        conn,
        user_id,
        ids.org_id,
        "project",
        ids.project_id,
        kind,
        &serde_json::json!({}),
        delivery,
        900,
        quiet.map(|q| q.0),
        quiet.map(|q| q.1),
        tz,
        &[],
    )
    .await
    .expect("seed subscription")
}

/// Without a unique constraint `ON CONFLICT DO NOTHING` can only ever fire on
/// the id PK — i.e. never — and the clause would read as idempotency while
/// providing none. Scoping the index to LIVE rows is what lets the next
/// legitimate notification through after the first one sends.
#[tokio::test]
async fn the_live_dedup_index_suppresses_only_live_duplicates() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;

    let dedup = format!("sub:{}:spike:{}", sub.id, ids.app_id);
    let row = QueueInsert {
        subscription_id: sub.id,
        project_id: ids.project_id,
        app_id: Some(ids.app_id),
        includes_unattributed: true,
        kind: "error_spike",
        dedup_key: &dedup,
        severity: "warning",
        title: "Error spike",
        body: "30 errors vs 10",
        link: None,
        env_enrollments: vec![ids.env_a],
    };

    // Two identical enqueues in the SAME statement produce one row.
    let n = sauron_db::repo::enqueue_notifications(&mut conn, &[row.clone(), row.clone()])
        .await
        .expect("double enqueue");
    assert_eq!(n, 1);

    // A third while the first is still pending produces nothing.
    let n = sauron_db::repo::enqueue_notifications(&mut conn, &[row.clone()])
        .await
        .expect("third enqueue");
    assert_eq!(n, 0);

    diesel::sql_query(
        "UPDATE notification_queue SET status='sent', sent_at=now(), finished_at=now()",
    )
    .execute(&mut conn)
    .await
    .expect("mark sent");

    // Once it has sent, the next legitimate notification is allowed through.
    let n = sauron_db::repo::enqueue_notifications(&mut conn, &[row])
        .await
        .expect("enqueue after send");
    assert_eq!(n, 1);

    db.cleanup().await;
}

/// `deliver_after` is computed entirely in SQL, because the workspace has no
/// `chrono-tz` and nothing in Rust can produce a subscription's local
/// wall-clock time. This asserts the SQL agrees with `in_quiet_hours` over the
/// cases that matter, including a `quiet_tz` Postgres does not know.
#[tokio::test]
async fn deliver_after_defers_into_quiet_hours_and_survives_an_unknown_zone() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // A window that covers the whole day but one minute: whatever the wall
    // clock says, `now()` is inside it, so delivery must be pushed forward.
    let quiet = seed_subscription(
        &mut conn, &ids, "error_new_issue", "immediate", Some((1, 0)), "Europe/Paris",
    )
    .await;
    let dedup_q = format!("sub:{}:issue:{}", quiet.id, ids.issue_id);
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: quiet.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_new_issue",
            dedup_key: &dedup_q,
            severity: "warning",
            title: "New issue",
            body: "body",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .expect("enqueue quiet");

    // An unknown zone must fall back to UTC rather than raising and killing the
    // whole batch — a zone that validated at write time can vanish with an OS
    // tzdata update.
    let bogus = seed_subscription(
        &mut conn, &ids, "error_regression", "daily", Some((1320, 360)), "Missing/Zone",
    )
    .await;
    let dedup_b = format!("sub:{}:issue:{}", bogus.id, ids.issue_id);
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: bogus.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_regression",
            dedup_key: &dedup_b,
            severity: "warning",
            title: "Regressed",
            body: "body",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .expect("enqueue with an unknown zone");

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Uuid)]
        subscription_id: uuid::Uuid,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        deferred: bool,
    }
    let rows: Vec<Row> = diesel::sql_query(
        "SELECT subscription_id, (deliver_after > now() + interval '30 seconds') AS deferred \
           FROM notification_queue",
    )
    .load(&mut conn)
    .await
    .expect("read deliver_after");
    assert_eq!(rows.len(), 2, "the unknown zone did not kill the batch");
    for r in rows {
        assert!(
            r.deferred,
            "subscription {} should have been deferred past its quiet window",
            r.subscription_id
        );
    }

    // The Rust twin agrees on the same shape. The exhaustive
    // agreement check — including the DST case — is the next test.
    assert!(sauron_alerts::subscription::in_quiet_hours(720, 1, 0));

    db.cleanup().await;
}

/// The SQL `CASE` and Rust's `in_quiet_hours` must agree on every case, and
/// only Postgres can turn `(now, tz)` into a local wall clock — `chrono-tz` is
/// not a dependency anywhere in this workspace and nothing in Rust here can do
/// it. Two implementations of one predicate drift silently, and the symptom of
/// the drift is somebody's phone at 04:00, so they are pinned to each other
/// over a shared table.
///
/// The two `Europe/Paris` rows on 2026-03-29 are the point of the test. The
/// clock jumps 02:00 -> 03:00 at 01:00 UTC that morning, so 01:30 UTC is 03:30
/// local, not 02:30. An implementation that computed the local minute by adding
/// a fixed offset — or that skipped the conversion entirely — puts it at 02:30,
/// inside the window, and holds the message for an hour it should have gone.
#[tokio::test]
async fn the_quiet_hours_sql_and_the_rust_twin_agree_over_a_shared_table() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    // (tz, instant in UTC, quiet_start_min, quiet_end_min, expected local
    //  minute, expected "is quiet")
    let cases: Vec<(&str, &str, i32, i32, i32, bool)> = vec![
        // Wrap-around window, UTC: inside, then outside.
        ("UTC", "2026-01-15T23:30:00Z", 1320, 360, 1410, true),
        ("UTC", "2026-01-15T07:00:00Z", 1320, 360, 420, false),
        // The start minute is inside; the end minute is outside.
        ("UTC", "2026-01-15T22:00:00Z", 1320, 360, 1320, true),
        ("UTC", "2026-01-15T06:00:00Z", 1320, 360, 360, false),
        // A zero-width window must not silence everything forever.
        ("UTC", "2026-01-15T05:00:00Z", 300, 300, 300, false),
        // Winter Paris is UTC+1: 22:30Z is 23:30 local (inside), 20:30Z is
        // 21:30 local (outside). A UTC-only implementation gets both backwards.
        ("Europe/Paris", "2026-01-15T22:30:00Z", 1320, 360, 1410, true),
        ("Europe/Paris", "2026-01-15T20:30:00Z", 1320, 360, 1290, false),
        // Spring-forward morning, window 01:00 -> 03:00 local.
        // 00:30Z is still CET (+1) => 01:30 local, inside.
        ("Europe/Paris", "2026-03-29T00:30:00Z", 60, 180, 90, true),
        // 01:30Z is already CEST (+2) => 03:30 local, OUTSIDE. Naive +1 would
        // say 02:30 and defer.
        ("Europe/Paris", "2026-03-29T01:30:00Z", 60, 180, 210, false),
    ];

    let idx: Vec<i32> = (0..cases.len() as i32).collect();
    let tzs: Vec<String> = cases.iter().map(|c| c.0.to_string()).collect();
    let ats: Vec<chrono::DateTime<chrono::Utc>> = cases
        .iter()
        .map(|c| {
            chrono::DateTime::parse_from_rfc3339(c.1)
                .expect("rfc3339 case instant")
                .with_timezone(&chrono::Utc)
        })
        .collect();
    let starts: Vec<i32> = cases.iter().map(|c| c.2).collect();
    let ends: Vec<i32> = cases.iter().map(|c| c.3).collect();

    #[derive(diesel::QueryableByName)]
    struct Verdict {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        idx: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        local_min: i32,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        quiet: bool,
    }

    // Character-for-character the branch structure of the `CASE` inside
    // `enqueue_notifications`: equal bounds are never quiet, `start < end` is a
    // same-day half-open interval, otherwise it wraps midnight.
    let rows: Vec<Verdict> = diesel::sql_query(
        "SELECT s.idx, s.local_min, \
                CASE \
                  WHEN s.qs = s.qe THEN false \
                  WHEN s.qs < s.qe THEN (s.local_min >= s.qs AND s.local_min < s.qe) \
                  ELSE (s.local_min >= s.qs OR s.local_min < s.qe) \
                END AS quiet \
           FROM ( \
             SELECT c.idx, c.qs, c.qe, \
                    (EXTRACT(HOUR FROM (c.at AT TIME ZONE c.tz)) * 60 \
                     + EXTRACT(MINUTE FROM (c.at AT TIME ZONE c.tz)))::int AS local_min \
               FROM unnest($1::int[], $2::text[], $3::timestamptz[], $4::int[], $5::int[]) \
                      AS c(idx, tz, at, qs, qe) \
           ) s \
          ORDER BY s.idx",
    )
    .bind::<diesel::sql_types::Array<diesel::sql_types::Integer>, _>(idx)
    .bind::<diesel::sql_types::Array<Text>, _>(tzs)
    .bind::<diesel::sql_types::Array<diesel::sql_types::Timestamptz>, _>(ats)
    .bind::<diesel::sql_types::Array<diesel::sql_types::Integer>, _>(starts)
    .bind::<diesel::sql_types::Array<diesel::sql_types::Integer>, _>(ends)
    .load(&mut conn)
    .await
    .expect("evaluate the quiet-hours table in SQL");

    assert_eq!(rows.len(), cases.len());
    for row in rows {
        let (tz, at, start, end, want_local, want_quiet) = cases[row.idx as usize];
        assert_eq!(
            row.local_min, want_local,
            "{tz} at {at}: Postgres computed local minute {} not {want_local}",
            row.local_min
        );
        assert_eq!(
            row.quiet, want_quiet,
            "{tz} at {at}: the SQL CASE disagrees with the expected verdict"
        );
        assert_eq!(
            sauron_alerts::subscription::in_quiet_hours(row.local_min, start, end),
            row.quiet,
            "{tz} at {at}: in_quiet_hours and the SQL CASE have drifted apart"
        );
    }

    db.cleanup().await;
}

/// The durable fallback for when Redis is unreachable — the direct analogue of
/// `alert_recently_sent`.
#[tokio::test]
async fn notification_recently_queued_is_the_durable_throttle_backstop() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;
    let dedup = format!("sub:{}:spike:{}", sub.id, ids.app_id);

    assert!(
        !sauron_db::repo::notification_recently_queued(&mut conn, sub.id, &dedup, 900)
            .await
            .unwrap()
    );
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_spike",
            dedup_key: &dedup,
            severity: "warning",
            title: "t",
            body: "b",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .unwrap();
    assert!(
        sauron_db::repo::notification_recently_queued(&mut conn, sub.id, &dedup, 900)
            .await
            .unwrap()
    );
    assert!(
        !sauron_db::repo::notification_recently_queued(&mut conn, sub.id, &dedup, 0)
            .await
            .unwrap(),
        "a zero window never suppresses"
    );

    db.cleanup().await;
}
```

Add `sauron-alerts = { workspace = true }` to `backend/crates/sauron-db/Cargo.toml`'s `[dev-dependencies]` (create the section if absent) so the test can call `sauron_alerts::subscription::in_quiet_hours`. `sauron-alerts` depends on `sauron-db`, so this must be a **dev**-dependency; a normal dependency would be a cycle.

- [ ] **Step 2: Run them and watch them fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications`
  Expected: `error[E0432]: unresolved import sauron_db::repo::QueueInsert`.

- [ ] **Step 3: Implement the enqueue.** Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// One row to enqueue. The environment list is **enrollment** ids
/// (`app_environments.id`) — what the events the body was computed from
/// actually carry, and what the drain's coverage check compares against
/// `Reach.envs`.
#[derive(Debug, Clone)]
pub struct QueueInsert<'a> {
    pub subscription_id: Uuid,
    pub project_id: Uuid,
    /// `None` for uptime.
    pub app_id: Option<Uuid>,
    pub includes_unattributed: bool,
    pub kind: &'a str,
    pub dedup_key: &'a str,
    pub severity: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub link: Option<&'a str>,
    pub env_enrollments: Vec<Uuid>,
}

/// Insert a batch of notifications and their environment child rows in ONE
/// data-modifying CTE, computing `deliver_after` in SQL.
///
/// `deliver_after` HAS to be computed here: the workspace has no `chrono-tz`
/// (adding one is a workspace-dependency edit affecting every crate), so
/// nothing in Rust can produce a subscription's local wall-clock time.
///
/// The `pg_timezone_names` lookup is not paranoia. A zone that validated at
/// write time can vanish with an OS tzdata update, and
/// `now() AT TIME ZONE 'Missing/Zone'` RAISES — one bad row would kill the
/// whole batch. Falling back to UTC is visible in the account card (which
/// renders the effective zone) rather than silent.
///
/// The env rows are in the same statement because a queue row with a stale-empty
/// env list is read downstream as "the body spans everything", so a partial
/// failure would WIDEN a row's implied scope instead of narrowing it.
pub async fn enqueue_notifications(
    conn: &mut AsyncPgConnection,
    rows: &[QueueInsert<'_>],
) -> QueryResult<i64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let sub_ids: Vec<Uuid> = rows.iter().map(|r| r.subscription_id).collect();
    let project_ids: Vec<Uuid> = rows.iter().map(|r| r.project_id).collect();
    let app_ids: Vec<Option<Uuid>> = rows.iter().map(|r| r.app_id).collect();
    let unattributed: Vec<bool> = rows.iter().map(|r| r.includes_unattributed).collect();
    let kinds: Vec<String> = rows.iter().map(|r| r.kind.to_string()).collect();
    let dedups: Vec<String> = rows.iter().map(|r| r.dedup_key.to_string()).collect();
    let severities: Vec<String> = rows.iter().map(|r| r.severity.to_string()).collect();
    let titles: Vec<String> = rows.iter().map(|r| r.title.to_string()).collect();
    let bodies: Vec<String> = rows.iter().map(|r| r.body.to_string()).collect();
    let links: Vec<Option<String>> = rows.iter().map(|r| r.link.map(String::from)).collect();

    // Parallel arrays of (dedup_key, enrollment_id). `dedup_key` embeds the
    // subscription id, so it is unique within one batch and can join the child
    // rows back to their parent without a second round trip.
    let mut env_keys: Vec<String> = Vec::new();
    let mut env_ids: Vec<Uuid> = Vec::new();
    for r in rows {
        for e in &r.env_enrollments {
            env_keys.push(r.dedup_key.to_string());
            env_ids.push(*e);
        }
    }

    let row: AlertCountRow = diesel::sql_query(
        "WITH v AS ( \
             SELECT * FROM unnest($1::uuid[], $2::uuid[], $3::uuid[], $4::bool[], $5::text[], \
                                  $6::text[], $7::text[], $8::text[], $9::text[], $10::text[]) \
                    AS t(subscription_id, project_id, app_id, includes_unattributed, kind, \
                         dedup_key, severity, title, body, link) \
         ), j AS ( \
             SELECT v.*, s.user_id, s.org_id, s.delivery, s.quiet_start_min, s.quiet_end_min, \
                    COALESCE((SELECT n.name FROM pg_timezone_names n WHERE n.name = s.quiet_tz), \
                             'UTC') AS tz \
               FROM v JOIN notification_subscriptions s ON s.id = v.subscription_id \
         ), b AS ( \
             SELECT j.*, \
                    CASE j.delivery \
                      WHEN 'hourly' THEN date_trunc('hour', now()) + interval '1 hour' \
                      WHEN 'daily'  THEN (date_trunc('day', now() AT TIME ZONE j.tz) \
                                          + interval '1 day') AT TIME ZONE j.tz \
                      ELSE now() \
                    END AS base \
               FROM j \
         ), q AS ( \
             SELECT b.*, \
                    (EXTRACT(HOUR FROM (b.base AT TIME ZONE b.tz)) * 60 \
                     + EXTRACT(MINUTE FROM (b.base AT TIME ZONE b.tz)))::int AS local_min, \
                    date_trunc('day', b.base AT TIME ZONE b.tz) AS local_day \
               FROM b \
         ), ins AS ( \
             INSERT INTO notification_queue \
                 (subscription_id, user_id, org_id, project_id, app_id, includes_unattributed, \
                  kind, dedup_key, severity, title, body, link, deliver_after) \
             SELECT q.subscription_id, q.user_id, q.org_id, q.project_id, q.app_id, \
                    q.includes_unattributed, q.kind, q.dedup_key, q.severity, q.title, q.body, \
                    q.link, \
                    CASE \
                      WHEN q.quiet_start_min IS NULL THEN q.base \
                      WHEN q.quiet_start_min = q.quiet_end_min THEN q.base \
                      WHEN q.quiet_start_min < q.quiet_end_min THEN \
                        CASE WHEN q.local_min >= q.quiet_start_min \
                              AND q.local_min <  q.quiet_end_min \
                             THEN (q.local_day + make_interval(mins => q.quiet_end_min)) \
                                  AT TIME ZONE q.tz \
                             ELSE q.base END \
                      ELSE \
                        CASE WHEN q.local_min >= q.quiet_start_min \
                             THEN (q.local_day + interval '1 day' \
                                   + make_interval(mins => q.quiet_end_min)) AT TIME ZONE q.tz \
                             WHEN q.local_min < q.quiet_end_min \
                             THEN (q.local_day + make_interval(mins => q.quiet_end_min)) \
                                  AT TIME ZONE q.tz \
                             ELSE q.base END \
                    END \
               FROM q \
             ON CONFLICT (subscription_id, dedup_key) WHERE status IN ('pending','claimed') \
             DO NOTHING \
             RETURNING id, dedup_key \
         ), envs AS ( \
             INSERT INTO notification_queue_envs (queue_id, environment_id) \
             SELECT ins.id, e.env_id \
               FROM ins JOIN unnest($11::text[], $12::uuid[]) AS e(dk, env_id) \
                 ON e.dk = ins.dedup_key \
             ON CONFLICT DO NOTHING \
             RETURNING queue_id \
         ) \
         SELECT count(*) AS n FROM ins",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(sub_ids)
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(project_ids)
    .bind::<diesel::sql_types::Array<Nullable<SqlUuid>>, _>(app_ids)
    .bind::<diesel::sql_types::Array<Bool>, _>(unattributed)
    .bind::<diesel::sql_types::Array<Text>, _>(kinds)
    .bind::<diesel::sql_types::Array<Text>, _>(dedups)
    .bind::<diesel::sql_types::Array<Text>, _>(severities)
    .bind::<diesel::sql_types::Array<Text>, _>(titles)
    .bind::<diesel::sql_types::Array<Text>, _>(bodies)
    .bind::<diesel::sql_types::Array<Nullable<Text>>, _>(links)
    .bind::<diesel::sql_types::Array<Text>, _>(env_keys)
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(env_ids)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// Durable throttle backstop: was a notification with this dedup key enqueued
/// for this subscription within the last `within_seconds`?
///
/// Used when Redis is unavailable. Extending the key with the subscription id
/// is what gives per-RECIPIENT throttling with no new infrastructure — the org
/// engine's equivalent (`alert_recently_sent`) is per rule.
pub async fn notification_recently_queued(
    conn: &mut AsyncPgConnection,
    subscription_id: Uuid,
    dedup_key: &str,
    within_seconds: i32,
) -> QueryResult<bool> {
    if within_seconds <= 0 {
        return Ok(false);
    }
    let cutoff = Utc::now() - chrono::Duration::seconds(within_seconds as i64);
    let n: i64 = notification_queue::table
        .filter(notification_queue::subscription_id.eq(subscription_id))
        .filter(notification_queue::dedup_key.eq(dedup_key))
        .filter(notification_queue::created_at.gt(cutoff))
        .count()
        .get_result(conn)
        .await?;
    Ok(n > 0)
}
```

- [ ] **Step 4: Run the four tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications`
  Expected: `the_live_dedup_index_suppresses_only_live_duplicates ... ok`, `deliver_after_defers_into_quiet_hours_and_survives_an_unknown_zone ... ok`, `the_quiet_hours_sql_and_the_rust_twin_agree_over_a_shared_table ... ok`, `notification_recently_queued_is_the_durable_throttle_backstop ... ok`.

- [ ] **Step 5: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 11: `repo.rs` — the claim, the terminal writes, requeue, prune, history

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs`
- Modify `backend/crates/sauron-db/tests/notifications.rs`

**Interfaces:**
- Consumes: `QueueInsert`, `enqueue_notifications` (Task 10); `models::NotificationQueueItem` (Task 1).
- Produces:
  - `pub const STUCK_CLAIM_SECS: i64 = 900;` and `pub const MAX_QUEUE_ATTEMPTS: i16 = 3;`
  - `pub async fn claim_due_notifications(conn, batch: i64) -> QueryResult<Vec<NotificationQueueItem>>`
  - `pub async fn mark_notifications_sent(conn, ids: &[Uuid], message_id: Uuid) -> QueryResult<usize>`
  - `pub async fn drop_notifications(conn, ids: &[Uuid], status: &str) -> QueryResult<usize>`
  - `pub async fn fail_notifications(conn, ids: &[Uuid], error: &str, max_attempts: i16) -> QueryResult<usize>` — back to `pending` while retries remain, `failed` + `finished_at` at the cap
  - `pub async fn requeue_stuck_notifications(conn, stale_secs: i64, max_attempts: i16) -> QueryResult<usize>`
  - `pub async fn prune_notification_queue(conn, retention_days: i32) -> QueryResult<usize>`
  - `pub async fn notification_queue_depth(conn) -> QueryResult<(i64, Option<DateTime<Utc>>)>`
  - `pub async fn queue_envs_for(conn, queue_ids: &[Uuid]) -> QueryResult<Vec<(Uuid, Uuid)>>`
  - `pub async fn project_org_batch(conn, project_ids: &[Uuid]) -> QueryResult<Vec<(Uuid, Uuid)>>`
  - `pub async fn grants_for_users_in_org(conn, user_ids: &[Uuid], org_id: Uuid) -> QueryResult<Vec<(Uuid, String, Uuid, Value)>>`
  - `pub async fn sent_messages_last_hour(conn, user_id: Uuid) -> QueryResult<i64>`
  - `pub async fn notification_history_for_user(conn, user_id: Uuid, limit: i64) -> QueryResult<Vec<NotificationQueueItem>>`

- [ ] **Step 1: Write the failing tests.** Append to `backend/crates/sauron-db/tests/notifications.rs`:

```rust
/// `FOR UPDATE SKIP LOCKED` alone only skips rows locked by an UNCOMMITTED
/// transaction; once replica A commits, replica B's next pass re-selects the
/// same rows and mails them again. A `claimed` status that leaves the partial
/// index is what makes the claim real — so the third pass, run after both
/// commit, is the assertion that matters.
#[tokio::test]
async fn claiming_is_exclusive_across_passes_not_just_across_transactions() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;

    let dedups: Vec<String> = (0..6).map(|i| format!("sub:{}:spike:{i}", sub.id)).collect();
    let rows: Vec<QueueInsert> = dedups
        .iter()
        .map(|d| QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_spike",
            dedup_key: d,
            severity: "warning",
            title: "t",
            body: "b",
            link: None,
            env_enrollments: vec![],
        })
        .collect();
    assert_eq!(
        sauron_db::repo::enqueue_notifications(&mut conn, &rows).await.unwrap(),
        6
    );

    let first = sauron_db::repo::claim_due_notifications(&mut conn, 4).await.unwrap();
    assert_eq!(first.len(), 4);
    assert!(first.iter().all(|r| r.status == "claimed" && r.attempts == 1));

    let second = sauron_db::repo::claim_due_notifications(&mut conn, 4).await.unwrap();
    assert_eq!(second.len(), 2, "the already-claimed rows are not re-selected");

    let mut all: Vec<uuid::Uuid> = first.iter().chain(second.iter()).map(|r| r.id).collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), 6, "disjoint claim sets");

    let third = sauron_db::repo::claim_due_notifications(&mut conn, 4).await.unwrap();
    assert!(third.is_empty(), "a committed claim is not re-claimable");

    db.cleanup().await;
}

/// A crash between claim and terminal status must be recoverable, not an
/// infinite redelivery loop — `attempts` is what makes the give-up decision
/// reachable.
#[tokio::test]
async fn stuck_claims_return_to_pending_then_fail_at_the_attempt_cap() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;
    let dedup = format!("sub:{}:spike:one", sub.id);
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_spike",
            dedup_key: &dedup,
            severity: "warning",
            title: "t",
            body: "b",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .unwrap();
    sauron_db::repo::claim_due_notifications(&mut conn, 10).await.unwrap();

    diesel::sql_query("UPDATE notification_queue SET claimed_at = now() - interval '20 minutes'")
        .execute(&mut conn)
        .await
        .unwrap();
    let n = sauron_db::repo::requeue_stuck_notifications(&mut conn, 900, 3).await.unwrap();
    assert_eq!(n, 1);

    let back = sauron_db::repo::claim_due_notifications(&mut conn, 10).await.unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].attempts, 2);

    diesel::sql_query(
        "UPDATE notification_queue SET attempts = 3, claimed_at = now() - interval '20 minutes'",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    sauron_db::repo::requeue_stuck_notifications(&mut conn, 900, 3).await.unwrap();

    #[derive(diesel::QueryableByName)]
    struct S {
        #[diesel(sql_type = diesel::sql_types::Text)]
        status: String,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        finished: bool,
    }
    let rows: Vec<S> = diesel::sql_query(
        "SELECT status, (finished_at IS NOT NULL) AS finished FROM notification_queue",
    )
    .load(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "failed");
    assert!(rows[0].finished);

    db.cleanup().await;
}

/// A row that fails DETERMINISTICALLY must still stop.
///
/// `fail_notifications` returns a row to `pending`, which
/// `requeue_stuck_notifications` can never see — it matches only
/// `status = 'claimed'`. So if `fail_notifications` did not apply the attempts
/// cap itself, a body that fails to render every single pass would be claimed,
/// failed and re-queued forever with nothing in the system able to break the
/// loop. This test drives exactly that: claim, fail, claim, fail, claim, fail.
#[tokio::test]
async fn a_deterministic_failure_stops_at_the_attempt_cap() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;
    let dedup = format!("sub:{}:spike:doomed", sub.id);
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_spike",
            dedup_key: &dedup,
            severity: "warning",
            title: "t",
            body: "b",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .unwrap();

    for expected_attempt in 1..=3i16 {
        let claimed = sauron_db::repo::claim_due_notifications(&mut conn, 10).await.unwrap();
        assert_eq!(claimed.len(), 1, "attempt {expected_attempt} should be claimable");
        assert_eq!(claimed[0].attempts, expected_attempt);
        let queue_ids: Vec<uuid::Uuid> = claimed.iter().map(|r| r.id).collect();
        sauron_db::repo::fail_notifications(&mut conn, &queue_ids, "render exploded", 3)
            .await
            .unwrap();
    }

    let after = sauron_db::repo::claim_due_notifications(&mut conn, 10).await.unwrap();
    assert!(
        after.is_empty(),
        "the third failure was terminal; a fourth claim means the cap is not applied"
    );

    #[derive(diesel::QueryableByName)]
    struct S {
        #[diesel(sql_type = diesel::sql_types::Text)]
        status: String,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        finished: bool,
    }
    let rows: Vec<S> = diesel::sql_query(
        "SELECT status, (finished_at IS NOT NULL) AS finished FROM notification_queue",
    )
    .load(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "failed");
    assert!(rows[0].finished, "a terminal row must carry finished_at or the prune never reaps it");

    db.cleanup().await;
}

/// Pruning on `created_at` with no status guard would destroy still-`pending`
/// rows — precisely the evidence of the outage that made them pile up.
#[tokio::test]
async fn the_prune_never_touches_pending_or_claimed_rows() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;
    let dedups: Vec<String> = (0..3).map(|i| format!("sub:{}:spike:{i}", sub.id)).collect();
    let rows: Vec<QueueInsert> = dedups
        .iter()
        .map(|d| QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_spike",
            dedup_key: d,
            severity: "warning",
            title: "t",
            body: "b",
            link: None,
            env_enrollments: vec![],
        })
        .collect();
    sauron_db::repo::enqueue_notifications(&mut conn, &rows).await.unwrap();

    // Age every row past any retention, then finish exactly one of them.
    diesel::sql_query("UPDATE notification_queue SET created_at = now() - interval '400 days'")
        .execute(&mut conn)
        .await
        .unwrap();
    diesel::sql_query(
        "UPDATE notification_queue SET status='sent', sent_at=now(), \
         finished_at = now() - interval '400 days' WHERE dedup_key = $1",
    )
    .bind::<diesel::sql_types::Text, _>(&dedups[0])
    .execute(&mut conn)
    .await
    .unwrap();

    let pruned = sauron_db::repo::prune_notification_queue(&mut conn, 14).await.unwrap();
    assert_eq!(pruned, 1, "only the finished row goes");

    let left: i64 = diesel::sql_query("SELECT count(*) AS n FROM notification_queue")
        .get_result::<sauron_db::repo::AlertCountRow>(&mut conn)
        .await
        .unwrap()
        .n;
    assert_eq!(left, 2);

    db.cleanup().await;
}
```

- [ ] **Step 2: Run them and watch them fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications`
  Expected: `cannot find function 'claim_due_notifications' in module 'sauron_db::repo'`.

- [ ] **Step 3: Implement.** Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// How long a `claimed` row may sit before the requeue reclaims it.
pub const STUCK_CLAIM_SECS: i64 = 900;
/// How many claims a row gets before it is abandoned as `failed`.
pub const MAX_QUEUE_ATTEMPTS: i16 = 3;

/// Claim due notifications for exclusive delivery.
///
/// The `status = 'claimed'` write is the entire point and is the one thing that
/// cannot be copied from `claim_due_monitors` without thinking. THAT query's
/// exclusivity comes from its SET clause — `next_check_at = now() + …` moves
/// the row out of the inner SELECT's predicate at commit. `FOR UPDATE SKIP
/// LOCKED` alone only skips rows locked by an UNCOMMITTED transaction; once one
/// replica commits, another replica's next pass re-selects the same rows and
/// mails them again. A `claimed` state that leaves the partial index is what
/// makes the claim real, and `attempts` is what makes a crash between claim and
/// terminal status recoverable instead of an infinite redelivery loop.
pub async fn claim_due_notifications(
    conn: &mut AsyncPgConnection,
    batch: i64,
) -> QueryResult<Vec<NotificationQueueItem>> {
    diesel::sql_query(
        "UPDATE notification_queue \
            SET status = 'claimed', claimed_at = now(), attempts = attempts + 1 \
          WHERE id IN ( \
              SELECT id FROM notification_queue \
               WHERE status = 'pending' AND deliver_after <= now() \
               ORDER BY deliver_after \
               FOR UPDATE SKIP LOCKED \
               LIMIT $1 \
          ) RETURNING *",
    )
    .bind::<BigInt, _>(batch.clamp(1, 5000))
    .load(conn)
    .await
}

/// Stamp one delivered message across every row it carried.
pub async fn mark_notifications_sent(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
    message_id: Uuid,
) -> QueryResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    diesel::sql_query(
        "UPDATE notification_queue \
            SET status = 'sent', message_id = $2, sent_at = now(), finished_at = now() \
          WHERE id = ANY($1)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(ids)
    .bind::<SqlUuid, _>(message_id)
    .execute(conn)
    .await
}

/// Terminally drop rows and BLANK their content in the same statement.
///
/// A dropped row's title/body/link have no further purpose and must not sit at
/// rest for the retention window outside the reader's authorization — which,
/// for `dropped_no_access`, is exactly the authorization that just failed.
pub async fn drop_notifications(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
    status: &str,
) -> QueryResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    diesel::sql_query(
        "UPDATE notification_queue \
            SET status = $2, title = NULL, body = NULL, link = NULL, finished_at = now() \
          WHERE id = ANY($1)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(ids)
    .bind::<Text, _>(status)
    .execute(conn)
    .await
}

/// Record a delivery failure without blanking the body, so a later requeue can
/// still send it — but only while there is a retry left.
///
/// The attempts guard is load-bearing and is the ONLY thing that terminates a
/// deterministic failure. `requeue_stuck_notifications` cannot help here: it
/// matches `WHERE status = 'claimed' AND claimed_at < …`, and a row this
/// function returns to `pending` is neither. A render that fails on its own
/// content — a `format!` that panics on a malformed body, an outbox that
/// rejects the row every time — would otherwise be re-claimed, re-failed and
/// re-queued forever, which is exactly the infinite redelivery loop
/// `MAX_QUEUE_ATTEMPTS` exists to stop.
///
/// `attempts` was already incremented by the claim, so `>= max_attempts` here
/// means "this was the last try".
pub async fn fail_notifications(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
    error: &str,
    max_attempts: i16,
) -> QueryResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    diesel::sql_query(
        "UPDATE notification_queue \
            SET status      = CASE WHEN attempts >= $3 THEN 'failed' ELSE 'pending' END, \
                finished_at = CASE WHEN attempts >= $3 THEN now() ELSE NULL END, \
                claimed_at  = NULL, \
                error       = $2 \
          WHERE id = ANY($1)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(ids)
    .bind::<Text, _>(error)
    .bind::<SmallInt, _>(max_attempts.max(1))
    .execute(conn)
    .await
}

/// Return abandoned `claimed` rows to `pending`, or give up on them.
///
/// There is no graceful shutdown anywhere in this codebase, so a process killed
/// mid-drain leaves rows `claimed` forever. `attempts >= max_attempts` is what
/// makes the give-up decision reachable rather than looping.
pub async fn requeue_stuck_notifications(
    conn: &mut AsyncPgConnection,
    stale_secs: i64,
    max_attempts: i16,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE notification_queue \
            SET status      = CASE WHEN attempts >= $2 THEN 'failed' ELSE 'pending' END, \
                finished_at = CASE WHEN attempts >= $2 THEN now() ELSE NULL END, \
                error       = CASE WHEN attempts >= $2 \
                                   THEN 'abandoned after repeated claims' ELSE error END, \
                claimed_at  = NULL \
          WHERE status = 'claimed' AND claimed_at < now() - make_interval(secs => $1)",
    )
    .bind::<BigInt, _>(stale_secs.max(60))
    .bind::<SmallInt, _>(max_attempts.max(1))
    .execute(conn)
    .await
}

/// Delete terminal rows past retention.
///
/// `alert_events` is append-only audit and prunes on `created_at`; this is a
/// WORK QUEUE. Pruning on `created_at` with no status guard would destroy
/// still-`pending` rows — precisely the evidence of the outage that made them
/// pile up — and none of the other indexes leads with `created_at`, so the
/// hourly DELETE would seq-scan a churned heap.
/// `notification_queue_finished_idx` serves this predicate directly.
pub async fn prune_notification_queue(
    conn: &mut AsyncPgConnection,
    retention_days: i32,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM notification_queue \
          WHERE finished_at IS NOT NULL \
            AND finished_at < now() - make_interval(days => $1)",
    )
    .bind::<Integer, _>(retention_days.clamp(1, 365))
    .execute(conn)
    .await
}

#[derive(Debug, QueryableByName)]
struct QueueDepthRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    oldest: Option<DateTime<Utc>>,
}

/// `(pending depth, oldest pending deliver_after)`.
///
/// Nothing else in the system would reveal a backlog: `status='sent'` means only
/// "handed to the outbox", so a stalled outbox and a healthy one look identical
/// from here.
pub async fn notification_queue_depth(
    conn: &mut AsyncPgConnection,
) -> QueryResult<(i64, Option<DateTime<Utc>>)> {
    let row: QueueDepthRow = diesel::sql_query(
        "SELECT count(*) AS n, min(deliver_after) AS oldest \
           FROM notification_queue WHERE status = 'pending'",
    )
    .get_result(conn)
    .await?;
    Ok((row.n, row.oldest))
}

/// `(queue_id, enrollment_environment_id)` for many queued rows at once.
pub async fn queue_envs_for(
    conn: &mut AsyncPgConnection,
    queue_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    if queue_ids.is_empty() {
        return Ok(Vec::new());
    }
    notification_queue_envs::table
        .filter(notification_queue_envs::queue_id.eq_any(queue_ids.to_vec()))
        .select((
            notification_queue_envs::queue_id,
            notification_queue_envs::environment_id,
        ))
        .load(conn)
        .await
}

/// `(project_id, org_id)` for many projects at once.
///
/// The drain re-derives every queued row's org from its project rather than
/// trusting the denormalized `notification_queue.org_id`. `reach_for`'s org arm
/// is `Scope::Org(_) => reach.org = true` and never compares the org id, so if a
/// row's stored `org_id` ever diverged from the true owner of its `project_id`,
/// `reach.org` would go true and the coverage test would accept a foreign
/// tenant's project. The column stays for indexing and the sweep; it is no
/// longer the tenant boundary.
pub async fn project_org_batch(
    conn: &mut AsyncPgConnection,
    project_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    projects::table
        .filter(projects::id.eq_any(project_ids.to_vec()))
        .select((projects::id, projects::org_id))
        .load(conn)
        .await
}

/// `(user_id, scope_type, scope_id, permissions)` for many users in ONE org.
///
/// The batched form of `user_grants_in_org`. Filtered to a single organization
/// for the reason `reach_for`'s doc comment records: its org arm does not
/// compare the grant's org id, so an unfiltered list would leak another org's
/// visibility.
pub async fn grants_for_users_in_org(
    conn: &mut AsyncPgConnection,
    user_ids: &[Uuid],
    org_id: Uuid,
) -> QueryResult<Vec<(Uuid, String, Uuid, Value)>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    role_grants::table
        .inner_join(roles::table.on(roles::id.eq(role_grants::role_id)))
        .filter(role_grants::user_id.eq_any(user_ids.to_vec()))
        .filter(role_grants::org_id.eq(org_id))
        .select((
            role_grants::user_id,
            role_grants::scope_type,
            role_grants::scope_id,
            roles::permissions,
        ))
        .load(conn)
        .await
}

/// How many distinct MESSAGES this user received in the trailing hour.
///
/// `COUNT(DISTINCT message_id)`, not a row count: one legitimate grouped email
/// carrying 25 issue rows would otherwise report 25 against a cap of 20 and
/// degrade the user to digests on their first normal delivery.
pub async fn sent_messages_last_hour(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<i64> {
    let row: AlertCountRow = diesel::sql_query(
        "SELECT count(DISTINCT message_id) AS n FROM notification_queue \
          WHERE user_id = $1 AND status = 'sent' AND sent_at > now() - interval '1 hour'",
    )
    .bind::<SqlUuid, _>(user_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// A user's own notification history, newest first.
///
/// Ownership alone is NOT a sufficient gate — the caller must still run the
/// coverage predicate against freshly loaded grants and drop non-covered rows,
/// because a row written with a title and body at enqueue time would otherwise
/// let a member whose grant was revoked read exactly the issue titles and counts
/// the drain refused to mail them.
pub async fn notification_history_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    limit: i64,
) -> QueryResult<Vec<NotificationQueueItem>> {
    notification_queue::table
        .filter(notification_queue::user_id.eq(user_id))
        .order(notification_queue::created_at.desc())
        .limit(limit.clamp(1, 200))
        .select(NotificationQueueItem::as_select())
        .load(conn)
        .await
}
```

Add `NotificationQueueItem` to the file's `use crate::models::{…}` list.

- [ ] **Step 4: Run all the DB tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications`
  Expected: all thirteen tests in the file pass.

- [ ] **Step 5: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 12: `ApiError::Unprocessable`, and the subscription list/create routes

**Files:**
- Modify `backend/bins/sauron-api/src/error.rs` (enum at line 11, `IntoResponse` match at 27)
- Create `backend/bins/sauron-api/src/routes/notification_prefs.rs`
- Modify `backend/bins/sauron-api/src/routes/mod.rs`
- Modify `backend/bins/sauron-api/src/main.rs` (route table around line 400)

**Interfaces:**
- Consumes: `SubKind`, `SubConditions` (Task 4); `covers`, `QueueTarget` (Task 6); `repo::{upsert_subscription, list_subscriptions_for_user, subscription_envs_for, live_catalogue_envs_for_project, live_enrollments_for_apps, timezone_exists}` (Tasks 2 and 9); `repo::{project_org, app_ancestry, user_grants_in_org, list_apps_for_project}`; `sauron_auth::rbac::{grants_from_rows, reach_for, perm}`; `crate::routes::db`; `super::scope::reject_environment_id` and `super::scope::RejectEnvQuery`.
- Produces:
  - `pub const MAX_SUBSCRIPTIONS_PER_USER: i64 = 50;`
  - `pub struct SubscriptionView { … }` (serialized response shape)
  - `pub async fn list_subscriptions(auth: AuthUser, State<AppState>, Query<RejectEnvQuery>) -> Result<Json<Vec<SubscriptionView>>, ApiError>`
  - `pub async fn create_subscription(auth: AuthUser, State<AppState>, Query<RejectEnvQuery>, Json<UpsertSubscriptionReq>) -> Result<Json<SubscriptionView>, ApiError>`
  - `async fn resolve_subscription_scope(conn: &mut AsyncPgConnection, scope_type: &str, scope_id: Uuid, kind: SubKind) -> Result<(Uuid, Uuid, Vec<Uuid>), ApiError>` — resolution only, no authorization; returns `(org_id, project_id, app_ids)`
  - `async fn authorize_subscription_scope(conn: &mut AsyncPgConnection, user_id: Uuid, org_id: Uuid, project_id: Uuid, kind: SubKind, app_ids: &[Uuid], env_enrollments: &[Uuid]) -> Result<(), ApiError>` — `env_enrollments` holds **enrollment** ids (`app_environments.id`), the id space `Reach.envs` is in; passing catalogue ids here matches nothing and fails silently. Both are private to `notification_prefs.rs`; Task 13's PATCH is in the same module.
  - `async fn enrollments_for(conn: &mut AsyncPgConnection, app_ids: &[Uuid], catalogue_envs: &[Uuid]) -> Result<Vec<Uuid>, ApiError>` — the catalogue→enrollment bridge

- [ ] **Step 1: Add the 422 variant.** In `backend/bins/sauron-api/src/error.rs`, add to the enum after `Conflict(String),`:

```rust
    /// Syntactically valid, semantically impossible. Used where a 400 would be
    /// misleading: the request parsed, the ids exist, and the operation is
    /// still refused — e.g. a project scope that resolves to zero apps, where
    /// the `for every app, covers()` test would otherwise succeed vacuously.
    Unprocessable(String),
```

and to the `IntoResponse` match after the `Conflict` arm:

```rust
            ApiError::Unprocessable(m) => {
                body(StatusCode::UNPROCESSABLE_ENTITY, "unprocessable", &m)
            }
```

- [ ] **Step 2: Write the failing test.** Create `backend/bins/sauron-api/src/routes/notification_prefs.rs` with only the module docs and a test module:

```rust
//! Personal notification subscriptions: `/v1/me/notification-subscriptions*`,
//! `/v1/me/notifications`, and the unauthenticated unsubscribe endpoint.
//!
//! `routes/account.rs` already owns `/v1/me/*` for sessions and profile; this
//! surface is large enough to justify its own module while sharing the
//! namespace.
//!
//! **There is no `org_id` field on any request body and there never will be
//! one.** The org is always re-derived from the scope itself, because
//! `reach_for`'s org arm sets `reach.org = true` without comparing the org id —
//! a caller-supplied org would be a cross-tenant escalation.
//!
//! None of these routes is added to the password-change allowlist in
//! `sauron-auth`'s `extractors.rs`: a temp-password holder must not reach them.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subscription_cap_is_fifty() {
        assert_eq!(MAX_SUBSCRIPTIONS_PER_USER, 50);
    }

    #[test]
    fn quiet_hours_must_be_supplied_as_a_pair() {
        // Mirrors the table CHECK: `(quiet_start_min IS NULL) = (quiet_end_min IS NULL)`.
        assert!(validate_quiet(None, None).is_ok());
        assert!(validate_quiet(Some(1320), Some(360)).is_ok());
        assert!(validate_quiet(Some(1320), None).is_err());
        assert!(validate_quiet(None, Some(360)).is_err());
        assert!(validate_quiet(Some(1440), Some(360)).is_err(), "out of range");
        assert!(validate_quiet(Some(-1), Some(360)).is_err(), "out of range");
    }

    #[test]
    fn uptime_refuses_app_scope() {
        assert!(validate_scope_kind("project", SubKind::Uptime).is_ok());
        assert!(validate_scope_kind("app", SubKind::Uptime).is_err());
        assert!(validate_scope_kind("app", SubKind::ErrorSpike).is_ok());
        assert!(validate_scope_kind("org", SubKind::ErrorSpike).is_err());
    }
}
```

- [ ] **Step 3: Register the module and run the test.** Add `pub mod notification_prefs;` to `backend/bins/sauron-api/src/routes/mod.rs` beside the other `pub mod` lines, then run
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api notification_prefs`
  Expected: `error[E0425]: cannot find value 'MAX_SUBSCRIPTIONS_PER_USER'` and `cannot find function 'validate_quiet'`.

- [ ] **Step 4: Write the module's imports, constants and pure validators.** Insert into `notification_prefs.rs` between the module docs and `#[cfg(test)]`:

```rust
use axum::extract::{Path, Query, State};
use axum::Json;
use sauron_alerts::subscription::{covers, QueueTarget, SubConditions, SubKind};
use sauron_auth::rbac::{grants_from_rows, perm, reach_for};
use sauron_auth::AuthUser;
use sauron_db::{repo, AsyncPgConnection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::error::ApiError;
use crate::routes::db;
use crate::AppState;

/// A compile-time ceiling, enforced at write time with a 409.
///
/// A guess, not a measurement: the per-org probe ceiling should be re-derived
/// from measured probe latency once there is data.
pub const MAX_SUBSCRIPTIONS_PER_USER: i64 = 50;

/// Mirrors the table CHECK `(quiet_start_min IS NULL) = (quiet_end_min IS NULL)`
/// so a half-specified window is a 400 with a readable message rather than a
/// constraint violation surfacing as a 500.
fn validate_quiet(start: Option<i16>, end: Option<i16>) -> Result<(), ApiError> {
    match (start, end) {
        (None, None) => Ok(()),
        (Some(s), Some(e)) => {
            if !(0..=1439).contains(&s) || !(0..=1439).contains(&e) {
                Err(ApiError::BadRequest(
                    "quiet_start_min and quiet_end_min must be minutes of day (0-1439)".into(),
                ))
            } else {
                Ok(())
            }
        }
        _ => Err(ApiError::BadRequest(
            "quiet_start_min and quiet_end_min must both be set or both be omitted".into(),
        )),
    }
}

/// `monitors` carries only `project_id`, so an app-scoped uptime subscription
/// could never fire. Refusing it is better than accepting one that is silently
/// inert.
fn validate_scope_kind(scope_type: &str, kind: SubKind) -> Result<(), ApiError> {
    match scope_type {
        "project" => Ok(()),
        "app" if kind.allows_app_scope() => Ok(()),
        "app" => Err(ApiError::BadRequest(
            "uptime subscriptions are project-scoped: monitors have no app dimension".into(),
        )),
        _ => Err(ApiError::BadRequest(
            "scope_type must be 'project' or 'app'".into(),
        )),
    }
}
```

- [ ] **Step 5: Run the unit tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api notification_prefs`
  Expected: 3 passing tests.

- [ ] **Step 6: Add the shared resolution and authorization helpers.** Append to `notification_prefs.rs` above `#[cfg(test)]`.

  **These are two functions, not one, and that split is the whole point.** The
  coverage test depends on the subscription's resolved enrollments, and the
  enrollments cannot be resolved until the scope has been resolved to its app
  set. A single function forces the caller to authorize twice — once with an
  empty enrollment list to learn `app_ids`, then again with the real list — and
  the first of those calls is fatal: `covers()` arm 5 is
  `!includes_unattributed && !env_enrollments.is_empty() && …`, so with `&[]`
  an env-scoped member is refused before the narrowed call is ever reached. No
  env-scoped member could create an env-narrowed subscription at all. Design
  §8.2 step 5 requires exactly one `covers()` pass, "with the subscription's
  resolved enrollments".

```rust
/// Resolve a scope to `(org_id, project_id, app_ids)`. **No authorization
/// happens here** — see [`authorize_subscription_scope`].
///
/// 1. The org comes from the SCOPE, never from the request body.
/// 2. A project scope resolving to ZERO apps is a 422, not a success. The
///    error-kind check is "covers() holds for every app in scope", and over an
///    empty set that is vacuously true — which would let any org member
///    subscribe to anything in a project that has no apps yet.
/// 3. Uptime resolves to an EMPTY app set on purpose: `monitors` carries only
///    `project_id`, so there is no app dimension to expand, and the 422 above
///    must not fire for a project whose monitors exist but whose apps do not.
async fn resolve_subscription_scope(
    conn: &mut AsyncPgConnection,
    scope_type: &str,
    scope_id: Uuid,
    kind: SubKind,
) -> Result<(Uuid, Uuid, Vec<Uuid>), ApiError> {
    let (project_id, org_id) = match scope_type {
        "project" => {
            let org = repo::project_org(conn, scope_id)
                .await?
                .ok_or(ApiError::NotFound)?;
            (scope_id, org)
        }
        "app" => repo::app_ancestry(conn, scope_id)
            .await?
            .ok_or(ApiError::NotFound)?,
        _ => {
            return Err(ApiError::BadRequest(
                "scope_type must be 'project' or 'app'".into(),
            ))
        }
    };

    if kind == SubKind::Uptime {
        return Ok((org_id, project_id, Vec::new()));
    }

    let app_ids: Vec<Uuid> = if scope_type == "app" {
        vec![scope_id]
    } else {
        repo::list_apps_for_project(conn, project_id)
            .await?
            .into_iter()
            .map(|a| a.id)
            .collect()
    };
    if app_ids.is_empty() {
        return Err(ApiError::Unprocessable(
            "this project has no apps yet, so there is nothing to subscribe to".into(),
        ));
    }
    Ok((org_id, project_id, app_ids))
}

/// Authorize an already-resolved scope. Called EXACTLY ONCE per write, with the
/// enrollment ids the subscription will actually carry.
///
/// Order matters and each step is load-bearing:
/// 1. No grants at all in that org is a 403 (non-membership), decided before
///    any permission arithmetic.
/// 2. Uptime is accepted only on org or project reach — every monitor read in
///    the product resolves with `app: None, env: None`, so an app- or
///    env-scoped member gets 403 from the monitor API and must not be able to
///    subscribe around it.
/// 3. Error kinds require `covers()` for every app in `app_ids`.
///
/// `env_enrollments` holds **enrollment** ids (`app_environments.id`) — the id
/// space `Reach.envs` is in. A catalogue id passed here matches nothing and the
/// failure is silent at every layer, which is the trap this whole slice exists
/// to avoid. Use [`enrollments_for`] to cross the two spaces.
///
/// An EMPTY `env_enrollments` means "this subscription does not narrow by
/// environment", which needs app-level reach — never pass `&[]` as a
/// placeholder to discover something else.
async fn authorize_subscription_scope(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
    project_id: Uuid,
    kind: SubKind,
    app_ids: &[Uuid],
    env_enrollments: &[Uuid],
) -> Result<(), ApiError> {
    let rows = repo::user_grants_in_org(conn, user_id, org_id).await?;
    if rows.is_empty() {
        return Err(ApiError::Forbidden(
            "you are not a member of the organization that owns this scope".into(),
        ));
    }
    let grants = grants_from_rows(rows);
    let reach = reach_for(&grants, kind.permission());

    if kind == SubKind::Uptime {
        if reach.org || reach.projects.contains(&project_id) {
            return Ok(());
        }
        return Err(ApiError::Forbidden(format!(
            "you cannot read monitors for project {project_id}"
        )));
    }

    for app_id in app_ids {
        let target = QueueTarget {
            project_id,
            app_id: Some(*app_id),
            env_enrollments,
            includes_unattributed: env_enrollments.is_empty(),
        };
        if !covers(&reach, &target) {
            return Err(ApiError::Forbidden(format!(
                "you cannot read issues for app {app_id} in the scope you selected"
            )));
        }
    }
    Ok(())
}

/// Resolve a subscription's CATALOGUE environment ids to the ENROLLMENT ids the
/// coverage predicate compares against `Reach.envs`.
///
/// The two id spaces are disjoint; comparing a catalogue id against
/// `Reach.envs` would silently never match and the subscriber would be refused
/// with no explanation.
async fn enrollments_for(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    catalogue_envs: &[Uuid],
) -> Result<Vec<Uuid>, ApiError> {
    if catalogue_envs.is_empty() {
        return Ok(Vec::new());
    }
    Ok(repo::live_enrollments_for_apps(conn, app_ids)
        .await?
        .into_iter()
        .filter(|(_, _, catalogue)| catalogue_envs.contains(catalogue))
        .map(|(enrollment, _, _)| enrollment)
        .collect())
}
```

- [ ] **Step 7: Add the request/response shapes and the two handlers.** Append above `#[cfg(test)]`:

```rust
#[derive(Debug, Deserialize)]
pub struct UpsertSubscriptionReq {
    pub scope_type: String,
    pub scope_id: Uuid,
    pub kind: String,
    /// CATALOGUE environment ids. `[]` means all environments, including
    /// unattributed events.
    #[serde(default)]
    pub environment_ids: Vec<Uuid>,
    #[serde(default)]
    pub conditions: Value,
    #[serde(default = "default_delivery")]
    pub delivery: String,
    #[serde(default = "default_throttle")]
    pub throttle_seconds: i32,
    pub quiet_start_min: Option<i16>,
    pub quiet_end_min: Option<i16>,
    #[serde(default = "default_tz")]
    pub quiet_tz: String,
}

fn default_delivery() -> String {
    "immediate".to_string()
}
fn default_throttle() -> i32 {
    900
}
fn default_tz() -> String {
    "UTC".to_string()
}

/// The row plus everything the card needs, joined on read. The environment list
/// and the best-effort scope name live here rather than on the row struct.
#[derive(Debug, Serialize)]
pub struct SubscriptionView {
    pub id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
    /// Best effort: `scope_id` has no FK, so a row can outlive its target.
    pub scope_name: Option<String>,
    pub project_id: Option<Uuid>,
    pub kind: String,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    pub environment_ids: Vec<Uuid>,
    pub conditions: Value,
    pub delivery: String,
    /// What delivery the user will ACTUALLY get. The per-user hourly cap
    /// degrades to digests, and a user permanently over it would otherwise
    /// never learn that their configured `immediate` is not what happens.
    pub effective_delivery: String,
    pub throttle_seconds: i32,
    pub quiet_start_min: Option<i16>,
    pub quiet_end_min: Option<i16>,
    pub quiet_tz: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

async fn views_for(
    conn: &mut AsyncPgConnection,
    subs: Vec<sauron_db::models::NotificationSubscription>,
    over_cap: bool,
) -> Result<Vec<SubscriptionView>, ApiError> {
    let ids: Vec<Uuid> = subs.iter().map(|s| s.id).collect();
    let env_rows = repo::subscription_envs_for(conn, &ids).await?;

    let project_scopes: Vec<Uuid> = subs
        .iter()
        .filter(|s| s.scope_type == "project")
        .map(|s| s.scope_id)
        .collect();
    let app_scopes: Vec<Uuid> = subs
        .iter()
        .filter(|s| s.scope_type == "app")
        .map(|s| s.scope_id)
        .collect();
    let projects = repo::list_projects_by_ids(conn, &project_scopes).await?;
    let apps = repo::apps_by_ids(conn, &app_scopes).await?;

    Ok(subs
        .into_iter()
        .map(|s| {
            let environment_ids = env_rows
                .iter()
                .filter(|(sid, _)| *sid == s.id)
                .map(|(_, e)| *e)
                .collect();
            let (scope_name, project_id) = if s.scope_type == "project" {
                (
                    projects.iter().find(|p| p.id == s.scope_id).map(|p| p.name.clone()),
                    Some(s.scope_id),
                )
            } else {
                let app = apps.iter().find(|a| a.id == s.scope_id);
                (app.map(|a| a.name.clone()), app.map(|a| a.project_id))
            };
            let effective_delivery = if over_cap && s.delivery == "immediate" {
                "hourly".to_string()
            } else {
                s.delivery.clone()
            };
            SubscriptionView {
                id: s.id,
                scope_type: s.scope_type,
                scope_id: s.scope_id,
                scope_name,
                project_id,
                kind: s.kind,
                enabled: s.enabled,
                disabled_reason: s.disabled_reason,
                environment_ids,
                conditions: s.conditions,
                delivery: s.delivery,
                effective_delivery,
                throttle_seconds: s.throttle_seconds,
                quiet_start_min: s.quiet_start_min,
                quiet_end_min: s.quiet_end_min,
                quiet_tz: s.quiet_tz,
                created_at: s.created_at,
            }
        })
        .collect())
}

pub async fn list_subscriptions(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Vec<SubscriptionView>>, ApiError> {
    // Not an `/v1/apps/{id}/…` route, so the dashboard interceptor never adds
    // the parameter — but silently ignoring an unsupported query parameter is
    // treated as a bug in this codebase even on a static endpoint.
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    let subs = repo::list_subscriptions_for_user(&mut conn, auth.user_id).await?;
    let sent = repo::sent_messages_last_hour(&mut conn, auth.user_id).await?;
    let over_cap = sent >= state.cfg.notify_max_emails_per_user_per_hour.clamp(1, 1000);
    let views = views_for(&mut conn, subs, over_cap).await?;
    Ok(Json(views))
}

pub async fn create_subscription(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<UpsertSubscriptionReq>,
) -> Result<Json<SubscriptionView>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let kind = SubKind::parse(&req.kind)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown kind '{}'", req.kind)))?;
    validate_scope_kind(&req.scope_type, kind)?;
    validate_quiet(req.quiet_start_min, req.quiet_end_min)?;

    let mut conn = db(&state).await?;

    if !repo::timezone_exists(&mut conn, &req.quiet_tz).await? {
        return Err(ApiError::BadRequest(format!(
            "'{}' is not a timezone this server knows",
            req.quiet_tz
        )));
    }

    // Uptime ignores the environment filter entirely, so accepting a set for it
    // would store something that silently does nothing.
    let catalogue_envs: Vec<Uuid> = if kind.supports_env_filter() {
        req.environment_ids.clone()
    } else {
        Vec::new()
    };

    // Resolve the scope FIRST so environment validation and the coverage test
    // both run against a project we have already confirmed exists. Resolution
    // does NOT authorize — see the two-function split in Step 6.
    let (org_id, project_id, app_ids) =
        resolve_subscription_scope(&mut conn, &req.scope_type, req.scope_id, kind).await?;

    // Validate the submitted catalogue ids and cross them into enrollment ids
    // BEFORE authorizing, because the coverage test is decided against the
    // enrollments this subscription will actually carry.
    let enrollments: Vec<Uuid> = if catalogue_envs.is_empty() {
        Vec::new()
    } else {
        let live = repo::live_catalogue_envs_for_project(&mut conn, project_id).await?;
        // Catches the commonest paste error by far: an ENROLLMENT id copied out
        // of a dashboard URL into a field that wants a catalogue id.
        if let Some(bad) = catalogue_envs.iter().find(|e| !live.contains(e)) {
            return Err(ApiError::BadRequest(format!(
                "{bad} is not a live environment of this project"
            )));
        }
        enrollments_for(&mut conn, &app_ids, &catalogue_envs).await?
    };

    // ONE authorization pass. A second pass with `&[]` would refuse every
    // env-scoped member before this one ever ran.
    //
    // If the selected catalogue environments resolve to zero enrollments (the
    // apps in scope are not enrolled in any of them), `enrollments` is empty and
    // this degrades to the unnarrowed, app-level test — the fail-closed
    // direction, and the subscription would have matched nothing anyway.
    authorize_subscription_scope(
        &mut conn,
        auth.user_id,
        org_id,
        project_id,
        kind,
        &app_ids,
        &enrollments,
    )
    .await?;

    // Counted before the write, and the upsert may be an update — so an
    // existing subscriber editing their 50th is not refused. The rows are
    // fetched rather than counted because `is_update` needs them anyway; a
    // separate `COUNT(*)` would be a second round trip for the same answer.
    let existing = repo::list_subscriptions_for_user(&mut conn, auth.user_id).await?;
    let is_update = existing.iter().any(|s| {
        s.scope_type == req.scope_type && s.scope_id == req.scope_id && s.kind == req.kind
    });
    if !is_update && existing.len() as i64 >= MAX_SUBSCRIPTIONS_PER_USER {
        return Err(ApiError::Conflict(format!(
            "you already have {MAX_SUBSCRIPTIONS_PER_USER} subscriptions; delete one first"
        )));
    }

    let cond = SubConditions::from_value(kind, &req.conditions);
    let stored = cond.to_value(kind);
    let sub = repo::upsert_subscription(
        &mut conn,
        auth.user_id,
        org_id,
        &req.scope_type,
        req.scope_id,
        kind.as_str(),
        &stored,
        match req.delivery.as_str() {
            "hourly" => "hourly",
            "daily" => "daily",
            _ => "immediate",
        },
        req.throttle_seconds.clamp(0, 604_800),
        req.quiet_start_min,
        req.quiet_end_min,
        &req.quiet_tz,
        &catalogue_envs,
    )
    .await?;

    let sent = repo::sent_messages_last_hour(&mut conn, auth.user_id).await?;
    let over_cap = sent >= state.cfg.notify_max_emails_per_user_per_hour.clamp(1, 1000);
    let mut views = views_for(&mut conn, vec![sub], over_cap).await?;
    Ok(Json(views.remove(0)))
}
```

- [ ] **Step 8: Add the two supporting repo functions the view needs.** Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// Projects by id, unfiltered by org — a best-effort display lookup for
/// polymorphic `scope_id`s the caller has already authorized.
pub async fn list_projects_by_ids(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
) -> QueryResult<Vec<Project>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    projects::table
        .filter(projects::id.eq_any(ids.to_vec()))
        .select(Project::as_select())
        .load(conn)
        .await
}

/// Apps by id — the display counterpart to [`list_projects_by_ids`].
pub async fn apps_by_ids(conn: &mut AsyncPgConnection, ids: &[Uuid]) -> QueryResult<Vec<App>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    apps::table
        .filter(apps::id.eq_any(ids.to_vec()))
        .select(App::as_select())
        .load(conn)
        .await
}
```

- [ ] **Step 9: Register the two routes.** In `backend/bins/sauron-api/src/main.rs`, beside the `/v1/alert-meta` line:

```rust
        .route(
            "/v1/me/notification-subscriptions",
            get(routes::notification_prefs::list_subscriptions)
                .post(routes::notification_prefs::create_subscription),
        )
```

- [ ] **Step 10: Check the workspace and run the module's tests.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets && cargo test -p sauron-api notification_prefs`
  Expected: compiles; 3 tests pass.

- [ ] **Step 11: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 13: PATCH, DELETE and the notification history endpoint

**Files:**
- Modify `backend/bins/sauron-api/src/routes/notification_prefs.rs`
- Modify `backend/bins/sauron-api/src/main.rs`

**Interfaces:**
- Consumes: `SubscriptionView`, `views_for`, `resolve_subscription_scope`, `authorize_subscription_scope`, `enrollments_for`, `validate_quiet` (Task 12); `repo::{get_subscription, set_subscription_enabled, delete_subscription, notification_history_for_user, queue_envs_for, project_org_batch, grants_for_users_in_org}` (Tasks 9 and 11).
- Produces:
  - `pub async fn patch_subscription(auth, State, Path<Uuid>, Query<RejectEnvQuery>, Json<PatchSubscriptionReq>) -> Result<Json<SubscriptionView>, ApiError>`
  - `pub async fn delete_subscription_route(auth, State, Path<Uuid>, Query<RejectEnvQuery>) -> Result<Json<Value>, ApiError>`
  - `pub async fn list_notifications(auth, State, Query<HistoryQuery>) -> Result<Json<Vec<NotificationView>>, ApiError>`

- [ ] **Step 1: Write the failing test.** Append inside `mod tests` in `notification_prefs.rs`:

```rust
    #[test]
    fn history_limit_is_clamped_not_trusted() {
        assert_eq!(history_limit(None), 50);
        assert_eq!(history_limit(Some(0)), 1);
        assert_eq!(history_limit(Some(10)), 10);
        assert_eq!(history_limit(Some(100_000)), 200);
        assert_eq!(history_limit(Some(-5)), 1);
    }
```

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api notification_prefs`
  Expected: `error[E0425]: cannot find function 'history_limit'`.

- [ ] **Step 3: Implement the three handlers.** Append to `notification_prefs.rs` above `#[cfg(test)]`:

```rust
#[derive(Debug, Deserialize)]
pub struct PatchSubscriptionReq {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub environment_ids: Option<Vec<Uuid>>,
    pub conditions: Option<Value>,
    pub delivery: Option<String>,
    pub throttle_seconds: Option<i32>,
    /// A field present with a `null` value clears the window; absent leaves it.
    #[serde(default, deserialize_with = "double_option")]
    pub quiet_start_min: Option<Option<i16>>,
    #[serde(default, deserialize_with = "double_option")]
    pub quiet_end_min: Option<Option<i16>>,
    pub quiet_tz: Option<String>,
}

/// Distinguishes "absent" from "present and null" — without it, clearing a
/// quiet-hours window is indistinguishable from leaving it alone.
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

pub async fn patch_subscription(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<PatchSubscriptionReq>,
) -> Result<Json<SubscriptionView>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;

    // 404, never 403: confirming that someone else's subscription exists is
    // itself a disclosure.
    let existing = repo::get_subscription(&mut conn, id)
        .await?
        .filter(|s| s.user_id == auth.user_id)
        .ok_or(ApiError::NotFound)?;

    let kind = SubKind::parse(&existing.kind).ok_or(ApiError::NotFound)?;

    let quiet_start = req.quiet_start_min.unwrap_or(existing.quiet_start_min);
    let quiet_end = req.quiet_end_min.unwrap_or(existing.quiet_end_min);
    validate_quiet(quiet_start, quiet_end)?;

    let quiet_tz = req.quiet_tz.clone().unwrap_or(existing.quiet_tz.clone());
    if !repo::timezone_exists(&mut conn, &quiet_tz).await? {
        return Err(ApiError::BadRequest(format!(
            "'{quiet_tz}' is not a timezone this server knows"
        )));
    }

    // An enable/disable-only PATCH must not silently re-run the write-time
    // authorization, because a member who legitimately lost reach should still
    // be able to turn their own stale subscription off.
    if let Some(enabled) = req.enabled {
        if !enabled {
            repo::set_subscription_enabled(&mut conn, id, auth.user_id, false).await?;
            let sub = repo::get_subscription(&mut conn, id).await?.ok_or(ApiError::NotFound)?;
            let mut views = views_for(&mut conn, vec![sub], false).await?;
            return Ok(Json(views.remove(0)));
        }
    }

    let catalogue_envs: Vec<Uuid> = if kind.supports_env_filter() {
        match &req.environment_ids {
            Some(v) => v.clone(),
            None => repo::subscription_envs_for(&mut conn, &[id])
                .await?
                .into_iter()
                .map(|(_, e)| e)
                .collect(),
        }
    } else {
        Vec::new()
    };

    // Same two-phase shape as `create_subscription`: resolve, cross the env id
    // spaces, then authorize ONCE against the resolved enrollments. Calling the
    // authorizer with `&[]` first to learn `app_ids` would 403 every env-scoped
    // member before the narrowed call could run.
    let (org_id, project_id, app_ids) =
        resolve_subscription_scope(&mut conn, &existing.scope_type, existing.scope_id, kind)
            .await?;

    let enrollments: Vec<Uuid> = if catalogue_envs.is_empty() {
        Vec::new()
    } else {
        let live = repo::live_catalogue_envs_for_project(&mut conn, project_id).await?;
        if let Some(bad) = catalogue_envs.iter().find(|e| !live.contains(e)) {
            return Err(ApiError::BadRequest(format!(
                "{bad} is not a live environment of this project"
            )));
        }
        enrollments_for(&mut conn, &app_ids, &catalogue_envs).await?
    };

    authorize_subscription_scope(
        &mut conn,
        auth.user_id,
        org_id,
        project_id,
        kind,
        &app_ids,
        &enrollments,
    )
    .await?;

    let cond = SubConditions::from_value(
        kind,
        req.conditions.as_ref().unwrap_or(&existing.conditions),
    );
    let sub = repo::upsert_subscription(
        &mut conn,
        auth.user_id,
        org_id,
        &existing.scope_type,
        existing.scope_id,
        kind.as_str(),
        &cond.to_value(kind),
        match req.delivery.as_deref().unwrap_or(&existing.delivery) {
            "hourly" => "hourly",
            "daily" => "daily",
            _ => "immediate",
        },
        req.throttle_seconds
            .unwrap_or(existing.throttle_seconds)
            .clamp(0, 604_800),
        quiet_start,
        quiet_end,
        &quiet_tz,
        &catalogue_envs,
    )
    .await?;

    let sent = repo::sent_messages_last_hour(&mut conn, auth.user_id).await?;
    let over_cap = sent >= state.cfg.notify_max_emails_per_user_per_hour.clamp(1, 1000);
    let mut views = views_for(&mut conn, vec![sub], over_cap).await?;
    Ok(Json(views.remove(0)))
}

pub async fn delete_subscription_route(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    let n = repo::delete_subscription(&mut conn, id, auth.user_id).await?;
    if n == 0 {
        // 404 rather than 403 — do not confirm that the id exists.
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub limit: Option<i64>,
    pub environment_id: Option<String>,
}

fn history_limit(raw: Option<i64>) -> i64 {
    raw.unwrap_or(50).clamp(1, 200)
}

#[derive(Debug, Serialize)]
pub struct NotificationView {
    pub id: Uuid,
    pub kind: String,
    pub severity: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub link: Option<String>,
    pub status: String,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub sent_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A user's own notification history.
///
/// Ownership alone is NOT a sufficient gate. Each row was written with a title
/// and body at enqueue time, so a member whose grant was revoked afterwards
/// would otherwise authenticate here and read exactly the issue titles and
/// counts the drain refused to mail them. Blanking on `dropped_no_access`
/// covers the rows the drain caught; this filter covers the rows whose access
/// changed after they were already sent.
pub async fn list_notifications(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<NotificationView>>, ApiError> {
    super::scope::reject_environment_id(q.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    let rows =
        repo::notification_history_for_user(&mut conn, auth.user_id, history_limit(q.limit))
            .await?;
    if rows.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let queue_ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let env_rows = repo::queue_envs_for(&mut conn, &queue_ids).await?;
    let project_ids: Vec<Uuid> = rows.iter().map(|r| r.project_id).collect();
    let orgs = repo::project_org_batch(&mut conn, &project_ids).await?;

    // One grant load per distinct org, never per row.
    let mut org_ids: Vec<Uuid> = orgs.iter().map(|(_, o)| *o).collect();
    org_ids.sort_unstable();
    org_ids.dedup();
    let mut reaches: Vec<(Uuid, sauron_auth::rbac::Reach, sauron_auth::rbac::Reach)> = Vec::new();
    for org_id in org_ids {
        let grants =
            grants_from_rows(repo::user_grants_in_org(&mut conn, auth.user_id, org_id).await?);
        reaches.push((
            org_id,
            reach_for(&grants, perm::ISSUE_READ),
            reach_for(&grants, perm::MONITOR_READ),
        ));
    }
    drop(conn);

    let out = rows
        .into_iter()
        .filter(|r| {
            let Some((_, org_id)) = orgs.iter().find(|(p, _)| *p == r.project_id) else {
                return false;
            };
            let Some((_, issue_reach, monitor_reach)) =
                reaches.iter().find(|(o, _, _)| o == org_id)
            else {
                return false;
            };
            let envs: Vec<Uuid> = env_rows
                .iter()
                .filter(|(q, _)| *q == r.id)
                .map(|(_, e)| *e)
                .collect();
            let reach = if r.kind == "uptime" { monitor_reach } else { issue_reach };
            covers(
                reach,
                &QueueTarget {
                    project_id: r.project_id,
                    app_id: r.app_id,
                    env_enrollments: &envs,
                    includes_unattributed: r.includes_unattributed,
                },
            )
        })
        .map(|r| NotificationView {
            id: r.id,
            kind: r.kind,
            severity: r.severity,
            title: r.title,
            body: r.body,
            link: r.link,
            status: r.status,
            occurred_at: r.occurred_at,
            sent_at: r.sent_at,
        })
        .collect();
    Ok(Json(out))
}
```

- [ ] **Step 4: Register the routes.** In `backend/bins/sauron-api/src/main.rs`, beside the route added in Task 12:

```rust
        .route(
            "/v1/me/notification-subscriptions/{id}",
            patch(routes::notification_prefs::patch_subscription)
                .delete(routes::notification_prefs::delete_subscription_route),
        )
        .route(
            "/v1/me/notifications",
            get(routes::notification_prefs::list_notifications),
        )
```

Add `patch` to the `use axum::routing::{…}` list in `main.rs` if it is not already imported.

- [ ] **Step 5: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api notification_prefs`
  Expected: 4 passing tests.

- [ ] **Step 6: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 14: The unauthenticated unsubscribe endpoint

**Files:**
- Modify `backend/bins/sauron-api/src/routes/notification_prefs.rs`
- Modify `backend/bins/sauron-api/src/main.rs`
- Modify `backend/bins/sauron-api/Cargo.toml` (add `sauron-mail.workspace = true` if S0 did not already)

**Interfaces:**
- Consumes: `sauron_alerts::crypto::{derive_unsub_key, verify_unsubscribe_token, days_since_epoch}` (Task 7); `super::auth::{rate_limit, client_addr}` (S2); `repo::{get_subscription, disable_subscription, find_user_by_id, enqueue_mail}`; `sauron_mail::{MailKind, MailContent, Branding, render}`.
- Produces: `pub async fn unsubscribe(State<AppState>, headers: HeaderMap, ConnectInfo<SocketAddr>, Json<UnsubscribeReq>) -> Result<Json<Value>, ApiError>`

- [ ] **Step 1: Write the failing test.** Append inside `mod tests` in `notification_prefs.rs`:

```rust
    #[test]
    fn the_unsubscribe_response_is_identical_whatever_happened() {
        // A caller must not be able to distinguish "token valid" from "token
        // forged" from "subscription already disabled". The body is a constant.
        assert_eq!(
            serde_json::to_string(&generic_unsubscribe_ok()).unwrap(),
            r#"{"ok":true}"#
        );
    }
```

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api notification_prefs`
  Expected: `error[E0425]: cannot find function 'generic_unsubscribe_ok'`.

- [ ] **Step 3: Implement.** Append to `notification_prefs.rs` above `#[cfg(test)]`:

```rust
#[derive(Debug, Deserialize)]
pub struct UnsubscribeReq {
    pub token: String,
}

/// The one and only unsubscribe response body.
///
/// Returned whether the token verified, was forged, named a subscription that
/// no longer exists, or was already disabled — anything else turns this
/// endpoint into an oracle for which subscription ids exist.
fn generic_unsubscribe_ok() -> Value {
    serde_json::json!({ "ok": true })
}

/// The signing key for unsubscribe tokens.
///
/// Derived, never raw. `NOTIFY_SECRET_KEY` is the AES-GCM key that encrypts
/// stored channel secrets, so "rotate it to invalidate outstanding links" is
/// not an available mitigation: rotating it makes every stored Slack webhook
/// URL and SMTP password undecryptable. Domain separation keeps the two uses
/// independent.
///
/// The fallback is `require_jwt_secret()`, not the field: `Config::jwt_secret`
/// is private on purpose (`sauron-core/src/config.rs:20` — "reach it through
/// `Config::require_jwt_secret`"), so touching it directly is E0616 from
/// `sauron-api`.
///
/// **This expression must stay byte-for-byte identical to `unsub_key` in
/// `sauron-alerts`' drain (Task 18 Step 5).** The drain mints the tokens and
/// this endpoint verifies them, in two different processes; if the two
/// derivations ever diverge, every link fails verification and — because this
/// endpoint deliberately returns the same body whatever happened — every
/// unsubscribe silently no-ops with no error anywhere.
pub(crate) fn unsub_signing_key(state: &AppState) -> String {
    let base = state.cfg.notify_secret_key.clone().unwrap_or_else(|| {
        state
            .cfg
            .require_jwt_secret()
            .map(String::from)
            .unwrap_or_default()
    });
    sauron_alerts::crypto::derive_unsub_key(base.as_bytes())
}

/// Disable exactly one subscription from a signed link.
///
/// Unauthenticated by necessity — the recipient is reading mail, not logged in.
/// The compensating controls are: a rate limiter consumed BEFORE any database
/// read, a constant-time signature compare, a 90-day token TTL, a constant
/// response body, a structured `info!` line, and a confirmation email to the
/// owner. This repo has no audit table, so those last two are the only
/// repudiation control there is.
pub async fn unsubscribe(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<UnsubscribeReq>,
) -> Result<Json<Value>, ApiError> {
    // Consumed before ANY database read, so a flood of forged tokens costs one
    // Redis round trip each rather than a row lookup each.
    let addr = super::auth::client_addr(&headers, &peer, &state);
    super::auth::rate_limit(&state, &format!("sauron:notify:unsub:{addr}"), 30, 60).await?;

    let key = unsub_signing_key(&state);
    let today = sauron_alerts::crypto::days_since_epoch(chrono::Utc::now());

    let mut conn = db(&state).await?;
    // The owner lookup is inside the closure so a malformed token never reaches
    // the database at all.
    let owner: std::cell::Cell<Option<Uuid>> = std::cell::Cell::new(None);
    let sub_id = {
        // Two-step because the closure cannot be async: parse the id first,
        // then resolve its owner, then verify.
        let parsed: Option<Uuid> = req.token.split('.').next().and_then(|s| s.parse().ok());
        match parsed {
            Some(id) => match repo::get_subscription(&mut conn, id).await? {
                Some(s) => {
                    owner.set(Some(s.user_id));
                    sauron_alerts::crypto::verify_unsubscribe_token(
                        key.as_bytes(),
                        &req.token,
                        today,
                        |_| owner.get(),
                    )
                }
                None => None,
            },
            None => None,
        }
    };

    let Some(sub_id) = sub_id else {
        drop(conn);
        return Ok(Json(generic_unsubscribe_ok()));
    };

    repo::disable_subscription(&mut conn, sub_id, "unsubscribed").await?;
    let user_id = owner.get();
    tracing::info!(
        subscription = %sub_id,
        user = ?user_id,
        "personal notification subscription disabled via unsubscribe link"
    );

    // A confirmation to the owner is the ONLY evidence a silencing happened,
    // and it is the sharpest case for `PersonalNotification`'s zero dedup
    // window: a confirmation suppressed because a notification reached the same
    // address minutes earlier would erase exactly that evidence.
    if let Some(user_id) = user_id {
        if let Some(user) = repo::find_user_by_id(&mut conn, user_id).await? {
            let branding = state.mail_branding();
            let manage = branding.link("/account").ok();
            let mut content = sauron_mail::MailContent {
                subject: "A notification subscription was turned off".to_string(),
                heading: "Subscription disabled".to_string(),
                paragraphs: vec![
                    "Someone used an unsubscribe link in one of your notification emails, \
                     and that subscription is now off."
                        .to_string(),
                    "If this was not you, turn it back on from your account page."
                        .to_string(),
                ],
                cta: None,
                footnotes: Vec::new(),
            };
            if let Some(url) = manage {
                content.cta = sauron_mail::Cta::new("Manage subscriptions", url).ok();
            }
            match sauron_mail::render(&branding, &content) {
                Ok(rendered) => {
                    let recipient_key = user.email.trim().to_lowercase();
                    let _ = repo::enqueue_mail(
                        &mut conn,
                        sauron_db::models::NewMailOutbox {
                            kind: sauron_mail::MailKind::PersonalNotification.as_str(),
                            recipient: &user.email,
                            recipient_key: &recipient_key,
                            subject: &rendered.subject,
                            body_text: &rendered.text,
                            body_html: &rendered.html,
                            user_id: Some(user.id),
                        },
                        86_400,
                        0,
                        true,
                    )
                    .await;
                }
                Err(e) => tracing::warn!(error = %e, "unsubscribe confirmation did not render"),
            }
        }
    }
    drop(conn);
    Ok(Json(generic_unsubscribe_ok()))
}
```

If `AppState` has no `mail_branding()` helper from S0, build the `Branding` inline from `state.cfg` instead — `Branding { product_name: "Sauron".into(), dashboard_url: state.cfg.dashboard_url.clone().ok(), footer: "You are receiving this because you subscribed to notifications in Sauron.".into() }` — and keep the rest unchanged.

- [ ] **Step 4: Register the route.** In `backend/bins/sauron-api/src/main.rs`:

```rust
        .route(
            "/v1/notifications/unsubscribe",
            post(routes::notification_prefs::unsubscribe),
        )
```

- [ ] **Step 5: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api notification_prefs`
  Expected: 5 passing tests.

- [ ] **Step 6: Confirm the route is NOT in the password-change allowlist.**
  `cd /home/splimter/projects/freelance/sauron && grep -n "notification" backend/crates/sauron-auth/src/extractors.rs`
  Expected: no output. A temp-password holder must not reach these routes, so that file stays untouched.

- [ ] **Step 7: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 15: `subscription_kinds` on `/v1/alert-meta`

**Files:**
- Modify `backend/bins/sauron-api/src/routes/notifications.rs` (the `meta` handler, ~line 624)

**Interfaces:**
- Consumes: `SubKind`, `SubConditions` (Task 4).
- Produces: a `subscription_kinds` key on the `/v1/alert-meta` response — an array of `{key, scope_types, env_filter, defaults, clamps}`.

- [ ] **Step 1: Write the failing test.** Append a `#[cfg(test)] mod meta_tests` block at the end of `backend/bins/sauron-api/src/routes/notifications.rs`:

```rust
#[cfg(test)]
mod meta_tests {
    use super::*;

    /// The house convention is to publish enum/option metadata from
    /// `/v1/alert-meta` rather than hardcode lists in Svelte, so the dialog's
    /// conditional fields and its per-kind "the environment filter does not
    /// apply" notice both come from here.
    #[test]
    fn subscription_kinds_metadata_matches_the_enum() {
        let meta = subscription_kinds_meta();
        let arr = meta.as_array().expect("an array");
        assert_eq!(arr.len(), 4);

        let uptime = arr.iter().find(|k| k["key"] == "uptime").expect("uptime");
        assert_eq!(uptime["env_filter"], serde_json::json!(false));
        assert_eq!(uptime["scope_types"], serde_json::json!(["project"]));

        let spike = arr.iter().find(|k| k["key"] == "error_spike").expect("spike");
        assert_eq!(spike["env_filter"], serde_json::json!(true));
        assert_eq!(spike["scope_types"], serde_json::json!(["project", "app"]));
        assert_eq!(spike["defaults"]["window_seconds"], serde_json::json!(900));
        assert_eq!(spike["defaults"]["factor"], serde_json::json!(3.0));
        assert_eq!(spike["defaults"]["min_count"], serde_json::json!(10));
        assert_eq!(spike["clamps"]["factor"], serde_json::json!([1.5, 100.0]));

        let new_issue = arr
            .iter()
            .find(|k| k["key"] == "error_new_issue")
            .expect("new issue");
        assert_eq!(new_issue["defaults"]["level"], serde_json::json!("error"));
    }
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api subscription_kinds_metadata`
  Expected: `error[E0425]: cannot find function 'subscription_kinds_meta'`.

- [ ] **Step 3: Implement.** In `backend/bins/sauron-api/src/routes/notifications.rs`, add above the `meta` handler:

```rust
/// Per-kind metadata for personal notification subscriptions.
///
/// Published here rather than hardcoded in Svelte for the same reason
/// `trigger_types` is: a kind added to `SubKind` without a matching dashboard
/// edit shows up as a missing option, not as a silently wrong form.
fn subscription_kinds_meta() -> serde_json::Value {
    use sauron_alerts::subscription::{SubConditions, SubKind};
    serde_json::Value::Array(
        SubKind::ALL
            .iter()
            .map(|k| {
                let scope_types = if k.allows_app_scope() {
                    json!(["project", "app"])
                } else {
                    json!(["project"])
                };
                let (defaults, clamps) = match k {
                    SubKind::Uptime => (json!({}), json!({})),
                    SubKind::ErrorSpike => (
                        json!({
                            "window_seconds": SubConditions::DEFAULT_WINDOW_SECONDS,
                            "factor": SubConditions::DEFAULT_FACTOR,
                            "min_count": SubConditions::DEFAULT_MIN_COUNT,
                            "level": serde_json::Value::Null,
                        }),
                        json!({
                            "window_seconds": [
                                SubConditions::MIN_WINDOW_SECONDS,
                                SubConditions::MAX_WINDOW_SECONDS
                            ],
                            "factor": [SubConditions::MIN_FACTOR, SubConditions::MAX_FACTOR],
                            "min_count": [
                                SubConditions::MIN_MIN_COUNT,
                                SubConditions::MAX_MIN_COUNT
                            ],
                        }),
                    ),
                    _ => (json!({ "level": "error" }), json!({})),
                };
                json!({
                    "key": k.as_str(),
                    "scope_types": scope_types,
                    "env_filter": k.supports_env_filter(),
                    "defaults": defaults,
                    "clamps": clamps,
                })
            })
            .collect(),
    )
}
```

and add one key to the `meta` handler's returned object, immediately after `"template_vars": { … },`:

```rust
        "subscription_kinds": subscription_kinds_meta(),
```

- [ ] **Step 4: Add the crate dependency if needed.** Confirm `backend/bins/sauron-api/Cargo.toml` lists `sauron-alerts = { workspace = true }` (it does today, for `AlertEngine`).

- [ ] **Step 5: Run the test and watch it pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api subscription_kinds_metadata`
  Expected: `test routes::notifications::meta_tests::subscription_kinds_metadata_matches_the_enum ... ok`.

- [ ] **Step 6: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 16: The revocation sweep

**Where this lives, and why it is not in `sauron-api`.** The sweep has two
callers in two different processes: the three grant-mutation handlers in
`sauron-api` (synchronous, closing the 24-hour window) and the daily backstop
slot in the `sauron-alerts` tick (design §8.4: "The daily pass remains as the
backstop for paths nobody remembered — role permission edits, project
deletion"). A module inside `sauron-api`'s route tree is unreachable from
`sauron-alerts`, and a second copy of the predicate in the binary would drift.
So the sweep goes in the **`sauron-alerts` library crate**, which both already
depend on (`sauron-api/Cargo.toml:19`, `bins/sauron-alerts/Cargo.toml`), and
which gained `sauron-auth` in Task 6. It returns `anyhow::Result`, not `ApiError`:
`sauron-api` has `impl From<anyhow::Error> for ApiError` (`error.rs:64`), so `?`
still works at the call sites, and `sauron-alerts` avoids a direct diesel
dependency it does not otherwise have.

**Files:**
- Create `backend/crates/sauron-alerts/src/sweep.rs`
- Modify `backend/crates/sauron-alerts/src/lib.rs` (`pub mod sweep;` + module-map line)
- Modify `backend/bins/sauron-api/src/routes/orgs.rs` (`delete_grant` ~623, `set_member_active` ~702, `update_grant_handler` ~801)
- Modify `backend/crates/sauron-db/src/repo.rs` (`enabled_subscriptions_all`)

**Interfaces:**
- Consumes: `covers`, `QueueTarget`, `SubKind` (Tasks 4 and 6); `repo::{subscriptions_for_user_in_org, enabled_subscriptions_all, disable_subscription, live_enrollments_for_apps, apps_for_projects, app_ancestries, user_grants_in_org, subscription_envs_for}`.
- Produces:
  - `pub async fn enabled_subscriptions_all(conn) -> QueryResult<Vec<NotificationSubscription>>` in `repo.rs`
  - `pub async fn sweep_user_subscriptions(conn: &mut AsyncPgConnection, user_id: Uuid, org_id: Uuid) -> anyhow::Result<usize>` in `sauron_alerts::sweep`
  - `pub async fn sweep_revoked_subscriptions(conn: &mut AsyncPgConnection) -> anyhow::Result<usize>` in `sauron_alerts::sweep` — the daily backstop, called from the tick loop in Task 17 Step 7

- [ ] **Step 1: Write the failing DB test.** Append to `backend/crates/sauron-db/tests/notifications.rs`:

```rust
/// The overwhelmingly common revocation is PARTIAL — moved off a project, an
/// env grant narrowed, a role downgraded so it no longer carries `issue:read`
/// — and in every one of those the user still holds grants in the org. So
/// "does this user still have any grants here" is the wrong question, and this
/// pins the right one: the subscription's own scope, against its own required
/// permission.
#[tokio::test]
async fn a_partial_revocation_still_leaves_grants_in_the_org() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let user_id = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .unwrap()
        .unwrap()
        .id;

    let sub = seed_subscription(&mut conn, &ids, "error_new_issue", "immediate", None, "UTC").await;
    assert!(sub.enabled);

    sauron_db::repo::disable_subscription(&mut conn, sub.id, "access_revoked")
        .await
        .expect("disable");

    let after = sauron_db::repo::get_subscription(&mut conn, sub.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!after.enabled);
    assert_eq!(after.disabled_reason.as_deref(), Some("access_revoked"));
    assert!(after.disabled_at.is_some());

    // Still visible to its owner and to the sweep's org-scoped query, so the
    // card can explain WHY it is off instead of it looking broken.
    let mine = sauron_db::repo::list_subscriptions_for_user(&mut conn, user_id)
        .await
        .unwrap();
    assert_eq!(mine.len(), 1);
    let live = sauron_db::repo::subscriptions_for_user_in_org(&mut conn, user_id, ids.org_id)
        .await
        .unwrap();
    assert!(live.is_empty(), "the sweep only re-evaluates enabled rows");

    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and watch it fail.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications a_partial_revocation`
  Expected: passes already if Task 9 landed; if `subscriptions_for_user_in_org` is missing, `cannot find function` — add it per Task 9 first.

- [ ] **Step 3: Add the sweep's whole-table read.** Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// Every enabled subscription, for the daily revocation sweep.
///
/// The daily pass is the backstop for the paths nobody remembered — a role's
/// permission list edited, a project deleted. The synchronous sweeps in
/// `routes/orgs.rs` cover the three deliberate grant-mutation sites and close
/// the 24-hour window for them.
pub async fn enabled_subscriptions_all(
    conn: &mut AsyncPgConnection,
) -> QueryResult<Vec<NotificationSubscription>> {
    notification_subscriptions::table
        .filter(notification_subscriptions::enabled.eq(true))
        .select(NotificationSubscription::as_select())
        .load(conn)
        .await
}
```

- [ ] **Step 4: Implement the shared sweep.** Create `backend/crates/sauron-alerts/src/sweep.rs`:

```rust
//! The revocation sweep: self-disable subscriptions whose owner lost reach.
//!
//! Lives in the library crate, not in `sauron-api`'s route tree, because it has
//! two callers in two processes — the grant-mutation handlers (synchronous) and
//! the `sauron-alerts` daily slot (the backstop). One copy, one predicate.

// `anyhow::Result`, not `QueryResult`: `sauron-alerts` has no direct diesel
// dependency (`sauron-db` re-exports `AsyncPgConnection` precisely so it does
// not need one), and `diesel::result::Error` converts into `anyhow::Error`
// through the blanket impl, so `?` still works on every `repo::` call. On the
// `sauron-api` side `impl From<anyhow::Error> for ApiError` (`error.rs:64`)
// makes the call sites unchanged.
use sauron_auth::rbac::{grants_from_rows, perm, reach_for};
use sauron_db::{repo, AsyncPgConnection};
use uuid::Uuid;

use crate::subscription::{covers, QueueTarget, SubKind};

/// Re-evaluate one user's subscriptions in one org and self-disable the ones
/// they can no longer reach. Returns how many were disabled.
///
/// Called synchronously from every grant-mutation site — the same request,
/// after the change commits — because a daily pass alone leaves a 24-hour
/// window in which a revoked member keeps receiving telemetry.
///
/// This deliberately does NOT ask "does this user still have any grants in the
/// org". The overwhelmingly common revocation is partial — moved off a project,
/// an env grant narrowed, a role downgraded so it no longer carries
/// `issue:read` — and in every one of those the answer is still yes.
///
/// Logs at debug, not warn: losing access is normal, not a fault.
pub async fn sweep_user_subscriptions(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> anyhow::Result<usize> {
    let subs = repo::subscriptions_for_user_in_org(conn, user_id, org_id).await?;
    if subs.is_empty() {
        return Ok(0);
    }
    let grants = grants_from_rows(repo::user_grants_in_org(conn, user_id, org_id).await?);
    let issue_reach = reach_for(&grants, perm::ISSUE_READ);
    let monitor_reach = reach_for(&grants, perm::MONITOR_READ);

    let ids: Vec<Uuid> = subs.iter().map(|s| s.id).collect();
    let env_rows = repo::subscription_envs_for(conn, &ids).await?;

    // Batched exactly like the evaluation pass: the daily backstop calls this
    // once per (user, org) pair in the whole install, so a per-subscription
    // query here is a per-subscription query across the entire table.
    let mut app_scope_ids: Vec<Uuid> = Vec::new();
    let mut project_scope_ids: Vec<Uuid> = Vec::new();
    for s in subs.iter().filter(|s| s.kind != "uptime") {
        match s.scope_type.as_str() {
            "app" => app_scope_ids.push(s.scope_id),
            _ => project_scope_ids.push(s.scope_id),
        }
    }
    app_scope_ids.sort_unstable();
    app_scope_ids.dedup();
    project_scope_ids.sort_unstable();
    project_scope_ids.dedup();
    let live_app_scopes = repo::app_ancestries(conn, &app_scope_ids).await?;
    let project_apps = repo::apps_for_projects(conn, &project_scope_ids).await?;

    let mut all_apps: Vec<Uuid> = live_app_scopes.iter().map(|(a, _, _)| *a).collect();
    all_apps.extend(project_apps.iter().map(|(_, a)| *a));
    all_apps.sort_unstable();
    all_apps.dedup();
    let enrollments = repo::live_enrollments_for_apps(conn, &all_apps).await?;

    let mut disabled = 0usize;
    for s in &subs {
        let Some(kind) = SubKind::parse(&s.kind) else {
            continue;
        };
        let still_covered = if kind == SubKind::Uptime {
            // Uptime is authorized at project scope only, exactly as every
            // monitor endpoint is.
            monitor_reach.org || monitor_reach.projects.contains(&s.scope_id)
        } else {
            // `scope_id` has no FK, so the target can be gone. A subscription
            // pointing at nothing can never fire; disable it rather than leave
            // it enabled forever.
            let (project_id, app_ids): (Uuid, Vec<Uuid>) = match s.scope_type.as_str() {
                "app" => match live_app_scopes.iter().find(|(a, _, _)| *a == s.scope_id) {
                    Some((_, project_id, _)) => (*project_id, vec![s.scope_id]),
                    None => (Uuid::nil(), Vec::new()),
                },
                _ => (
                    s.scope_id,
                    project_apps
                        .iter()
                        .filter(|(project_id, _)| *project_id == s.scope_id)
                        .map(|(_, app_id)| *app_id)
                        .collect(),
                ),
            };
            if app_ids.is_empty() {
                false
            } else {
                let catalogue: Vec<Uuid> = env_rows
                    .iter()
                    .filter(|(sid, _)| *sid == s.id)
                    .map(|(_, e)| *e)
                    .collect();
                // Catalogue ids cross to ENROLLMENT ids here. `Reach.envs` holds
                // enrollment ids; a catalogue id compared against it matches
                // nothing and would silently disable every env-narrowed
                // subscription in the install.
                let sub_enrollments: Vec<Uuid> = if catalogue.is_empty() {
                    Vec::new()
                } else {
                    enrollments
                        .iter()
                        .filter(|(_, app, c)| app_ids.contains(app) && catalogue.contains(c))
                        .map(|(e, _, _)| *e)
                        .collect()
                };
                app_ids.iter().all(|app_id| {
                    covers(
                        &issue_reach,
                        &QueueTarget {
                            project_id,
                            app_id: Some(*app_id),
                            env_enrollments: &sub_enrollments,
                            includes_unattributed: sub_enrollments.is_empty(),
                        },
                    )
                })
            }
        };
        if !still_covered {
            repo::disable_subscription(conn, s.id, "access_revoked").await?;
            tracing::debug!(
                subscription = %s.id,
                user = %user_id,
                org = %org_id,
                "subscription disabled: owner no longer reaches its scope"
            );
            disabled += 1;
        }
    }
    Ok(disabled)
}

/// The daily backstop: re-evaluate EVERY enabled subscription.
///
/// The three synchronous call sites in `routes/orgs.rs` cover the grant
/// mutations a human performs deliberately. They do not cover a role's
/// permission list being edited, a project being deleted, or an app being
/// removed — the paths nobody remembered. This pass is what catches those, at
/// the cost of a 24-hour worst case.
///
/// Grouped by `(user_id, org_id)` so the grant load and the batched scope
/// resolution are paid once per pair rather than once per subscription.
pub async fn sweep_revoked_subscriptions(conn: &mut AsyncPgConnection) -> anyhow::Result<usize> {
    let all = repo::enabled_subscriptions_all(conn).await?;
    let mut pairs: Vec<(Uuid, Uuid)> = all.iter().map(|s| (s.user_id, s.org_id)).collect();
    pairs.sort_unstable();
    pairs.dedup();

    let mut disabled = 0usize;
    for (user_id, org_id) in pairs {
        // One tenant's failure must not abandon the rest of the pass. This is
        // the LAST line of defence against a member who kept receiving
        // telemetry after losing access; `?` here would let a single unlucky
        // row silence the backstop for the entire install, once a day, forever.
        match sweep_user_subscriptions(conn, user_id, org_id).await {
            Ok(n) => disabled += n,
            Err(e) => tracing::warn!(
                error = %e,
                user = %user_id,
                org = %org_id,
                "revocation sweep failed for one user"
            ),
        }
    }
    Ok(disabled)
}
```

  Then add to `backend/crates/sauron-alerts/src/lib.rs`, beside the `pub mod subscription;` line Task 4 added, `pub mod sweep;`, and to the module map doc comment:

```rust
//! - [`sweep`]   — self-disable personal subscriptions whose owner lost reach.
```

- [ ] **Step 5: Call it from the three grant-mutation sites.** In `backend/bins/sauron-api/src/routes/orgs.rs`:

In `delete_grant`, immediately before `Ok(Json(serde_json::json!({ "ok": true })))`:

```rust
    // A daily sweep alone leaves a 24-hour window in which a revoked member
    // keeps receiving telemetry by email. Run it here, after the grant change
    // has committed, for the paths a human actually takes.
    if let Err(e) =
        sauron_alerts::sweep::sweep_user_subscriptions(&mut conn, grant.user_id, org_id).await
    {
        tracing::warn!(error = ?e, "notification subscription sweep failed after grant delete");
    }
```

In `set_member_active` and in `update_grant_handler`, insert the same block immediately before each handler's success `Ok(Json(...))`, substituting the local variable that holds the affected member's user id (`user_id` in `set_member_active`, `grant.user_id` in `update_grant_handler`) and the org id already in scope.

- [ ] **Step 6: Run the DB test and check the workspace.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications && cargo check --workspace --all-targets`
  Expected: all DB tests pass and the workspace compiles.

- [ ] **Step 7: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 17: The subscription evaluation pass

**Files:**
- Modify `backend/bins/sauron-alerts/Cargo.toml` (add `sauron-auth.workspace = true`, `sauron-mail.workspace = true`)
- Create `backend/bins/sauron-alerts/src/subs.rs`
- Modify `backend/bins/sauron-alerts/src/main.rs` (`mod subs;` + the tick loop's scheduled slots)

**Interfaces:**
- Consumes: `sauron_alerts::subscription::{SubKind, SubConditions, SubInput, Probe, coalesce, spike_fires}` (Tasks 4-5); `repo::{enabled_subscriptions_by_kinds, subscription_envs_for, apps_for_projects, app_ancestries, live_enrollments_for_apps, alert_count_errors_by_app, alert_new_issues_env, alert_regressed_issues_env, alert_new_issues, alert_regressed_issues, enqueue_notifications, notification_recently_queued, touch_subscriptions_evaluated, QueueInsert}`.
- Produces: `pub async fn evaluate_subscriptions(pool: &PgPool, redis: &RedisStore, cfg: &Config, tick_counter: u64) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing test.** Create `backend/bins/sauron-alerts/src/subs.rs` with only the docs and a test module:

```rust
//! The personal-subscription evaluation pass: load, coalesce, probe, fan out
//! by app id, throttle, enqueue.
//!
//! Producers never send mail. This module only INSERTs into
//! `notification_queue`; `drain.rs` is the single place that turns queue rows
//! into `mail_outbox` rows. That split is what lets `sauron-monitor`
//! participate without ever learning about SMTP, and it is what makes delivery
//! exclusive across replicas.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orgs_rotate_so_a_clip_does_not_always_land_on_the_same_tenant() {
        let orgs: Vec<u64> = (0..5).collect();
        // A single global probe ceiling is a cross-tenant starvation vector, so
        // the ceiling is per-org AND the visiting order rotates.
        assert_eq!(rotate(&orgs, 0), vec![0, 1, 2, 3, 4]);
        assert_eq!(rotate(&orgs, 2), vec![2, 3, 4, 0, 1]);
        assert_eq!(rotate(&orgs, 7), vec![2, 3, 4, 0, 1]);
        assert_eq!(rotate(&[] as &[u64], 3), Vec::<u64>::new());
    }

    #[test]
    fn the_issue_limit_scales_with_app_count_and_is_capped() {
        // A probe spans several apps, so a fixed 20 lets one noisy app starve
        // the rest — but an unbounded limit would let a 5000-app org pull 100k
        // rows into one tick.
        assert_eq!(issue_limit(1), 21);
        assert_eq!(issue_limit(3), 61);
        assert_eq!(issue_limit(50), 201);
        assert_eq!(issue_limit(0), 21);
    }
}
```

- [ ] **Step 2: Register the module and run the test.** Add `mod subs;` near the top of `backend/bins/sauron-alerts/src/main.rs`, then run
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts-bin subs`
  Expected: `error[E0425]: cannot find function 'rotate'` and `cannot find function 'issue_limit'`.

- [ ] **Step 3: Add the dependencies.** In `backend/bins/sauron-alerts/Cargo.toml`, after `sauron-alerts = { workspace = true }`:

```toml
sauron-auth = { workspace = true }
sauron-mail = { workspace = true }
```

`sauron-alerts-bin` takes `sauron-mail` solely for the template renderer — no `lettre`, no SMTP config. `sauron-api` remains the only process that drains `mail_outbox`, so a deployment running `sauron-alerts` needs no relay settings and personal mail cannot be delivered twice.

- [ ] **Step 4: Implement the two pure helpers.** Insert into `subs.rs` between the module docs and `#[cfg(test)]`:

```rust
use std::sync::Arc;

use chrono::Utc;
use sauron_alerts::subscription::{coalesce, spike_fires, Probe, SubConditions, SubInput, SubKind};
use sauron_core::Config;
use sauron_db::models::NotificationSubscription;
use sauron_db::repo::{self, QueueInsert};
use sauron_db::PgPool;
use sauron_redis::RedisStore;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use uuid::Uuid;

/// Visit `items` starting at `offset % len`, wrapping.
///
/// A single global probe ceiling is a cross-tenant starvation vector: a handful
/// of self-registered accounts saturating it would silently stop evaluating a
/// paying tenant's subscriptions. The ceiling is therefore per-org, and this
/// rotation makes a clip move around instead of always landing on the same
/// alphabetically-unlucky tenant.
fn rotate<T: Clone>(items: &[T], offset: u64) -> Vec<T> {
    if items.is_empty() {
        return Vec::new();
    }
    let start = (offset % items.len() as u64) as usize;
    items[start..]
        .iter()
        .chain(items[..start].iter())
        .cloned()
        .collect()
}

/// `min(20 × app_count, 200) + 1`.
///
/// A probe spans several apps, so the shipped fixed 20 lets one noisy app
/// starve the rest. The `+ 1` is the truncation sentinel: if the full count
/// comes back, the rendered email says "and more".
fn issue_limit(app_count: usize) -> i64 {
    (20i64.saturating_mul(app_count.max(1) as i64)).min(200) + 1
}

/// The `dedup_key` infix that marks a truncation-sentinel queue row.
///
/// The sentinel travels as an ordinary queue row so it gets the same
/// delivery-time coverage re-check as the issues it summarises. `drain.rs`
/// matches on this to sort it last — "and more" printed in the middle of a
/// list conveys nothing.
pub(crate) const TRUNCATION_MARKER: &str = ":truncated:";
```

- [ ] **Step 5: Run the two tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts-bin subs`
  Expected: 2 passing tests.

- [ ] **Step 6: Implement the pass.** Append to `subs.rs` above `#[cfg(test)]`:

```rust
/// One notification the evaluator decided to send, before throttling.
struct Candidate {
    subscription_id: Uuid,
    project_id: Uuid,
    app_id: Uuid,
    throttle_seconds: i32,
    env_enrollments: Vec<Uuid>,
    includes_unattributed: bool,
    kind: String,
    dedup_key: String,
    severity: String,
    title: String,
    body: String,
}

/// Evaluate every enabled non-uptime subscription once.
///
/// Uptime is NOT evaluated here: it is event-driven and enqueued inline by
/// `sauron-monitor`, exactly as `monitor_down`/`monitor_up` alert rules are.
pub async fn evaluate_subscriptions(
    pool: &PgPool,
    redis: &RedisStore,
    cfg: &Config,
    tick_counter: u64,
) -> anyhow::Result<()> {
    let mut conn = sauron_db::conn(pool).await?;
    let subs = repo::enabled_subscriptions_by_kinds(
        &mut conn,
        &["error_spike", "error_new_issue", "error_regression"],
    )
    .await?;
    if subs.is_empty() {
        return Ok(());
    }

    // Every scope and every environment set resolved in BATCHED queries, never
    // one per subscription. Three round trips total, whatever N is: the env
    // child rows, the app-scope ancestries, the project-scope app lists. Doing
    // this per subscription would be N round trips per tick against a pool of 8
    // shared with the drain, which is the blow-up the probe coalescing further
    // down exists to prevent.
    let sub_ids: Vec<Uuid> = subs.iter().map(|s| s.id).collect();
    let env_rows = repo::subscription_envs_for(&mut conn, &sub_ids).await?;

    let mut app_scope_ids: Vec<Uuid> = Vec::new();
    let mut project_scope_ids: Vec<Uuid> = Vec::new();
    for s in subs.iter() {
        match s.scope_type.as_str() {
            "app" => app_scope_ids.push(s.scope_id),
            _ => project_scope_ids.push(s.scope_id),
        }
    }
    app_scope_ids.sort_unstable();
    app_scope_ids.dedup();
    project_scope_ids.sort_unstable();
    project_scope_ids.dedup();

    // `scope_id` has no FK, so a row can outlive its target. An id absent from
    // these results is an unresolvable scope and its subscription is skipped,
    // never guessed at.
    let live_app_scopes = repo::app_ancestries(&mut conn, &app_scope_ids).await?;
    let project_apps = repo::apps_for_projects(&mut conn, &project_scope_ids).await?;

    let mut inputs: Vec<SubInput> = Vec::with_capacity(subs.len());
    for (index, s) in subs.iter().enumerate() {
        let Some(kind) = SubKind::parse(&s.kind) else {
            continue;
        };
        let app_ids: Vec<Uuid> = match s.scope_type.as_str() {
            "app" => {
                if live_app_scopes.iter().any(|(a, _, _)| *a == s.scope_id) {
                    vec![s.scope_id]
                } else {
                    continue;
                }
            }
            _ => project_apps
                .iter()
                .filter(|(project_id, _)| *project_id == s.scope_id)
                .map(|(_, app_id)| *app_id)
                .collect(),
        };
        if app_ids.is_empty() {
            continue;
        }
        inputs.push(SubInput {
            index,
            org_id: s.org_id,
            kind,
            cond: SubConditions::from_value(kind, &s.conditions),
            catalogue_envs: env_rows
                .iter()
                .filter(|(sid, _)| *sid == s.id)
                .map(|(_, e)| *e)
                .collect(),
            app_ids,
        });
    }

    // One crossing of the catalogue -> enrollment bridge over the union of every
    // app in play.
    let mut all_apps: Vec<Uuid> = inputs.iter().flat_map(|i| i.app_ids.clone()).collect();
    all_apps.sort_unstable();
    all_apps.dedup();
    let enrollments = repo::live_enrollments_for_apps(&mut conn, &all_apps).await?;
    // Don't hold a pooled connection across the fan-out: this pool is 8 for the
    // whole process and is shared with the drain.
    drop(conn);

    let probes = coalesce(&inputs);

    // Per-ORG ceiling, applied in rotating order.
    let mut org_ids: Vec<Uuid> = probes.iter().map(|p| p.key.org_id).collect();
    org_ids.sort_unstable();
    org_ids.dedup();
    let ceiling = cfg.notify_subs_max_probes_per_org.clamp(1, 1000);
    let mut allowed: Vec<usize> = Vec::new();
    for org_id in rotate(&org_ids, tick_counter) {
        let mine: Vec<usize> = probes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.key.org_id == org_id)
            .map(|(i, _)| i)
            .collect();
        if mine.len() > ceiling {
            // Observable rather than inferred: "we are not evaluating your
            // subscriptions" must appear in the log, with the org named.
            warn!(
                org = %org_id,
                probes = mine.len(),
                skipped = mine.len() - ceiling,
                "subscription probe ceiling reached"
            );
        }
        allowed.extend(mine.into_iter().take(ceiling));
    }

    // The same Semaphore(4) bound the rule evaluator uses, for the same reason.
    let sem = Arc::new(Semaphore::new(4));
    let now = Utc::now();
    let subs = Arc::new(subs);
    let enrollments = Arc::new(enrollments);
    let mut handles = Vec::with_capacity(allowed.len());
    for probe_idx in allowed {
        let probe = probes[probe_idx].clone();
        let pool = pool.clone();
        let redis = redis.clone();
        let sem = sem.clone();
        let subs = subs.clone();
        let enrollments = enrollments.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            if let Err(e) = run_probe(&pool, &redis, &probe, &subs, &enrollments, now).await {
                warn!(error = %e, "subscription probe failed");
            }
        }));
    }
    for h in handles {
        if let Err(e) = h.await {
            warn!(error = %e, "subscription probe task panicked");
        }
    }
    Ok(())
}

/// Run one probe and enqueue whatever it produced.
///
/// Fan-out is BY APP ID, never by positional index. A key-collision bug in the
/// coalescing would otherwise attribute one app's counts to another user's
/// subscription — a telemetry leak inside an email. App ids are globally unique
/// UUIDs, so a wrong attribution requires an id bug rather than a
/// set-membership bug, and the drain's independent reach re-check catches the
/// cross-tenant case even then.
async fn run_probe(
    pool: &PgPool,
    redis: &RedisStore,
    probe: &Probe,
    subs: &[NotificationSubscription],
    enrollments: &[(Uuid, Uuid, Uuid)],
    now: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    let key = &probe.key;
    // The probe's enrollment array: live enrollments of its apps whose
    // CATALOGUE environment is in the key's set. `None` when the set is empty,
    // which means "every environment plus unattributed rows".
    let env_ids: Option<Vec<Uuid>> = if key.catalogue_envs.is_empty() {
        None
    } else {
        Some(
            enrollments
                .iter()
                .filter(|(_, app, catalogue)| {
                    probe.app_ids.contains(app) && key.catalogue_envs.contains(catalogue)
                })
                .map(|(enrollment, _, _)| *enrollment)
                .collect(),
        )
    };
    let env_ref = env_ids.as_deref();
    let includes_unattributed = env_ids.is_none();

    let window = chrono::Duration::seconds(key.cond.window_seconds as i64);
    let mut conn = sauron_db::conn(pool).await?;

    let mut candidates: Vec<Candidate> = Vec::new();
    match key.kind {
        SubKind::ErrorSpike => {
            let from = now - window;
            let prev_from = from - window;
            let current = repo::alert_count_errors_by_app(
                &mut conn,
                &probe.app_ids,
                from,
                now,
                key.cond.level.as_deref(),
                env_ref,
            )
            .await?;
            let baseline = repo::alert_count_errors_by_app(
                &mut conn,
                &probe.app_ids,
                prev_from,
                from,
                key.cond.level.as_deref(),
                env_ref,
            )
            .await?;
            let mins = key.cond.window_seconds / 60;
            for (app_id, c) in current {
                let b = baseline
                    .iter()
                    .find(|(a, _)| *a == app_id)
                    .map(|(_, n)| *n)
                    .unwrap_or(0);
                if !spike_fires(
                    c,
                    b,
                    key.cond.min_count,
                    key.cond.factor_milli as f64 / 1000.0,
                ) {
                    continue;
                }
                for &sub_idx in &probe.subs {
                    let s = &subs[sub_idx];
                    if !subscription_owns_app(s, app_id) {
                        continue;
                    }
                    candidates.push(Candidate {
                        subscription_id: s.id,
                        project_id: Uuid::nil(),
                        app_id,
                        throttle_seconds: s.throttle_seconds,
                        env_enrollments: env_ids.clone().unwrap_or_default(),
                        includes_unattributed,
                        kind: "error_spike".into(),
                        dedup_key: format!("sub:{}:spike:{app_id}", s.id),
                        severity: "warning".into(),
                        title: format!("Error spike in the last {mins}m"),
                        body: format!(
                            "{c} error event(s) in the last {mins} minute(s) vs {b} in the \
                             previous {mins}."
                        ),
                    });
                }
            }
        }
        SubKind::ErrorNewIssue | SubKind::ErrorRegression => {
            // The watermark is the OLDEST `last_evaluated_at` among this probe's
            // subscriptions, floored at one window, so a subscription that fell
            // behind is caught up rather than skipped.
            let since = probe
                .subs
                .iter()
                .filter_map(|i| subs[*i].last_evaluated_at)
                .min()
                .unwrap_or(now - window)
                .max(now - window);
            let limit = issue_limit(probe.app_ids.len());
            let mut issues = match (key.kind, env_ref) {
                (SubKind::ErrorNewIssue, Some(envs)) => {
                    repo::alert_new_issues_env(
                        &mut conn,
                        &probe.app_ids,
                        since,
                        now,
                        key.cond.level.as_deref(),
                        envs,
                        limit,
                    )
                    .await?
                }
                (SubKind::ErrorNewIssue, None) => {
                    repo::alert_new_issues(
                        &mut conn,
                        &probe.app_ids,
                        since,
                        now,
                        key.cond.level.as_deref(),
                        limit,
                    )
                    .await?
                }
                (_, Some(envs)) => {
                    repo::alert_regressed_issues_env(
                        &mut conn,
                        &probe.app_ids,
                        since,
                        now,
                        key.cond.level.as_deref(),
                        envs,
                        limit,
                    )
                    .await?
                }
                (_, None) => {
                    repo::alert_regressed_issues(
                        &mut conn,
                        &probe.app_ids,
                        since,
                        now,
                        key.cond.level.as_deref(),
                        limit,
                    )
                    .await?
                }
            };
            // `issue_limit` asked for one row more than it intends to send. A
            // full result set therefore means "there is at least one more issue
            // that will not be named", and the sentinel row below is what turns
            // that into something the reader can see. Without it a truncated
            // batch is indistinguishable from a complete one, and the reader
            // draws the wrong conclusion from a number that is simply short.
            let truncated = issues.len() as i64 >= limit;
            if truncated {
                issues.truncate((limit - 1).max(0) as usize);
            }

            let verb = if key.kind == SubKind::ErrorNewIssue {
                "New issue"
            } else {
                "Issue regressed"
            };
            for issue in issues {
                for &sub_idx in &probe.subs {
                    let s = &subs[sub_idx];
                    if !subscription_owns_app(s, issue.app_id) {
                        continue;
                    }
                    candidates.push(Candidate {
                        subscription_id: s.id,
                        project_id: Uuid::nil(),
                        app_id: issue.app_id,
                        throttle_seconds: s.throttle_seconds,
                        env_enrollments: env_ids.clone().unwrap_or_default(),
                        includes_unattributed,
                        kind: key.kind.as_str().to_string(),
                        dedup_key: format!("sub:{}:issue:{}", s.id, issue.id),
                        severity: "warning".into(),
                        title: format!("{verb}: {}", issue.title),
                        body: format!(
                            "{verb} ({}) — seen {} time(s).",
                            issue.level, issue.times_seen
                        ),
                    });
                }
            }

            if truncated {
                // The sentinel is a real queue row, not a flag, so it inherits
                // the drain's delivery-time coverage re-check exactly like the
                // issues it summarises. It carries the app id of that
                // subscription's last issue because a row with `app_id = None`
                // is read as UPTIME by `covers` and would be refused to every
                // app- and env-scoped member.
                let sentinels: Vec<Candidate> = probe
                    .subs
                    .iter()
                    .filter_map(|&sub_idx| {
                        let s = &subs[sub_idx];
                        let app_id = candidates
                            .iter()
                            .rev()
                            .find(|c| c.subscription_id == s.id)?
                            .app_id;
                        Some(Candidate {
                            subscription_id: s.id,
                            project_id: Uuid::nil(),
                            app_id,
                            // Never throttled. It is the honesty marker on a
                            // batch that IS being delivered; suppressing it
                            // would leave the undercount silent, which is the
                            // whole failure it exists to prevent.
                            throttle_seconds: 0,
                            env_enrollments: env_ids.clone().unwrap_or_default(),
                            includes_unattributed,
                            kind: key.kind.as_str().to_string(),
                            // The marker is how the drain recognises this row
                            // and sorts it last; the timestamp keeps successive
                            // truncated passes from colliding on the partial
                            // unique index.
                            dedup_key: format!(
                                "sub:{}{TRUNCATION_MARKER}{}",
                                s.id,
                                now.timestamp()
                            ),
                            severity: "info".into(),
                            title: "…and more".into(),
                            // No count. The probe's limit is shared across every
                            // subscription in it, but `subscription_owns_app`
                            // hands each one a different subset, so any number
                            // printed here would be right for some readers and
                            // wrong for the rest.
                            body: "More issues matched than fit in one notification; the list \
                                   above is not complete."
                                .to_string(),
                        })
                    })
                    .collect();
                candidates.extend(sentinels);
            }
        }
        SubKind::Uptime => {}
    }

    if candidates.is_empty() {
        let ids: Vec<Uuid> = probe.subs.iter().map(|i| subs[*i].id).collect();
        repo::touch_subscriptions_evaluated(&mut conn, &ids, now).await?;
        return Ok(());
    }

    // Fill each candidate's project id from its own app, in one query.
    let app_ids: Vec<Uuid> = candidates.iter().map(|c| c.app_id).collect();
    let ancestries = repo::app_ancestries(&mut conn, &app_ids).await?;
    for c in &mut candidates {
        if let Some((_, project_id, _)) = ancestries.iter().find(|(a, _, _)| *a == c.app_id) {
            c.project_id = *project_id;
        }
    }
    candidates.retain(|c| c.project_id != Uuid::nil());

    // Throttle: Redis first, durable fallback when Redis is unreachable.
    // Extending the key with the subscription id is what gives PER-RECIPIENT
    // throttling with no new infrastructure — the org engine's key is per rule.
    // The 250ms timeout exists because `RedisStore` is built with
    // `set_response_timeout(None)` and a command against a dead Redis is
    // measured at 9-19s.
    let mut allowed: Vec<Candidate> = Vec::new();
    for c in candidates {
        if c.throttle_seconds <= 0 {
            allowed.push(c);
            continue;
        }
        let redis_key = format!("sauron:notify:{}:{}", c.subscription_id, c.dedup_key);
        let claimed = match tokio::time::timeout(
            std::time::Duration::from_millis(250),
            redis.set_nx_ex(&redis_key, "1", c.throttle_seconds as u64),
        )
        .await
        {
            Ok(Ok(true)) => true,
            Ok(Ok(false)) => false,
            _ => !repo::notification_recently_queued(
                &mut conn,
                c.subscription_id,
                &c.dedup_key,
                c.throttle_seconds,
            )
            .await?,
        };
        if claimed {
            allowed.push(c);
        }
    }

    let rows: Vec<QueueInsert> = allowed
        .iter()
        .map(|c| QueueInsert {
            subscription_id: c.subscription_id,
            project_id: c.project_id,
            app_id: Some(c.app_id),
            includes_unattributed: c.includes_unattributed,
            kind: &c.kind,
            dedup_key: &c.dedup_key,
            severity: &c.severity,
            title: &c.title,
            body: &c.body,
            link: None,
            env_enrollments: c.env_enrollments.clone(),
        })
        .collect();
    let n = repo::enqueue_notifications(&mut conn, &rows).await?;
    if n > 0 {
        info!(
            enqueued = n,
            kind = key.kind.as_str(),
            "personal notifications enqueued"
        );
    }

    let ids: Vec<Uuid> = probe.subs.iter().map(|i| subs[*i].id).collect();
    repo::touch_subscriptions_evaluated(&mut conn, &ids, now).await?;
    Ok(())
}

/// Whether this subscription's own scope includes `app_id`.
///
/// A probe's app array is the UNION of its subscriptions' scopes, so a result
/// for app X must only be attributed to the subscriptions that actually cover
/// X — otherwise a shared condition bucket would cross-deliver between users of
/// the same org.
fn subscription_owns_app(s: &NotificationSubscription, app_id: Uuid) -> bool {
    match s.scope_type.as_str() {
        "app" => s.scope_id == app_id,
        // A project-scoped subscription owns every app resolved from its own
        // project query, and `evaluate_subscriptions` built the app list from
        // exactly that query.
        _ => true,
    }
}
```

- [ ] **Step 7: Wire the sub-jobs into the tick loop.** In `backend/bins/sauron-alerts/src/main.rs`, replace the `let mut last_prune = …;` declaration and the whole `loop { … }` body with:

```rust
    // Dated so the first tick prunes: a fresh deploy should reclaim whatever
    // accumulated while nothing was reaping, not wait an hour to start.
    let mut last_prune = Utc::now() - chrono::Duration::days(1);
    let mut last_subs_eval = Utc::now() - chrono::Duration::days(1);
    // NOT dated into the past like the others: the sweep is the expensive
    // whole-table pass, and running it during boot — before the process has
    // even proven it can reach Postgres — buys nothing. The synchronous sweeps
    // in `routes/orgs.rs` already cover every deliberate grant change; this slot
    // exists only for the paths nobody remembered.
    let mut last_sweep = Utc::now();
    let mut tick_counter: u64 = 0;
    loop {
        tick_counter = tick_counter.wrapping_add(1);
        if let Err(e) = evaluate_all(&pool, &redis, &engine).await {
            warn!(error = %e, "alert evaluation tick failed");
        }

        // 120s by default, deliberately slower than the 30s org tick: personal
        // email does not need 30s latency, and cadence is the single largest
        // cost lever in this subsystem.
        let subs_tick = cfg.notify_subs_tick_secs.clamp(30, 3600) as i64;
        if (Utc::now() - last_subs_eval).num_seconds() >= subs_tick {
            if let Err(e) = subs::evaluate_subscriptions(&pool, &redis, &cfg, tick_counter).await {
                warn!(error = %e, "subscription evaluation tick failed");
            }
            last_subs_eval = Utc::now();
        }

        // Every tick, not on the subscription cadence, so `immediate` really is
        // immediate.
        if let Err(e) = drain::drain_notification_queue(&pool, &cfg).await {
            warn!(error = %e, "notification drain failed");
        }

        // The daily backstop for revocations no handler caught: a role's
        // permission list edited, a project deleted, an app removed. The
        // synchronous sweeps in `routes/orgs.rs` close the 24-hour window for
        // the three deliberate grant-mutation paths; this closes it for
        // everything else.
        if (Utc::now() - last_sweep).num_hours() >= 24 {
            match sauron_db::conn(&pool).await {
                Ok(mut conn) => {
                    match sauron_alerts::sweep::sweep_revoked_subscriptions(&mut conn).await {
                        Ok(n) if n > 0 => {
                            info!(disabled = n, "subscriptions disabled: owner lost reach")
                        }
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "revocation sweep failed"),
                    }
                }
                Err(e) => warn!(error = %e, "revocation sweep: no database connection"),
            }
            last_sweep = Utc::now();
        }

        // `alert_events` gains a row per evaluation — including every suppressed
        // one — so without a reaper a throttled rule grows it without bound.
        if (Utc::now() - last_prune).num_minutes() >= 60 {
            match sauron_db::conn(&pool).await {
                Ok(mut conn) => {
                    match repo::prune_alert_events(&mut conn, cfg.alert_event_retention_days).await
                    {
                        Ok(n) if n > 0 => info!(pruned = n, "pruned old alert events"),
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "pruning alert events failed"),
                    }
                    // A queue's reaper runs in the process that DRAINS it, and
                    // `notification_queue` is drained right here.
                    match repo::prune_notification_queue(
                        &mut conn,
                        cfg.notify_queue_retention_days.clamp(1, 365) as i32,
                    )
                    .await
                    {
                        Ok(n) if n > 0 => info!(pruned = n, "pruned finished notifications"),
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "pruning notification queue failed"),
                    }
                    // No graceful shutdown exists anywhere in this codebase, so
                    // a process killed mid-drain leaves rows `claimed` forever.
                    match repo::requeue_stuck_notifications(
                        &mut conn,
                        repo::STUCK_CLAIM_SECS,
                        repo::MAX_QUEUE_ATTEMPTS,
                    )
                    .await
                    {
                        Ok(n) if n > 0 => info!(requeued = n, "requeued stuck notifications"),
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "requeueing stuck notifications failed"),
                    }
                }
                Err(e) => warn!(error = %e, "prune: no database connection"),
            }
            last_prune = Utc::now();
        }
        tokio::time::sleep(tick).await;
    }
```

The `drain::` call will not compile until Task 18; comment that one line out until then, or do Task 18 first and paste this block once. The `sauron_alerts::sweep::` call needs Task 16 to have landed — it is in the library crate precisely so this binary can reach it.

- [ ] **Step 8: Check the workspace and run the tests.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets && cargo test -p sauron-alerts-bin`
  Expected: compiles; `rotate` and `issue_limit` pass alongside the existing `percentile_mapping` / `fmt_num_drops_trailing_zero`.

- [ ] **Step 9: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 18: The drain

**Files:**
- Create `backend/bins/sauron-alerts/src/drain.rs`
- Modify `backend/bins/sauron-alerts/src/main.rs` (`mod drain;`)
- Modify `backend/crates/sauron-db/tests/notifications.rs`

**Interfaces:**
- Consumes: `covers`, `QueueTarget` (Task 6); `crate::subs::TRUNCATION_MARKER` (Task 17); `repo::{claim_due_notifications, queue_envs_for, project_org_batch, user_grants_in_org, sent_messages_last_hour, mark_notifications_sent, drop_notifications, fail_notifications, notification_queue_depth, find_user_by_id, enqueue_mail, MAX_QUEUE_ATTEMPTS}`; `sauron_alerts::crypto::{derive_unsub_key, unsubscribe_token, days_since_epoch}`; `sauron_mail::{Branding, MailContent, Cta, MailKind, render}`.
- Produces: `pub async fn drain_notification_queue(pool: &PgPool, cfg: &Config) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing DB test.** Append to `backend/crates/sauron-db/tests/notifications.rs`:

```rust
/// The delivery-time re-check. The write-time check is a point-in-time snapshot
/// and reach can be revoked afterwards, so the drain repeats the whole
/// computation against freshly loaded grants immediately before rendering — the
/// last moment the data is still inside the trust boundary. A dropped row's
/// content is blanked in the SAME statement that marks it, because it has no
/// further purpose and must not sit at rest for the retention window outside
/// the reader's authorization.
#[tokio::test]
async fn dropping_a_row_for_lost_access_blanks_its_content() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_new_issue", "immediate", None, "UTC").await;
    let dedup = format!("sub:{}:issue:{}", sub.id, ids.issue_id);
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_new_issue",
            dedup_key: &dedup,
            severity: "warning",
            title: "Secret issue title",
            body: "Secret body",
            link: Some("https://example.test/#/issues/1"),
            env_enrollments: vec![],
        }],
    )
    .await
    .unwrap();

    let claimed = sauron_db::repo::claim_due_notifications(&mut conn, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].title.as_deref(), Some("Secret issue title"));

    let n =
        sauron_db::repo::drop_notifications(&mut conn, &[claimed[0].id], "dropped_no_access")
            .await
            .unwrap();
    assert_eq!(n, 1);

    let after = sauron_db::repo::notification_history_for_user(&mut conn, claimed[0].user_id, 10)
        .await
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].status, "dropped_no_access");
    assert_eq!(after[0].title, None);
    assert_eq!(after[0].body, None);
    assert_eq!(after[0].link, None);
    assert!(after[0].finished_at.is_some());

    db.cleanup().await;
}
```

- [ ] **Step 2: Run it.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications dropping_a_row`
  Expected: passes on Task 11's `drop_notifications`. If it fails on a non-NULL title, the blanking clause is missing from that UPDATE — fix it before continuing.

- [ ] **Step 3: Write the drain's own failing unit tests.** Create `backend/bins/sauron-alerts/src/drain.rs` with docs and tests only:

```rust
//! The notification drain: claim, re-check reach, group by user, render one
//! email, hand it to `mail_outbox`.
//!
//! Rendering deliberately does NOT go through `sauron_alerts::deliver` or build
//! an `AlertContext`: `render::email_subject` would stamp `[Sauron/info]` on it
//! and `render::email_body` would sign it "— Sauron alerting". Personal mail
//! must not carry alert-engine branding.
//!
//! `sauron_mail::text::html_escape` does NOT escape the single quote, so
//! anything rendered through the house layout must double-quote every
//! attribute.
//!
//! This process ENQUEUES into `mail_outbox` and never drains it — `sauron-api`
//! is the sole drainer, so `sauron-alerts` needs no SMTP configuration and
//! personal mail cannot be delivered twice by two processes.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_over_the_hourly_cap_is_digested_never_dropped() {
        // Quiet hours and the cap both DEFER or MERGE; neither discards,
        // because "quiet" and "broken" must not look identical from the user's
        // side — which for an observability product is the worst available
        // outcome.
        assert!(!should_digest(0, 20));
        assert!(!should_digest(19, 20));
        assert!(should_digest(20, 20));
        assert!(should_digest(100, 20));
    }

    #[test]
    fn the_subject_reflects_how_many_rows_the_message_carries() {
        assert_eq!(digest_subject(1, "New issue: boom"), "New issue: boom");
        assert_eq!(digest_subject(3, "New issue: boom"), "3 Sauron notifications");
        assert_eq!(digest_subject(0, "fallback"), "fallback");
    }
}
```

- [ ] **Step 4: Register the module and run it.** Add `mod drain;` beside `mod subs;` in `backend/bins/sauron-alerts/src/main.rs`, then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts-bin drain`
  Expected: `error[E0425]: cannot find function 'should_digest'`.

- [ ] **Step 5: Implement.** Insert into `drain.rs` between the docs and `#[cfg(test)]`:

```rust
use std::collections::HashMap;

use chrono::Utc;
use sauron_alerts::subscription::{covers, QueueTarget};
use sauron_auth::rbac::{grants_from_rows, perm, reach_for, Reach};
use sauron_core::Config;
use sauron_db::models::{NewMailOutbox, NotificationQueueItem};
use sauron_db::repo;
use sauron_db::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// The per-user hourly cap degrades delivery to one digest; it never discards.
fn should_digest(sent_messages_last_hour: i64, cap: i64) -> bool {
    sent_messages_last_hour >= cap
}

/// One row keeps its own subject; several become a counted digest.
fn digest_subject(rows: usize, first_title: &str) -> String {
    if rows <= 1 {
        first_title.to_string()
    } else {
        format!("{rows} Sauron notifications")
    }
}

/// Claim and deliver whatever is due, looping until the batch is short or the
/// wall-clock budget is spent.
///
/// A single 200-row batch per 30s tick is ~400 rows/minute, and two shapes
/// exceed that routinely: every `daily` subscriber's rows come due at the same
/// bucket boundary, and a broad incident enqueues across many subscribers at
/// once. Each pass logs pending depth and the oldest pending `deliver_after`,
/// because nothing else in the system would reveal a backlog — `status='sent'`
/// means only "handed to the outbox".
pub async fn drain_notification_queue(pool: &PgPool, cfg: &Config) -> anyhow::Result<()> {
    let batch = cfg.notify_subs_batch.clamp(1, 5000);
    let budget = std::time::Duration::from_millis(cfg.notify_drain_budget_ms.clamp(500, 60_000));
    let started = std::time::Instant::now();

    loop {
        let mut conn = sauron_db::conn(pool).await?;
        let claimed = repo::claim_due_notifications(&mut conn, batch).await?;
        if claimed.is_empty() {
            drop(conn);
            break;
        }
        let taken = claimed.len();
        deliver_batch(&mut conn, cfg, claimed).await?;
        let (depth, oldest) = repo::notification_queue_depth(&mut conn).await?;
        drop(conn);
        info!(delivered = taken, pending = depth, oldest = ?oldest, "notification drain pass");

        if taken < batch as usize || started.elapsed() >= budget {
            break;
        }
    }
    Ok(())
}

async fn deliver_batch(
    conn: &mut sauron_db::AsyncPgConnection,
    cfg: &Config,
    claimed: Vec<NotificationQueueItem>,
) -> anyhow::Result<()> {
    let queue_ids: Vec<Uuid> = claimed.iter().map(|r| r.id).collect();
    let env_rows = repo::queue_envs_for(conn, &queue_ids).await?;
    let project_ids: Vec<Uuid> = claimed.iter().map(|r| r.project_id).collect();
    let orgs = repo::project_org_batch(conn, &project_ids).await?;

    let mut by_user: HashMap<Uuid, Vec<NotificationQueueItem>> = HashMap::new();
    for row in claimed {
        by_user.entry(row.user_id).or_default().push(row);
    }

    // Byte-for-byte identical to `unsub_signing_key` in
    // `sauron-api/src/routes/notification_prefs.rs` (Task 14 Step 3). This
    // process mints the tokens and that one verifies them; a divergence makes
    // every unsubscribe link fail verification, and that endpoint returns the
    // same body whether verification succeeded or not, so the breakage is
    // completely silent. Change one, change the other.
    let unsub_key = {
        let base = cfg
            .notify_secret_key
            .clone()
            .unwrap_or_else(|| cfg.require_jwt_secret().map(String::from).unwrap_or_default());
        sauron_alerts::crypto::derive_unsub_key(base.as_bytes())
    };
    let today = sauron_alerts::crypto::days_since_epoch(Utc::now());

    let branding = sauron_mail::Branding {
        product_name: "Sauron".to_string(),
        dashboard_url: cfg.dashboard_url.clone().ok(),
        footer: "You are receiving this because you subscribed to notifications in Sauron."
            .to_string(),
    };

    for (user_id, rows) in by_user {
        // A deactivated account must never be mailed, whatever its grants say.
        let user = match repo::find_user_by_id(conn, user_id).await? {
            Some(u) if u.is_active => u,
            _ => {
                let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
                repo::drop_notifications(conn, &ids, "dropped_inactive").await?;
                continue;
            }
        };

        let mut survivors: Vec<NotificationQueueItem> = Vec::new();
        let mut dropped: Vec<Uuid> = Vec::new();
        let mut reaches: HashMap<Uuid, (Reach, Reach)> = HashMap::new();
        for row in rows {
            // Re-derive the org from the PROJECT and treat a mismatch with the
            // stored `org_id` as a hard drop. `reach_for`'s org arm is
            // `Scope::Org(_) => reach.org = true` and never compares the org id,
            // so a diverged denormalized column would set `reach.org` and
            // release a foreign tenant's project.
            let Some((_, true_org)) = orgs.iter().find(|(p, _)| *p == row.project_id).copied()
            else {
                dropped.push(row.id);
                continue;
            };
            if true_org != row.org_id {
                warn!(
                    queue_row = %row.id,
                    stored_org = %row.org_id,
                    true_org = %true_org,
                    "queued notification's denormalized org_id diverged from its project"
                );
                dropped.push(row.id);
                continue;
            }
            if !reaches.contains_key(&true_org) {
                let grants =
                    grants_from_rows(repo::user_grants_in_org(conn, user_id, true_org).await?);
                reaches.insert(
                    true_org,
                    (
                        reach_for(&grants, perm::ISSUE_READ),
                        reach_for(&grants, perm::MONITOR_READ),
                    ),
                );
            }
            let (issue_reach, monitor_reach) = &reaches[&true_org];
            let envs: Vec<Uuid> = env_rows
                .iter()
                .filter(|(q, _)| *q == row.id)
                .map(|(_, e)| *e)
                .collect();
            let reach = if row.kind == "uptime" {
                monitor_reach
            } else {
                issue_reach
            };
            let ok = covers(
                reach,
                &QueueTarget {
                    project_id: row.project_id,
                    app_id: row.app_id,
                    env_enrollments: &envs,
                    includes_unattributed: row.includes_unattributed,
                },
            );
            if ok {
                survivors.push(row);
            } else {
                // Debug, not warn: losing access is normal, not an anomaly.
                debug!(queue_row = %row.id, user = %user_id, "notification dropped: no access");
                dropped.push(row.id);
            }
        }
        repo::drop_notifications(conn, &dropped, "dropped_no_access").await?;
        if survivors.is_empty() {
            continue;
        }

        let cap = cfg.notify_max_emails_per_user_per_hour.clamp(1, 1000);
        let digest = should_digest(repo::sent_messages_last_hour(conn, user_id).await?, cap);

        // A truncation sentinel must read as the LAST line: "…and more" printed
        // between two issue titles says nothing. `sort_by_key` on a bool is a
        // stable partition, so everything else keeps its claim order, and the
        // ids/count used below are unaffected by a reorder.
        survivors.sort_by_key(|r| r.dedup_key.contains(crate::subs::TRUNCATION_MARKER));

        let first_title = survivors[0].title.clone().unwrap_or_default();
        let mut paragraphs: Vec<String> = survivors
            .iter()
            .map(|row| {
                format!(
                    "{} — {}",
                    row.title.clone().unwrap_or_default(),
                    row.body.clone().unwrap_or_default()
                )
            })
            .collect();
        if digest {
            paragraphs.insert(
                0,
                format!(
                    "You have reached {cap} notification emails this hour, so the rest are \
                     grouped into this one message."
                ),
            );
        }

        // `DASHBOARD_URL` fails CLOSED at point of use: unset means the
        // notification still sends, with the unsubscribe footer replaced by a
        // line telling the user where to manage subscriptions. It never bails.
        let mut footnotes: Vec<String> = Vec::new();
        let mut cta = None;
        match branding.link("/account") {
            Ok(account_url) => {
                cta = sauron_mail::Cta::new("Manage subscriptions", account_url).ok();
                // A fresh token per send, so links in live mail always work and
                // one forwarded into an archive stops working after 90 days.
                let token = sauron_alerts::crypto::unsubscribe_token(
                    unsub_key.as_bytes(),
                    survivors[0].subscription_id,
                    user_id,
                    today,
                );
                if let Ok(url) = branding.link(&format!("/unsubscribe?token={token}")) {
                    footnotes.push(format!("To stop these emails, open {url}"));
                }
            }
            Err(_) => footnotes
                .push("Manage these notifications from your account page in Sauron.".to_string()),
        }

        let content = sauron_mail::MailContent {
            subject: digest_subject(survivors.len(), &first_title),
            heading: digest_subject(survivors.len(), &first_title),
            paragraphs,
            cta,
            footnotes,
        };

        let ids: Vec<Uuid> = survivors.iter().map(|r| r.id).collect();
        match sauron_mail::render(&branding, &content) {
            Ok(rendered) => {
                let recipient_key = user.email.trim().to_lowercase();
                let enqueued = repo::enqueue_mail(
                    conn,
                    NewMailOutbox {
                        kind: sauron_mail::MailKind::PersonalNotification.as_str(),
                        recipient: &user.email,
                        recipient_key: &recipient_key,
                        subject: &rendered.subject,
                        body_text: &rendered.text,
                        body_html: &rendered.html,
                        user_id: Some(user_id),
                    },
                    // Past a day the grants snapshot behind this body is too old
                    // to release, and `claim_due_mail` refuses an expired row.
                    86_400,
                    // Zero, and load-bearing: S3 already de-duplicates twice (the
                    // Redis SET NX EX per (subscription, dedup_key) and the
                    // partial unique index behind it), so a per-recipient
                    // suppression window here could only discard mail that
                    // survived both — silent loss no signal in this design would
                    // reveal.
                    0,
                    true,
                )
                .await;
                match enqueued {
                    Ok(_) => {
                        let message_id = Uuid::new_v4();
                        repo::mark_notifications_sent(conn, &ids, message_id).await?;
                    }
                    Err(e) => {
                        warn!(error = %e, user = %user_id, "enqueueing notification mail failed");
                        repo::fail_notifications(
                            conn,
                            &ids,
                            &e.to_string(),
                            repo::MAX_QUEUE_ATTEMPTS,
                        )
                        .await?;
                    }
                }
            }
            Err(e) => {
                // A render failure is usually deterministic — the same body will
                // fail the same way next pass — so the attempts cap inside
                // `fail_notifications` is what terminates it. Nothing else
                // would: a row returned to `pending` is invisible to
                // `requeue_stuck_notifications`.
                warn!(error = %e, user = %user_id, "notification mail did not render");
                repo::fail_notifications(conn, &ids, &e.to_string(), repo::MAX_QUEUE_ATTEMPTS)
                    .await?;
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 6: Uncomment the drain call.** In `backend/bins/sauron-alerts/src/main.rs`, restore the `drain::drain_notification_queue(&pool, &cfg)` block from Task 17 Step 7 if it was commented out.

- [ ] **Step 7: Run everything.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-alerts-bin && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications`
  Expected: `should_digest` and `digest_subject` pass; every DB test passes.

- [ ] **Step 8: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 19: `sauron-monitor` enqueues personal uptime notifications

**Files:**
- Modify `backend/bins/sauron-monitor/Cargo.toml` (`sauron-auth.workspace = true`)
- Modify `backend/bins/sauron-monitor/src/main.rs` (`notify_transition`, lines ~276-392)
- Modify `backend/crates/sauron-db/tests/notifications.rs`

**Interfaces:**
- Consumes: `repo::{uptime_subscriptions_for_project, grants_for_users_in_org, project_org, notification_recently_queued, enqueue_notifications, QueueInsert}`; `covers`, `QueueTarget` (Task 6).
- Produces: no new public API; a behaviour change in `notify_transition` plus a private `enqueue_personal_uptime`.

- [ ] **Step 1: Write the failing DB test.** Append to `backend/crates/sauron-db/tests/notifications.rs`:

```rust
/// A project whose admin configured NO monitor_down/monitor_up alert rule is
/// exactly the deployment where a personal uptime subscription is the entire
/// point. `notify_transition` used to `return` on `rules.is_empty()`, so under
/// that early return the enqueue would never happen, forever, with no log line
/// — and that is invisible to every other test in the repository.
#[tokio::test]
async fn a_project_with_zero_alert_rules_still_has_uptime_subscribers() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "uptime", "immediate", None, "UTC").await;

    let rules = sauron_db::repo::alert_rules_for_monitor(&mut conn, ids.project_id, "monitor_down")
        .await
        .expect("load rules");
    assert!(rules.is_empty(), "the harness configures no alert rules");

    let found = sauron_db::repo::uptime_subscriptions_for_project(&mut conn, ids.project_id)
        .await
        .expect("uptime subscriptions");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, sub.id);

    // And the enqueue path itself works with `app_id = NULL` — uptime has no app
    // dimension, because `monitors` carries only `project_id`.
    let monitor_id = uuid::Uuid::from_u128(7);
    let dedup = format!("sub:{}:monitor:{monitor_id}:monitor_down", sub.id);
    let n = sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: None,
            includes_unattributed: false,
            kind: "uptime",
            dedup_key: &dedup,
            severity: "critical",
            title: "Monitor down: api",
            body: "api (https://example.test) is DOWN",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .expect("enqueue uptime notification");
    assert_eq!(n, 1);

    db.cleanup().await;
}
```

- [ ] **Step 2: Run it.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications a_project_with_zero_alert_rules`
  Expected: passes on Tasks 9-10. If `uptime_subscriptions_for_project` is missing, add it per Task 9 first.

- [ ] **Step 3: Add the dependency.** In `backend/bins/sauron-monitor/Cargo.toml`, after `sauron-alerts = { workspace = true }`:

```toml
sauron-auth = { workspace = true }
```

- [ ] **Step 4: Enqueue before the rule lookup.** In `backend/bins/sauron-monitor/src/main.rs`, inside `notify_transition`, immediately after the `let mut conn = match sauron_db::conn(&notifier.pool).await { … };` block and **before** `let rules = match repo::alert_rules_for_monitor(…)`, insert:

```rust
    // BEFORE the rule lookup, deliberately. A project whose admin configured no
    // monitor alert rule is exactly the deployment where a personal uptime
    // subscription is the point, and the `rules.is_empty()` early return below
    // used to make that case enqueue nothing, forever, with no log line.
    enqueue_personal_uptime(notifier, m, status, cause, incident_id, trigger).await;
```

`enqueue_personal_uptime` takes its own short-lived connections rather than borrowing this one, so there is no borrow conflict with the rule loading that follows and the connection can be released around the Redis call.

- [ ] **Step 5: Turn the early return into a scoped one.** Replace:

```rust
    if rules.is_empty() {
        return;
    }
```

with:

```rust
    if rules.is_empty() {
        // Everything below is rule-specific, so returning here is correct NOW —
        // the personal uptime enqueue already ran above and no longer depends on
        // an admin having configured a rule.
        drop(conn);
        return;
    }
```

- [ ] **Step 6: Implement the helper.** Append to `backend/bins/sauron-monitor/src/main.rs`:

```rust
/// Enqueue a `notification_queue` row for every personal uptime subscription on
/// this monitor's project whose owner still reaches it.
///
/// Ordering around Redis is the difference between safe and a prober outage.
/// `RedisStore` is built with `set_response_timeout(None)`, and a command
/// issued against a dead Redis is measured at 9-19s. `notify_transition` is
/// `tokio::spawn`ed per transition, this pool is `monitor_max_concurrency + 8`
/// and `monitor_batch` is 100, so a network fault that both degrades Redis and
/// flips many monitors could pin up to 100 connections for 19s each and starve
/// `record_check_and_state` — uptime probing would die precisely when it
/// matters. So: load under a connection, DROP it, run the Redis claim under a
/// 250ms timeout, then re-acquire for the INSERT.
async fn enqueue_personal_uptime(
    notifier: &Notifier,
    m: &Monitor,
    status: &str,
    cause: Option<&str>,
    incident_id: Option<uuid::Uuid>,
    trigger: &str,
) {
    let mut conn = match sauron_db::conn(&notifier.pool).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "uptime subscriptions: no db connection");
            return;
        }
    };
    let subs = match repo::uptime_subscriptions_for_project(&mut conn, m.project_id).await {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => {
            drop(conn);
            return;
        }
        Err(e) => {
            warn!(error = %e, "uptime subscriptions: load failed");
            drop(conn);
            return;
        }
    };
    let org_id = match repo::project_org(&mut conn, m.project_id).await {
        Ok(Some(o)) => o,
        _ => {
            warn!(project = %m.project_id, "uptime subscriptions: project has no org");
            drop(conn);
            return;
        }
    };
    let user_ids: Vec<uuid::Uuid> = subs.iter().map(|s| s.user_id).collect();
    let grant_rows = match repo::grants_for_users_in_org(&mut conn, &user_ids, org_id).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "uptime subscriptions: grant load failed");
            drop(conn);
            return;
        }
    };
    // Released before anything touches Redis.
    drop(conn);

    // Authorize before enqueueing so the drain is not handed rows it will only
    // discard. It re-checks anyway — this is the first of two, and the second
    // is the one that survives a revocation between here and delivery.
    let mut prepared: Vec<(uuid::Uuid, String, i32)> = Vec::new();
    for s in &subs {
        let rows: Vec<(String, uuid::Uuid, serde_json::Value)> = grant_rows
            .iter()
            .filter(|(u, _, _, _)| *u == s.user_id)
            .map(|(_, t, id, p)| (t.clone(), *id, p.clone()))
            .collect();
        let reach = sauron_auth::rbac::reach_for(
            &sauron_auth::rbac::grants_from_rows(rows),
            sauron_auth::rbac::perm::MONITOR_READ,
        );
        let covered = sauron_alerts::subscription::covers(
            &reach,
            &sauron_alerts::subscription::QueueTarget {
                project_id: m.project_id,
                app_id: None,
                env_enrollments: &[],
                includes_unattributed: false,
            },
        );
        if !covered {
            continue;
        }
        // Dedup per incident so a flapping monitor cannot re-alert for the same
        // outage; recovery keys on the transition itself.
        let dedup = match incident_id {
            Some(id) => format!("sub:{}:incident:{}:{}", s.id, id, trigger),
            None => format!("sub:{}:monitor:{}:{}", s.id, m.id, trigger),
        };
        prepared.push((s.id, dedup, s.throttle_seconds));
    }
    if prepared.is_empty() {
        return;
    }

    let mut claimed: Vec<(uuid::Uuid, String)> = Vec::new();
    for (sub_id, dedup, throttle) in prepared {
        if throttle <= 0 {
            claimed.push((sub_id, dedup));
            continue;
        }
        let redis_key = format!("sauron:notify:{sub_id}:{dedup}");
        let ok = match tokio::time::timeout(
            Duration::from_millis(250),
            notifier.redis.set_nx_ex(&redis_key, "1", throttle as u64),
        )
        .await
        {
            Ok(Ok(true)) => true,
            Ok(Ok(false)) => false,
            // Redis unreachable or slow: fall through to the durable check.
            _ => match sauron_db::conn(&notifier.pool).await {
                Ok(mut c) => !repo::notification_recently_queued(&mut c, sub_id, &dedup, throttle)
                    .await
                    .unwrap_or(false),
                Err(_) => false,
            },
        };
        if ok {
            claimed.push((sub_id, dedup));
        }
    }
    if claimed.is_empty() {
        return;
    }

    let down = trigger == "monitor_down";
    let title = if down {
        format!("Monitor down: {}", m.name)
    } else {
        format!("Monitor recovered: {}", m.name)
    };
    let body = if down {
        format!(
            "{} ({}) is {} — {}",
            m.name,
            m.target,
            status,
            cause.unwrap_or("check failed")
        )
    } else {
        format!("{} ({}) recovered and is UP again.", m.name, m.target)
    };
    let severity = if down { "critical" } else { "info" };

    let rows: Vec<repo::QueueInsert> = claimed
        .iter()
        .map(|(sub_id, dedup)| repo::QueueInsert {
            subscription_id: *sub_id,
            project_id: m.project_id,
            app_id: None,
            includes_unattributed: false,
            kind: "uptime",
            dedup_key: dedup,
            severity,
            title: &title,
            body: &body,
            link: None,
            env_enrollments: Vec::new(),
        })
        .collect();

    match sauron_db::conn(&notifier.pool).await {
        Ok(mut c) => match repo::enqueue_notifications(&mut c, &rows).await {
            Ok(n) if n > 0 => info!(enqueued = n, "personal uptime notifications enqueued"),
            Ok(_) => {}
            Err(e) => warn!(error = %e, "enqueueing personal uptime notifications failed"),
        },
        Err(e) => warn!(error = %e, "uptime subscriptions: no db connection for enqueue"),
    }
}
```

Add `info` to the file's `use tracing::{…}` list if only `warn` is imported.

- [ ] **Step 7: Check the workspace and run the DB tests.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db --test notifications`
  Expected: compiles; every DB test passes.

- [ ] **Step 8: Clippy and fmt.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
  Expected: clean.

---

## Task 20: Dashboard types and the pure model module

**Files:**
- Modify `dashboard/src/lib/models/index.ts` (after `AlertMeta` at line 1016)
- Create `dashboard/src/lib/models/notification-prefs.ts`
- Create `dashboard/src/lib/models/notification-prefs.test.ts`

**Interfaces:**
- Consumes: `ScopeSelection` from `dashboard/src/lib/models/scope-tree.ts` (`{ org: boolean; projects: string[]; apps: string[]; envs: string[] }`).
- Produces (all exported from `notification-prefs.ts`):
  - `selectionToSubscriptionScope(sel: ScopeSelection): { ok: true; scope_type: 'project' | 'app'; scope_id: string } | { ok: false; reason: string }`
  - `kindSupportsEnvFilter(kind: SubscriptionKind): boolean`
  - `kindScopeTypes(kind: SubscriptionKind): ('project' | 'app')[]`
  - `clampConditions(kind: SubscriptionKind, raw: Partial<SubscriptionConditions>): SubscriptionConditions`
  - `describeSubscription(s: NotificationSubscription): string`
  - `quietHoursLabel(start: number | null, end: number | null, tz: string): string`
  - `validateSubscription(input: SubscriptionDraft): string[]`

- [ ] **Step 1: Add the types.** In `dashboard/src/lib/models/index.ts`, append after the `AlertMeta` interface:

```ts
export type SubscriptionKind =
  | 'uptime'
  | 'error_spike'
  | 'error_new_issue'
  | 'error_regression';

export type SubscriptionDelivery = 'immediate' | 'hourly' | 'daily';

export interface SubscriptionConditions {
  window_seconds: number;
  factor: number;
  min_count: number;
  level: string | null;
}

export interface NotificationSubscription {
  id: string;
  scope_type: 'project' | 'app';
  scope_id: string;
  /** Best effort: `scope_id` has no foreign key, so a row can outlive its target. */
  scope_name: string | null;
  project_id: string | null;
  kind: SubscriptionKind;
  enabled: boolean;
  disabled_reason: 'unsubscribed' | 'access_revoked' | null;
  /** CATALOGUE environment ids (`environments.id`), never enrollment ids. */
  environment_ids: string[];
  conditions: Partial<SubscriptionConditions>;
  delivery: SubscriptionDelivery;
  /** What the user will actually get once the per-hour cap is applied. */
  effective_delivery: SubscriptionDelivery;
  throttle_seconds: number;
  quiet_start_min: number | null;
  quiet_end_min: number | null;
  quiet_tz: string;
  created_at: string;
}

export interface NotificationQueueItem {
  id: string;
  kind: SubscriptionKind;
  severity: AlertSeverity;
  title: string | null;
  body: string | null;
  link: string | null;
  status: string;
  occurred_at: string;
  sent_at: string | null;
}

export interface SubscriptionKindMeta {
  key: SubscriptionKind;
  scope_types: ('project' | 'app')[];
  env_filter: boolean;
  defaults: Partial<SubscriptionConditions>;
  clamps: Record<string, [number, number]>;
}
```

and add one field to the existing `AlertMeta` interface:

```ts
  subscription_kinds: SubscriptionKindMeta[];
```

- [ ] **Step 2: Write the failing tests.** Create `dashboard/src/lib/models/notification-prefs.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import {
  clampConditions,
  describeSubscription,
  kindScopeTypes,
  kindSupportsEnvFilter,
  quietHoursLabel,
  selectionToSubscriptionScope,
  validateSubscription,
} from './notification-prefs';
import type { NotificationSubscription } from './index';

describe('selectionToSubscriptionScope', () => {
  it('accepts exactly one project or one app', () => {
    expect(
      selectionToSubscriptionScope({ org: false, projects: ['p1'], apps: [], envs: [] }),
    ).toEqual({ ok: true, scope_type: 'project', scope_id: 'p1' });
    expect(
      selectionToSubscriptionScope({ org: false, projects: [], apps: ['a1'], envs: [] }),
    ).toEqual({ ok: true, scope_type: 'app', scope_id: 'a1' });
  });

  it('rejects a multi-node selection', () => {
    // Subscriptions are one row per scope, not a collapsed grant set, so
    // grant-plan.ts's coverage-diff machinery is deliberately not reused.
    const r = selectionToSubscriptionScope({
      org: false,
      projects: ['p1'],
      apps: ['a1'],
      envs: [],
    });
    expect(r.ok).toBe(false);
  });

  it('rejects an org selection', () => {
    // One org tick would fan out to every app in the org.
    const r = selectionToSubscriptionScope({ org: true, projects: [], apps: [], envs: [] });
    expect(r.ok).toBe(false);
  });

  it('rejects a non-empty envs array rather than ignoring it', () => {
    // ScopeTree's env rows are ENROLLMENT ids; a subscription stores CATALOGUE
    // ids. Silently dropping them would put two id spaces in one form. Failing
    // loudly is what catches a regression that re-enables the level.
    const r = selectionToSubscriptionScope({
      org: false,
      projects: [],
      apps: ['a1'],
      envs: ['e1'],
    });
    expect(r.ok).toBe(false);
  });

  it('rejects an empty selection', () => {
    expect(
      selectionToSubscriptionScope({ org: false, projects: [], apps: [], envs: [] }).ok,
    ).toBe(false);
  });
});

describe('kind metadata', () => {
  it('uptime has no environment filter and is project-only', () => {
    expect(kindSupportsEnvFilter('uptime')).toBe(false);
    expect(kindScopeTypes('uptime')).toEqual(['project']);
  });

  it('the error kinds narrow by environment and accept both scope types', () => {
    for (const k of ['error_spike', 'error_new_issue', 'error_regression'] as const) {
      expect(kindSupportsEnvFilter(k)).toBe(true);
      expect(kindScopeTypes(k)).toEqual(['project', 'app']);
    }
  });
});

describe('clampConditions', () => {
  // These numbers are hardcoded on purpose and duplicate the backend's clamps
  // exactly. A mismatch is the drift this test exists to catch.
  it('matches the backend clamps', () => {
    expect(clampConditions('error_spike', { window_seconds: 5 }).window_seconds).toBe(300);
    expect(clampConditions('error_spike', { window_seconds: 999999 }).window_seconds).toBe(86400);
    expect(clampConditions('error_spike', { factor: 0.1 }).factor).toBe(1.5);
    expect(clampConditions('error_spike', { factor: 900 }).factor).toBe(100);
    expect(clampConditions('error_spike', { min_count: 0 }).min_count).toBe(1);
    expect(clampConditions('error_spike', { min_count: 9999999 }).min_count).toBe(100000);
  });

  it('applies the documented defaults', () => {
    const c = clampConditions('error_spike', {});
    expect(c.window_seconds).toBe(900);
    expect(c.factor).toBe(3);
    expect(c.min_count).toBe(10);
    expect(c.level).toBeNull();
    expect(clampConditions('error_new_issue', {}).level).toBe('error');
    expect(clampConditions('error_regression', {}).level).toBe('error');
  });

  it('rejects a non-finite factor', () => {
    expect(clampConditions('error_spike', { factor: Number.NaN }).factor).toBe(3);
    expect(clampConditions('error_spike', { factor: Number.POSITIVE_INFINITY }).factor).toBe(3);
  });
});

describe('quietHoursLabel', () => {
  it('renders a window with its effective zone', () => {
    expect(quietHoursLabel(1320, 360, 'Europe/Paris')).toBe('22:00 – 06:00 (Europe/Paris)');
    expect(quietHoursLabel(null, null, 'UTC')).toBe('Always on');
    expect(quietHoursLabel(1320, null, 'UTC')).toBe('Always on');
  });
});

describe('validateSubscription', () => {
  it('enumerates every reason the save button is disabled', () => {
    expect(
      validateSubscription({
        kind: 'error_spike',
        selection: { org: false, projects: [], apps: [], envs: [] },
        environmentIds: [],
        conditions: {},
        delivery: 'immediate',
        throttleSeconds: 900,
        quietStartMin: null,
        quietEndMin: null,
        quietTz: 'UTC',
      }),
    ).toContain('Pick one project or one app.');

    expect(
      validateSubscription({
        kind: 'uptime',
        selection: { org: false, projects: [], apps: ['a1'], envs: [] },
        environmentIds: [],
        conditions: {},
        delivery: 'immediate',
        throttleSeconds: 900,
        quietStartMin: null,
        quietEndMin: null,
        quietTz: 'UTC',
      }),
    ).toContain('Uptime subscriptions are project-scoped.');

    expect(
      validateSubscription({
        kind: 'error_spike',
        selection: { org: false, projects: ['p1'], apps: [], envs: [] },
        environmentIds: [],
        conditions: {},
        delivery: 'immediate',
        throttleSeconds: 900,
        quietStartMin: 1320,
        quietEndMin: null,
        quietTz: 'UTC',
      }),
    ).toContain('Set both a quiet-hours start and end, or neither.');

    expect(
      validateSubscription({
        kind: 'error_spike',
        selection: { org: false, projects: ['p1'], apps: [], envs: [] },
        environmentIds: [],
        conditions: {},
        delivery: 'immediate',
        throttleSeconds: -1,
        quietStartMin: null,
        quietEndMin: null,
        quietTz: 'UTC',
      }),
    ).toContain('Throttle must be between 0 and 604800 seconds.');

    expect(
      validateSubscription({
        kind: 'error_spike',
        selection: { org: false, projects: ['p1'], apps: [], envs: [] },
        environmentIds: [],
        conditions: {},
        delivery: 'immediate',
        throttleSeconds: 900,
        quietStartMin: null,
        quietEndMin: null,
        quietTz: 'UTC',
      }),
    ).toEqual([]);
  });
});

describe('describeSubscription', () => {
  it('names the scope and falls back when the target is gone', () => {
    const base: NotificationSubscription = {
      id: 's1',
      scope_type: 'project',
      scope_id: 'p1',
      scope_name: 'Checkout',
      project_id: 'p1',
      kind: 'error_spike',
      enabled: true,
      disabled_reason: null,
      environment_ids: [],
      conditions: {},
      delivery: 'immediate',
      effective_delivery: 'immediate',
      throttle_seconds: 900,
      quiet_start_min: null,
      quiet_end_min: null,
      quiet_tz: 'UTC',
      created_at: '2026-08-01T00:00:00Z',
    };
    expect(describeSubscription(base)).toBe('Project “Checkout”');
    expect(describeSubscription({ ...base, scope_name: null })).toBe('Project (deleted)');
    expect(
      describeSubscription({ ...base, scope_type: 'app', scope_name: 'web' }),
    ).toBe('App “web”');
  });
});
```

- [ ] **Step 3: Run them and watch them fail.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected: `Failed to resolve import "./notification-prefs"`.

- [ ] **Step 4: Implement.** Create `dashboard/src/lib/models/notification-prefs.ts`:

```ts
/**
 * Pure decision logic for personal notification subscriptions.
 *
 * DOM-free on purpose: there is no DOM test environment in this project, so
 * anything a `.svelte` file decides is untestable. The `.svelte` files render;
 * this file decides.
 */
import type {
  NotificationSubscription,
  SubscriptionConditions,
  SubscriptionDelivery,
  SubscriptionKind,
} from './index';
import type { ScopeSelection } from './scope-tree';

/** These duplicate the backend clamps exactly; a mismatch is drift, not style. */
export const COND_DEFAULTS = {
  window_seconds: 900,
  factor: 3,
  min_count: 10,
} as const;
export const COND_CLAMPS = {
  window_seconds: [300, 86400],
  factor: [1.5, 100],
  min_count: [1, 100000],
} as const;
export const MAX_THROTTLE_SECONDS = 604800;

export type ScopeResult =
  | { ok: true; scope_type: 'project' | 'app'; scope_id: string }
  | { ok: false; reason: string };

/**
 * Collapse a `ScopeTree` selection into the single scope a subscription can
 * carry.
 *
 * A subscription is one row per scope, not a collapsed grant set, so
 * `grant-plan.ts`'s coverage-diff machinery is deliberately not reused — a
 * multi-node selection is refused rather than merged.
 */
export function selectionToSubscriptionScope(sel: ScopeSelection): ScopeResult {
  if (sel.org) {
    return { ok: false, reason: 'Pick one project or one app.' };
  }
  if (sel.envs.length > 0) {
    // ScopeTree's env rows are `AppEnvironment.id` — ENROLLMENT ids — while a
    // subscription stores CATALOGUE ids in a separate chip row. Rejecting
    // rather than ignoring is what makes a regression that re-enables the env
    // level fail loudly instead of storing the wrong id space.
    return { ok: false, reason: 'Pick one project or one app.' };
  }
  const picked = sel.projects.length + sel.apps.length;
  if (picked !== 1) {
    return { ok: false, reason: 'Pick one project or one app.' };
  }
  if (sel.projects.length === 1) {
    return { ok: true, scope_type: 'project', scope_id: sel.projects[0] };
  }
  return { ok: true, scope_type: 'app', scope_id: sel.apps[0] };
}

/** `monitors` carries only `project_id`, so uptime has nothing to narrow on. */
export function kindSupportsEnvFilter(kind: SubscriptionKind): boolean {
  return kind !== 'uptime';
}

export function kindScopeTypes(kind: SubscriptionKind): ('project' | 'app')[] {
  return kind === 'uptime' ? ['project'] : ['project', 'app'];
}

function clampNumber(value: number | undefined, fallback: number, lo: number, hi: number): number {
  if (value === undefined || value === null || !Number.isFinite(value)) return fallback;
  return Math.min(hi, Math.max(lo, value));
}

export function clampConditions(
  kind: SubscriptionKind,
  raw: Partial<SubscriptionConditions>,
): SubscriptionConditions {
  const defaultLevel =
    kind === 'error_new_issue' || kind === 'error_regression' ? 'error' : null;
  return {
    window_seconds: clampNumber(
      raw.window_seconds,
      COND_DEFAULTS.window_seconds,
      COND_CLAMPS.window_seconds[0],
      COND_CLAMPS.window_seconds[1],
    ),
    factor: clampNumber(
      raw.factor,
      COND_DEFAULTS.factor,
      COND_CLAMPS.factor[0],
      COND_CLAMPS.factor[1],
    ),
    min_count: clampNumber(
      raw.min_count,
      COND_DEFAULTS.min_count,
      COND_CLAMPS.min_count[0],
      COND_CLAMPS.min_count[1],
    ),
    level: raw.level === undefined ? defaultLevel : raw.level,
  };
}

export function describeSubscription(s: NotificationSubscription): string {
  const noun = s.scope_type === 'project' ? 'Project' : 'App';
  // `scope_id` has no foreign key, so the target can be gone. Say so instead of
  // rendering a bare uuid nobody can act on.
  return s.scope_name ? `${noun} “${s.scope_name}”` : `${noun} (deleted)`;
}

function hhmm(minuteOfDay: number): string {
  const h = Math.floor(minuteOfDay / 60);
  const m = minuteOfDay % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
}

/**
 * Renders the EFFECTIVE zone, which is what the enqueue actually used — a zone
 * the server does not know falls back to UTC there, and this is where a user
 * would notice.
 */
export function quietHoursLabel(
  start: number | null,
  end: number | null,
  tz: string,
): string {
  if (start === null || end === null) return 'Always on';
  return `${hhmm(start)} – ${hhmm(end)} (${tz})`;
}

export interface SubscriptionDraft {
  kind: SubscriptionKind;
  selection: ScopeSelection;
  environmentIds: string[];
  conditions: Partial<SubscriptionConditions>;
  delivery: SubscriptionDelivery;
  throttleSeconds: number;
  quietStartMin: number | null;
  quietEndMin: number | null;
  quietTz: string;
}

/** Every reason the save button is disabled, in the order they are shown. */
export function validateSubscription(input: SubscriptionDraft): string[] {
  const reasons: string[] = [];
  const scope = selectionToSubscriptionScope(input.selection);
  if (!scope.ok) {
    reasons.push(scope.reason);
  } else if (!kindScopeTypes(input.kind).includes(scope.scope_type)) {
    reasons.push('Uptime subscriptions are project-scoped.');
  }
  if ((input.quietStartMin === null) !== (input.quietEndMin === null)) {
    reasons.push('Set both a quiet-hours start and end, or neither.');
  }
  for (const v of [input.quietStartMin, input.quietEndMin]) {
    if (v !== null && (v < 0 || v > 1439)) {
      reasons.push('Quiet hours must be times of day.');
      break;
    }
  }
  if (
    !Number.isFinite(input.throttleSeconds) ||
    input.throttleSeconds < 0 ||
    input.throttleSeconds > MAX_THROTTLE_SECONDS
  ) {
    reasons.push(`Throttle must be between 0 and ${MAX_THROTTLE_SECONDS} seconds.`);
  }
  if (!input.quietTz.trim()) {
    reasons.push('Pick a timezone for quiet hours.');
  }
  return reasons;
}
```

- [ ] **Step 5: Run the tests and watch them pass.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected: every `notification-prefs.test.ts` case green, and no regressions elsewhere.

- [ ] **Step 6: Typecheck.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`
  Expected: 0 errors.

---

## Task 21: The dashboard API module

**Files:**
- Create `dashboard/src/lib/api/notification-prefs.ts`

**Interfaces:**
- Consumes: `api` and `bareClient` from `dashboard/src/lib/api/client.ts`; the types from Task 20.
- Produces: `listSubscriptions`, `createSubscription`, `updateSubscription`, `deleteSubscription`, `listNotifications`, `unsubscribe`.

- [ ] **Step 1: Write the module.** Create `dashboard/src/lib/api/notification-prefs.ts`:

```ts
/**
 * One thin wrapper per endpoint, on the `api/alerts.ts` template.
 *
 * `api` for `/v1/me/*` (bearer + the 401 refresh-and-replay interceptor);
 * `bareClient` for the unsubscribe POST, which is unauthenticated and must
 * never be retried through the refresh path.
 */
import { api, bareClient } from './client';
import type {
  NotificationQueueItem,
  NotificationSubscription,
  SubscriptionConditions,
  SubscriptionDelivery,
  SubscriptionKind,
} from '../models';

export async function listSubscriptions(): Promise<NotificationSubscription[]> {
  const { data } = await api.get<NotificationSubscription[]>(
    '/v1/me/notification-subscriptions',
  );
  return data;
}

export interface UpsertSubscriptionBody {
  scope_type: 'project' | 'app';
  scope_id: string;
  kind: SubscriptionKind;
  /** CATALOGUE environment ids. `[]` means every environment. */
  environment_ids: string[];
  conditions: Partial<SubscriptionConditions>;
  delivery: SubscriptionDelivery;
  throttle_seconds: number;
  quiet_start_min: number | null;
  quiet_end_min: number | null;
  quiet_tz: string;
}

export async function createSubscription(
  body: UpsertSubscriptionBody,
): Promise<NotificationSubscription> {
  // There is no `org_id` field and there never will be one: the server derives
  // the org from the scope itself.
  const { data } = await api.post<NotificationSubscription>(
    '/v1/me/notification-subscriptions',
    body,
  );
  return data;
}

export async function updateSubscription(
  id: string,
  body: Partial<UpsertSubscriptionBody> & { enabled?: boolean },
): Promise<NotificationSubscription> {
  const { data } = await api.patch<NotificationSubscription>(
    `/v1/me/notification-subscriptions/${id}`,
    body,
  );
  return data;
}

export async function deleteSubscription(id: string): Promise<void> {
  await api.delete(`/v1/me/notification-subscriptions/${id}`);
}

export async function listNotifications(limit = 50): Promise<NotificationQueueItem[]> {
  const { data } = await api.get<NotificationQueueItem[]>('/v1/me/notifications', {
    params: { limit },
  });
  return data;
}

/**
 * Always resolves for any token the server accepted the request for — the
 * endpoint returns a generic 200 whether or not the token matched, so nothing
 * is disclosed about which subscription ids exist.
 */
export async function unsubscribe(token: string): Promise<void> {
  await bareClient.post('/v1/notifications/unsubscribe', { token });
}
```

- [ ] **Step 2: Typecheck and watch it pass.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`
  Expected: 0 errors. A `Cannot find name 'NotificationSubscription'` here means Task 20's `models/index.ts` edit is missing.

- [ ] **Step 3: Run the test suite for regressions.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected: green.

---

## Task 22: `ScopeTree` gains `allowOrg` and `allowEnv`

**Files:**
- Modify `dashboard/src/lib/components/members/ScopeTree.svelte` (the `Props` interface at lines 11-35, the destructure at 37-49, the org row at ~185, the env rows)
- Modify `dashboard/src/lib/models/notification-prefs.test.ts`

**Interfaces:**
- Consumes: nothing new.
- Produces: two additive optional props, both defaulting to `true` so `Members.svelte` and `EditMemberDialog` behaviour is byte-identical.

- [ ] **Step 1: Write the failing test.** Append to `dashboard/src/lib/models/notification-prefs.test.ts`:

```ts
describe('the subscription dialog never offers an org or an environment row', () => {
  it('a selection carrying either is refused by the model, not silently trimmed', () => {
    // `ScopeTree` gains `allowOrg`/`allowEnv` so the dialog cannot produce
    // these — but the model refuses them anyway, so a regression that
    // re-enables the level fails loudly at save time rather than storing an
    // enrollment id where a catalogue id belongs.
    expect(
      selectionToSubscriptionScope({ org: true, projects: [], apps: [], envs: [] }).ok,
    ).toBe(false);
    expect(
      selectionToSubscriptionScope({ org: false, projects: ['p'], apps: [], envs: ['e'] }).ok,
    ).toBe(false);
  });
});
```

- [ ] **Step 2: Run it.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected: passes on Task 20's implementation. This test is the model-side guard; the component change below is what makes the UI match it.

- [ ] **Step 3: Add the props.** In `dashboard/src/lib/components/members/ScopeTree.svelte`, add to the `Props` interface after `disabled?: boolean;`:

```ts
    /**
     * Whether the "entire org" row is offered. Defaults to true so Members and
     * EditMember are unchanged; the subscription dialog passes false, because
     * one org tick would fan a subscription out to every app in the org.
     */
    allowOrg?: boolean;
    /**
     * Whether the environment level is offered under each app. Defaults to
     * true. The subscription dialog passes false: these rows are
     * `AppEnvironment.id` — ENROLLMENT ids — while a subscription stores
     * CATALOGUE ids in its own chip row, and rendering both with identical
     * labels would put two id spaces in one form.
     */
    allowEnv?: boolean;
```

and to the destructure, after `disabled = false,`:

```ts
    allowOrg = true,
    allowEnv = true,
```

- [ ] **Step 4: Gate the two levels in the markup.** Three edits in `ScopeTree.svelte`, all additive.

  **4a — the org row (lines 182-190).** Replace:

```svelte
    <div class="row">
      <span class="twisty-gap"></span>
      <label class="node">
        <input type="checkbox" checked={value.org} {disabled} onchange={toggleOrg} />
        <span class="n-name">{orgName}</span>
        <span class="n-hint">entire org</span>
      </label>
    </div>
```

  with:

```svelte
    {#if allowOrg}
      <div class="row">
        <span class="twisty-gap"></span>
        <label class="node">
          <input type="checkbox" checked={value.org} {disabled} onchange={toggleOrg} />
          <span class="n-name">{orgName}</span>
          <span class="n-hint">entire org</span>
        </label>
      </div>
    {/if}
```

  **4b — the app row's disclosure twisty (lines 238-256).** The comment on it says it is always rendered because an app's env count is unknown until fetched. With `allowEnv = false` there is nothing to disclose at all, so replace the `<button class="twisty">…</button>` inside `<div class="row lvl-2">` with:

```svelte
            {#if allowEnv}
              <!-- Always rendered when environments are offered, unlike the
                   project twisty above: an app's env count is unknown until
                   fetched, so the disclosure can't be conditionally hidden the
                   way an empty project's can. With `allowEnv = false` there is
                   nothing to disclose, and an expander that opens onto nothing
                   reads as a broken control. -->
              <button
                type="button"
                class="twisty"
                aria-expanded={appOpen}
                aria-label={`${appOpen ? 'Collapse' : 'Expand'} ${app.name}`}
                onclick={() => toggleOpenApp(app.id)}
              >
                {#if envsLoading}
                  <Spinner size={11} stroke={1.5} />
                {:else}
                  <Icon name={appOpen ? 'chevron-down' : 'chevron-right'} size={13} />
                {/if}
              </button>
            {:else}
              <span class="twisty-gap"></span>
            {/if}
```

  and drop the `{#if envs.length}<span class="n-hint">…env…</span>{/if}` hint in the same `<label>` to `{#if allowEnv && envs.length}` — an env count under an app whose environments are not selectable is a promise the tree does not keep.

  **4c — the environment rows (lines 268-292).** Change the opening guard from `{#if appOpen}` to `{#if allowEnv && appOpen}`. That one condition covers the `{#each envs}` block, the `envsLoading` row and the `No environments.` empty state in a single edit — none of the three should render when the level is not offered.

- [ ] **Step 5: Typecheck and run the tests.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check && npm run test`
  Expected: 0 type errors; all tests green, including the existing `scope-tree.test.ts`.

- [ ] **Step 6: Confirm the existing callers are untouched.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && grep -rn "ScopeTree" src/lib/components/members/`
  Expected: `CreateMemberDialog` and `EditMemberDialog` still pass no `allowOrg`/`allowEnv` — both default to `true`, so their behaviour is unchanged.

---

## Task 23: `SubscriptionDialog.svelte`

**Files:**
- Create `dashboard/src/lib/components/account/SubscriptionDialog.svelte`

**Interfaces:**
- Consumes: `Modal`, `Button`, `Input` from `../ui/`; `ScopeTree` from `../members/ScopeTree.svelte`; `EMPTY_SELECTION` and `ScopeSelection` from `../../models/scope-tree`; `validateSubscription`, `clampConditions`, `kindSupportsEnvFilter`, `selectionToSubscriptionScope` from `../../models/notification-prefs`; `createSubscription`/`updateSubscription` from `../../api/notification-prefs`; `listProjectEnvironments` from `../../api/environments`.
- Produces: a component with props `{ open, orgId, orgName, projects, appsByProject, envsByApp, existing, onsaved, onclose }`.

- [ ] **Step 1: Create the component.** Create `dashboard/src/lib/components/account/SubscriptionDialog.svelte`:

```svelte
<script lang="ts">
  import { untrack } from 'svelte';
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import ScopeTree from '../members/ScopeTree.svelte';
  import { EMPTY_SELECTION, type ScopeSelection } from '../../models/scope-tree';
  import {
    clampConditions,
    kindSupportsEnvFilter,
    selectionToSubscriptionScope,
    validateSubscription,
  } from '../../models/notification-prefs';
  import { createSubscription, updateSubscription } from '../../api/notification-prefs';
  import type {
    NotificationSubscription,
    SubscriptionDelivery,
    SubscriptionKind,
  } from '../../models';
  import { toastStore } from '../../stores/toast.svelte';

  interface Props {
    open: boolean;
    orgId: string;
    orgName: string;
    projects: { id: string; name: string }[];
    appsByProject: Record<string, { id: string; name: string }[]>;
    /** Catalogue environments per project id, loaded on demand by the parent. */
    catalogueEnvsByProject: Record<string, { id: string; name: string }[]>;
    existing: NotificationSubscription | null;
    onopenproject: (projectId: string) => void;
    onsaved: () => void;
    onclose: () => void;
  }

  let {
    open = $bindable(false),
    orgId,
    orgName,
    projects,
    appsByProject,
    catalogueEnvsByProject,
    existing,
    onopenproject,
    onsaved,
    onclose,
  }: Props = $props();

  let kind = $state<SubscriptionKind>('error_spike');
  let selection = $state<ScopeSelection>(EMPTY_SELECTION);
  let environmentIds = $state<string[]>([]);
  let windowSeconds = $state('900');
  let factor = $state('3');
  let minCount = $state('10');
  let level = $state('');
  let delivery = $state<SubscriptionDelivery>('immediate');
  let throttleSeconds = $state('900');
  let quietStart = $state('');
  let quietEnd = $state('');
  let quietTz = $state('UTC');
  let saving = $state(false);
  let error = $state('');

  // Reseeding from props inside `untrack` so a parent reload — which replaces
  // `existing` with an equal-but-not-identical object — cannot wipe a
  // half-finished edit mid-typing.
  $effect(() => {
    const src = existing;
    const isOpen = open;
    untrack(() => {
      if (!isOpen) return;
      if (src) {
        kind = src.kind;
        selection =
          src.scope_type === 'project'
            ? { org: false, projects: [src.scope_id], apps: [], envs: [] }
            : { org: false, projects: [], apps: [src.scope_id], envs: [] };
        environmentIds = [...src.environment_ids];
        windowSeconds = String(src.conditions.window_seconds ?? 900);
        factor = String(src.conditions.factor ?? 3);
        minCount = String(src.conditions.min_count ?? 10);
        level = src.conditions.level ?? '';
        delivery = src.delivery;
        throttleSeconds = String(src.throttle_seconds);
        quietStart = src.quiet_start_min === null ? '' : String(src.quiet_start_min);
        quietEnd = src.quiet_end_min === null ? '' : String(src.quiet_end_min);
        quietTz = src.quiet_tz;
      } else {
        kind = 'error_spike';
        selection = EMPTY_SELECTION;
        environmentIds = [];
        windowSeconds = '900';
        factor = '3';
        minCount = '10';
        level = '';
        delivery = 'immediate';
        throttleSeconds = '900';
        quietStart = '';
        quietEnd = '';
        quietTz = 'UTC';
      }
      error = '';
    });
  });

  // Copied in shape from Alerts.svelte's `triggerNeeds` — which fields a kind
  // actually uses, decided in one place.
  const needs = $derived({
    spike: kind === 'error_spike',
    level: kind !== 'uptime',
    envFilter: kindSupportsEnvFilter(kind),
  });

  const scopeProjectId = $derived.by(() => {
    const s = selectionToSubscriptionScope(selection);
    if (!s.ok) return null;
    if (s.scope_type === 'project') return s.scope_id;
    for (const [pid, apps] of Object.entries(appsByProject)) {
      if (apps.some((a) => a.id === s.scope_id)) return pid;
    }
    return null;
  });

  $effect(() => {
    const pid = scopeProjectId;
    if (pid && !catalogueEnvsByProject[pid]) onopenproject(pid);
  });

  const offeredEnvs = $derived(
    scopeProjectId ? (catalogueEnvsByProject[scopeProjectId] ?? []) : [],
  );

  const draft = $derived({
    kind,
    selection,
    environmentIds,
    conditions: {
      window_seconds: Number(windowSeconds),
      factor: Number(factor),
      min_count: Number(minCount),
      level: level ? level : null,
    },
    delivery,
    throttleSeconds: Number(throttleSeconds),
    quietStartMin: quietStart === '' ? null : Number(quietStart),
    quietEndMin: quietEnd === '' ? null : Number(quietEnd),
    quietTz,
  });

  const problems = $derived(validateSubscription(draft));

  function toggleEnv(id: string) {
    // Replaced, never mutated: `$state` arrays are proxies and an in-place
    // push does not always re-derive downstream.
    environmentIds = environmentIds.includes(id)
      ? environmentIds.filter((e) => e !== id)
      : [...environmentIds, id];
  }

  async function save() {
    const scope = selectionToSubscriptionScope(selection);
    if (!scope.ok) return;
    saving = true;
    error = '';
    try {
      const body = {
        scope_type: scope.scope_type,
        scope_id: scope.scope_id,
        kind,
        environment_ids: needs.envFilter ? environmentIds : [],
        conditions: clampConditions(kind, draft.conditions),
        delivery,
        throttle_seconds: Number(throttleSeconds),
        quiet_start_min: draft.quietStartMin,
        quiet_end_min: draft.quietEndMin,
        quiet_tz: quietTz,
      };
      if (existing) {
        await updateSubscription(existing.id, body);
      } else {
        await createSubscription(body);
      }
      toastStore.success('Subscription saved');
      onsaved();
    } catch (e) {
      error = e instanceof Error ? e.message : 'Could not save the subscription';
    } finally {
      saving = false;
    }
  }
</script>

<Modal bind:open size="lg" title={existing ? 'Edit subscription' : 'New subscription'} {onclose}>
  <div class="form">
    <label class="fld">
      <span class="lbl">Notify me about</span>
      <!-- A raw select: there is no Select primitive in lib/components/ui. -->
      <select class="sel" bind:value={kind}>
        <option value="error_spike">Error rate increasing</option>
        <option value="error_new_issue">A new issue appears</option>
        <option value="error_regression">A resolved issue regresses</option>
        <option value="uptime">A monitor goes down or recovers</option>
      </select>
    </label>

    <div class="fld">
      <span class="lbl">Scope</span>
      <ScopeTree
        {orgId}
        {orgName}
        {projects}
        {appsByProject}
        envsByApp={{}}
        allowOrg={false}
        allowEnv={false}
        value={selection}
        onchange={(next) => (selection = next)}
        onopenapp={() => {}}
      />
    </div>

    {#if needs.envFilter}
      <div class="fld">
        <span class="lbl">Environments</span>
        <p class="hint">Leave all unticked to be notified about every environment.</p>
        <div class="chips">
          {#each offeredEnvs as env (env.id)}
            <button
              type="button"
              class="chip"
              class:on={environmentIds.includes(env.id)}
              onclick={() => toggleEnv(env.id)}
            >{env.name}</button>
          {/each}
          {#if offeredEnvs.length === 0}
            <span class="hint">Pick a scope to choose environments.</span>
          {/if}
        </div>
      </div>
    {:else}
      <p class="hint">
        Monitors belong to a whole project, so the environment filter does not apply to uptime.
      </p>
    {/if}

    {#if needs.spike}
      <div class="row">
        <Input label="Window (seconds)" bind:value={windowSeconds} hint="300 – 86400" />
        <Input label="Increase factor" bind:value={factor} hint="1.5 – 100" />
        <Input label="Minimum errors" bind:value={minCount} hint="1 – 100000" />
      </div>
    {/if}

    {#if needs.level}
      <label class="fld">
        <span class="lbl">Level</span>
        <select class="sel" bind:value={level}>
          <option value="">Any level</option>
          <option value="error">error</option>
          <option value="warning">warning</option>
          <option value="fatal">fatal</option>
        </select>
      </label>
    {/if}

    <div class="row">
      <label class="fld">
        <span class="lbl">Delivery</span>
        <select class="sel" bind:value={delivery}>
          <option value="immediate">As it happens</option>
          <option value="hourly">Hourly summary</option>
          <option value="daily">Daily summary</option>
        </select>
      </label>
      <Input label="Throttle (seconds)" bind:value={throttleSeconds} hint="0 – 604800" />
    </div>

    <div class="row">
      <Input label="Quiet from (minute of day)" bind:value={quietStart} hint="e.g. 1320 = 22:00" />
      <Input label="Quiet until (minute of day)" bind:value={quietEnd} hint="e.g. 360 = 06:00" />
      <Input label="Timezone" bind:value={quietTz} hint="IANA name, e.g. Europe/Paris" />
    </div>
    <p class="hint">
      Quiet hours never drop a notification — they hold it until the window ends, so a
      night-time outage still reaches you in the morning.
    </p>

    {#if problems.length > 0}
      <ul class="problems">
        {#each problems as p (p)}<li>{p}</li>{/each}
      </ul>
    {/if}
    {#if error}<p class="err">{error}</p>{/if}
  </div>

  {#snippet footer()}
    <Button onclick={onclose}>Cancel</Button>
    <Button variant="primary" disabled={problems.length > 0} loading={saving} onclick={save}>
      Save
    </Button>
  {/snippet}
</Modal>

<style>
  .form { display: flex; flex-direction: column; gap: 16px; }
  .fld { display: flex; flex-direction: column; gap: 6px; }
  .lbl { font-size: 12px; font-weight: 600; color: var(--text-faint); }
  .row { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 12px; }
  .hint { font-size: 12px; color: var(--text-faint); margin: 0; }
  .sel {
    padding: 7px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface);
    color: var(--text);
    font-size: 13px;
  }
  .chips { display: flex; flex-wrap: wrap; gap: 6px; }
  .chip {
    padding: 4px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    background: var(--surface);
    color: var(--text-faint);
    font-size: 12px;
    cursor: pointer;
  }
  .chip.on { border-color: var(--accent); color: var(--text); }
  .problems { margin: 0; padding-left: 18px; font-size: 12px; color: var(--text-faint); }
  .err { font-size: 13px; color: var(--danger); margin: 0; }
</style>
```

- [ ] **Step 2: Typecheck.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`
  Expected: 0 errors. If `--danger` or `--accent` are not real CSS custom properties in this theme, substitute the ones `Button.svelte` uses.

- [ ] **Step 3: Run the tests for regressions.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected: green.

---

## Task 24: The Notifications card on `#/account`

**Files:**
- Create `dashboard/src/lib/components/account/NotificationSubscriptions.svelte`
- Modify `dashboard/src/pages/Account.svelte`

**Interfaces:**
- Consumes: `Card`, `Button`, `Badge`, `Spinner`, `EmptyState`, `ConfirmDialog`, `Icon` from `../ui/`; `DataTable` from `../DataTable.svelte`; `SubscriptionDialog` (Task 23); `listSubscriptions`, `updateSubscription`, `deleteSubscription` (Task 21); `describeSubscription`, `quietHoursLabel` (Task 20); `listProjects`, `listApps`, `listProjectEnvironments`, `listEnvironments` from the existing api modules; `sessionStore` from `../../stores/session.svelte` (the current org lives there, **not** on `authStore`).
- Produces: `<NotificationSubscriptions />`, taking no props.

- [ ] **Step 1: Create the card.** Create `dashboard/src/lib/components/account/NotificationSubscriptions.svelte`:

```svelte
<script lang="ts">
  import Card from '../ui/Card.svelte';
  import Button from '../ui/Button.svelte';
  import Badge from '../ui/Badge.svelte';
  import Spinner from '../ui/Spinner.svelte';
  import EmptyState from '../ui/EmptyState.svelte';
  import ConfirmDialog from '../ui/ConfirmDialog.svelte';
  import DataTable from '../DataTable.svelte';
  import SubscriptionDialog from './SubscriptionDialog.svelte';
  import {
    deleteSubscription,
    listSubscriptions,
    updateSubscription,
  } from '../../api/notification-prefs';
  import { listProjects } from '../../api/projects';
  import { listApps } from '../../api/apps';
  import { listEnvironments, listProjectEnvironments } from '../../api/environments';
  import { describeSubscription, quietHoursLabel } from '../../models/notification-prefs';
  import type { NotificationSubscription } from '../../models';
  import { sessionStore } from '../../stores/session.svelte';
  import { toastStore } from '../../stores/toast.svelte';

  const KIND_LABELS: Record<string, string> = {
    uptime: 'Uptime',
    error_spike: 'Error rate increasing',
    error_new_issue: 'New issue',
    error_regression: 'Issue regressed',
  };

  let subs = $state<NotificationSubscription[]>([]);
  let loading = $state(true);
  // Reads set a local error; mutations toast. Both conventions in one file.
  let loadError = $state('');
  let projects = $state<{ id: string; name: string }[]>([]);
  let appsByProject = $state<Record<string, { id: string; name: string }[]>>({});
  let catalogueEnvsByProject = $state<Record<string, { id: string; name: string }[]>>({});
  let dialogOpen = $state(false);
  let editing = $state<NotificationSubscription | null>(null);
  let confirming = $state<NotificationSubscription | null>(null);
  let busyId = $state('');

  // `authStore` carries authentication, not the org selection — the current org
  // lives on `sessionStore`, which is where every other page reads it from
  // (`pages/Members.svelte:385`).
  const orgId = $derived(sessionStore.currentOrg?.id ?? '');
  const orgName = $derived(sessionStore.currentOrg?.name ?? 'Organization');

  async function load() {
    loading = true;
    loadError = '';
    try {
      subs = await listSubscriptions();
      projects = (await listProjects(orgId)).map((p) => ({ id: p.id, name: p.name }));
      // Apps are loaded per project up front here (a personal account has far
      // fewer projects than an org member admin screen), but environments stay
      // on-demand: there is no batched org-wide environments endpoint.
      const next: Record<string, { id: string; name: string }[]> = {};
      for (const p of projects) {
        next[p.id] = (await listApps(p.id)).map((a) => ({ id: a.id, name: a.name }));
      }
      appsByProject = next;
    } catch (e) {
      loadError = e instanceof Error ? e.message : 'Could not load subscriptions';
    } finally {
      loading = false;
    }
  }

  async function loadEnvs(projectId: string) {
    if (catalogueEnvsByProject[projectId]) return;
    let envs: { id: string; name: string }[] = [];
    try {
      envs = (await listProjectEnvironments(projectId)).map((e) => ({ id: e.id, name: e.name }));
    } catch {
      // `GET /v1/projects/{id}/environments` is project-authorized, so it 403s
      // for an app-scoped member — who is precisely the member environment
      // narrowing exists for. Leaving the row empty would make the env chips
      // unreachable from the UI for exactly the users `covers()` arm 5 was
      // written to serve. `GET /v1/apps/{app_id}/environments` is `reach_for`-
      // based, so fall back to it and rebuild the CATALOGUE list from each
      // enrollment's `environment_id`.
      //
      // The chip value must stay a CATALOGUE id: the create/patch endpoints
      // validate `environment_ids` against the project's live catalogue and
      // reject an enrollment id outright. `AppEnvironment.id` is the enrollment
      // id and is the wrong one to send.
      const byId = new Map<string, string>();
      for (const app of appsByProject[projectId] ?? []) {
        try {
          for (const e of await listEnvironments(app.id)) {
            byId.set(e.environment_id, e.name);
          }
        } catch {
          // One unreachable app must not blank the whole picker: an app-scoped
          // member reaches some apps of this project and not others.
        }
      }
      envs = [...byId].map(([id, name]) => ({ id, name }));
    }
    // Replaced, never mutated: a Record inside `$state` is a proxy and an
    // in-place assignment does not reliably re-derive downstream.
    catalogueEnvsByProject = { ...catalogueEnvsByProject, [projectId]: envs };
  }

  async function toggle(s: NotificationSubscription) {
    busyId = s.id;
    try {
      await updateSubscription(s.id, { enabled: !s.enabled });
      toastStore.success(s.enabled ? 'Subscription disabled' : 'Subscription enabled');
      await load();
    } catch (e) {
      toastStore.error(e instanceof Error ? e.message : 'Could not update the subscription');
    } finally {
      busyId = '';
    }
  }

  async function remove() {
    const s = confirming;
    if (!s) return;
    busyId = s.id;
    try {
      await deleteSubscription(s.id);
      toastStore.success('Subscription deleted');
      confirming = null;
      await load();
    } catch (e) {
      toastStore.error(e instanceof Error ? e.message : 'Could not delete the subscription');
    } finally {
      busyId = '';
    }
  }

  $effect(() => {
    if (orgId) void load();
  });
</script>

<Card title="Notifications">
  {#snippet actions()}
    <Button
      variant="primary"
      size="sm"
      onclick={() => {
        editing = null;
        dialogOpen = true;
      }}
    >New subscription</Button>
  {/snippet}

  {#if loading}
    <Spinner />
  {:else if loadError}
    <p class="err">{loadError}</p>
  {:else if subs.length === 0}
    <EmptyState
      icon="bell"
      title="No personal notifications yet"
      description="Subscribe yourself to uptime or error notifications for a project or app. Only you see and control these."
    />
  {:else}
    <DataTable>
      {#snippet head()}
        <tr>
          <th>Scope</th>
          <th>Notify about</th>
          <th>Environments</th>
          <th>Delivery</th>
          <th>Quiet hours</th>
          <th>State</th>
          <th></th>
        </tr>
      {/snippet}
      {#snippet children()}
        {#each subs as s (s.id)}
          <tr>
            <td>{describeSubscription(s)}</td>
            <td>{KIND_LABELS[s.kind] ?? s.kind}</td>
            <td>{s.environment_ids.length === 0 ? 'All' : s.environment_ids.length}</td>
            <td>
              {s.effective_delivery}
              {#if s.effective_delivery !== s.delivery}
                <Badge tone="warning" size="sm">capped</Badge>
              {/if}
            </td>
            <td>{quietHoursLabel(s.quiet_start_min, s.quiet_end_min, s.quiet_tz)}</td>
            <td>
              {#if s.enabled}
                <Badge tone="success" size="sm">On</Badge>
              {:else if s.disabled_reason === 'access_revoked'}
                <!-- Explain rather than look broken: the subscription is off
                     because the owner lost access to its scope, and re-granting
                     access deliberately does not resurrect it. -->
                <Badge tone="warning" size="sm">Off — access removed</Badge>
              {:else}
                <Badge tone="neutral" size="sm">Off</Badge>
              {/if}
            </td>
            <td class="acts">
              <Button
                size="sm"
                disabled={busyId === s.id}
                onclick={() => {
                  editing = s;
                  dialogOpen = true;
                }}
              >Edit</Button>
              <Button size="sm" disabled={busyId === s.id} onclick={() => toggle(s)}>
                {s.enabled ? 'Disable' : 'Enable'}
              </Button>
              <Button size="sm" variant="danger" onclick={() => (confirming = s)}>Delete</Button>
            </td>
          </tr>
        {/each}
      {/snippet}
    </DataTable>
  {/if}
</Card>

<SubscriptionDialog
  bind:open={dialogOpen}
  {orgId}
  {orgName}
  {projects}
  {appsByProject}
  {catalogueEnvsByProject}
  existing={editing}
  onopenproject={(id) => void loadEnvs(id)}
  onsaved={() => {
    dialogOpen = false;
    void load();
  }}
  onclose={() => (dialogOpen = false)}
/>

<ConfirmDialog
  open={confirming !== null}
  title="Delete subscription"
  message="You will stop receiving these notifications. This does not affect anyone else."
  confirmLabel="Delete"
  danger
  loading={busyId === confirming?.id}
  onconfirm={remove}
  oncancel={() => (confirming = null)}
/>

<style>
  .err { font-size: 13px; color: var(--danger); margin: 0; }
  .acts { display: flex; gap: 6px; justify-content: flex-end; }
</style>
```

- [ ] **Step 2: Add the card to the account page.** In `dashboard/src/pages/Account.svelte`, import the component and render it as the last card in the container:

```svelte
  import NotificationSubscriptions from '../lib/components/account/NotificationSubscriptions.svelte';
```

```svelte
  <NotificationSubscriptions />
```

No `routes.ts` edit and no `Sidebar.svelte` edit: `#/account` already exists (S2) and this is a card on it, not a page. The `bell` icon is already registered at `Icon.svelte:64`, so the icon registry is untouched too.

- [ ] **Step 3: Typecheck.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`
  Expected: 0 errors. The four API signatures this file depends on are, verified against the tree: `listProjects(orgId: string): Promise<Project[]>` (`api/projects.ts:4`), `listApps(projectId: string): Promise<App[]>` (`api/apps.ts:4`), `listProjectEnvironments(projectId: string, includeRetired = false): Promise<ProjectEnvironment[]>` (`api/environments.ts:70`), `listEnvironments(appId: string, includeRetired = false): Promise<AppEnvironment[]>` (`api/environments.ts:26`).

- [ ] **Step 4: Run the tests.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected: green.

---

## Task 25: The `/unsubscribe` page

**Files:**
- Create `dashboard/src/pages/Unsubscribe.svelte`
- Modify `dashboard/src/routes.ts`

**Interfaces:**
- Consumes: `unsubscribe` from `../lib/api/notification-prefs` (Task 21); `querystring` from `svelte-spa-router`.
- Produces: the `/unsubscribe` route.

- [ ] **Step 1: Create the page.** Create `dashboard/src/pages/Unsubscribe.svelte`:

```svelte
<script lang="ts">
  import { querystring } from 'svelte-spa-router';
  import { get } from 'svelte/store';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import { unsubscribe } from '../lib/api/notification-prefs';

  // Read once at init, NEVER inside an effect: an effect that re-reads the
  // query string would re-POST the token on every unrelated store update.
  const token = new URLSearchParams(get(querystring) ?? '').get('token') ?? '';

  let state = $state<'working' | 'done' | 'missing'>(token ? 'working' : 'missing');

  $effect(() => {
    if (state !== 'working') return;
    void (async () => {
      try {
        await unsubscribe(token);
      } catch {
        // The endpoint answers a generic 200 whether or not the token matched,
        // so anything reaching here is a transport problem. Show the same
        // confirmation either way rather than inviting a retry loop that the
        // rate limiter will refuse.
      }
      state = 'done';
    })();
  });
</script>

<div class="wrap">
  <Card title="Unsubscribe">
    {#if state === 'missing'}
      <p>This link is missing its token. Open it directly from the notification email.</p>
    {:else if state === 'working'}
      <Spinner />
    {:else}
      <p>That subscription is now off. You will not receive those notifications again.</p>
      <p class="hint">You can turn it back on at any time from your account page.</p>
      <Button href="#/account" variant="primary">Manage subscriptions</Button>
    {/if}
  </Card>
</div>

<style>
  .wrap { max-width: 520px; margin: 64px auto; padding: 0 16px; }
  .hint { font-size: 13px; color: var(--text-faint); }
</style>
```

- [ ] **Step 2: Register the route.** In `dashboard/src/routes.ts`, add the import beside the other page imports:

```ts
import Unsubscribe from './pages/Unsubscribe.svelte';
```

and the route entry beside `/change-password`:

```ts
  // `conditions: []` — not `guarded()`, and deliberately NOT in App.svelte's
  // PUBLIC_ROUTES either. That array drives an $effect that pushes
  // authenticated users OFF those paths, which is exactly wrong here: a
  // logged-in user clicking an unsubscribe link must still see the
  // confirmation.
  '/unsubscribe': wrap({ component: Unsubscribe as never, conditions: [] }),
```

- [ ] **Step 3: Confirm `PUBLIC_ROUTES` is untouched.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && grep -n "PUBLIC_ROUTES" -A 6 src/App.svelte`
  Expected: `/unsubscribe` does **not** appear in the array.

- [ ] **Step 4: Confirm there is no Sidebar entry.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && grep -n "unsubscribe" src/lib/components/layout/Sidebar.svelte`
  Expected: no output. This is a link-target page, not a navigable one.

- [ ] **Step 5: Typecheck and test.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check && npm run test`
  Expected: 0 type errors; all tests green.

---

## Task 26: Documentation, the upgrade runbook, and final verification

**Files:**
- Create `wiki/Notifications.md`
- Modify `wiki/_Sidebar.md` (the **Guides** block, lines 18-23)
- Modify `wiki/Home.md` (the Pages index)
- Modify `packaging/rpm/SETUP.md` (§11 "Upgrading", its per-migration table)

**Interfaces:**
- Consumes: nothing.
- Produces: the user-facing guide and the operator-facing upgrade row.

- [ ] **Step 1: Write the wiki page.** Create `wiki/Notifications.md`:

```markdown
# Notifications

Sauron sends two kinds of email. **Alerts** are configured by an organization
admin and go to org-wide channels. **Personal notifications** — this page — are
configured by you, go to your own address, and nobody else can see or change
them.

## What you can subscribe to

| Kind | Scope | Environment filter | Fires when |
|---|---|---|---|
| Uptime | A project | Not applicable | A monitor in that project goes down or recovers |
| Error rate increasing | A project or one app | Yes | Errors in the last window are at least `min_count` **and** either the previous window was empty or the count is at least `factor` times it |
| New issue | A project or one app | Yes | An issue is seen for the first time |
| Issue regressed | A project or one app | Yes | A resolved or ignored issue starts erroring again |

Uptime has no environment filter and no app scope because a monitor belongs to a
whole project — it has no app or environment of its own.

### "Error rate increasing" in detail

With a window `W`:

- `C` = error count over the last `W`
- `B` = error count over the `W` before that
- It fires when `C >= min_count` **and** (`B = 0` **or** `C >= B × factor`).

The `B = 0` case matters: an app that was silent and is now on fire is exactly
the situation you want to hear about. The `min_count` floor matters too — without
it, 1 error becoming 3 is a 3× spike and would wake you up.

Defaults: window 15 minutes, factor 3×, minimum 10 errors. Ranges: window
300–86400 seconds, factor 1.5–100, minimum 1–100000.

## Environments

The environment chips list your **project's** environments — `prod`, `staging`,
and so on. Ticking `prod` means "prod, in every app in scope", and it keeps
meaning that when a new app is added to the project later.

Leaving every chip unticked means all environments, **including** events that
arrived with no environment attached.

## Noise control

| Control | What it does |
|---|---|
| **Throttle** (default 15 minutes) | The same notification is not repeated inside this window |
| **Delivery** | *As it happens*, *hourly summary*, or *daily summary* |
| **Quiet hours** | Notifications raised inside the window are **held**, not dropped, and arrive when it ends |
| Per-hour cap | Above 20 emails an hour, the rest are merged into one digest — never discarded. The card shows the delivery you are actually getting |
| Maximum subscriptions | 50 per person |

Quiet hours defer rather than drop on purpose: a night-time outage is still an
outage, and "quiet" must never be indistinguishable from "broken".

## Unsubscribing

Every notification email carries an unsubscribe link. Opening it turns off that
one subscription and sends you a short confirmation email saying so — that
confirmation is deliberate, so a silencing is never invisible to you. Links stay
valid for 90 days and a fresh one is minted with every message.

## Why a subscription can turn itself off

A subscription only ever delivers telemetry you are already allowed to read. If
your access to its project or app is removed, the subscription disables itself
and the card says "Off — access removed". Getting access back does not silently
turn it on again: you turn it back on yourself.

## What this does not do

- It does not deliver to Slack, Discord, a webhook or Telegram. Those are
  org-level alert channels an admin configures.
- It does not narrow uptime below a project.
- It does not notify on analytics event volume or latency percentiles. Those are
  team dashboards, not personal inboxes.
```

- [ ] **Step 2: Register the page in both indexes.** In `wiki/_Sidebar.md`, the **Guides** block currently reads:

```markdown
**Guides**

- [Framework Integrations](Framework-Integrations.md)
- [Best Practices](Best-Practices.md)
- [Troubleshooting](Troubleshooting.md)
```

  Insert `Notifications` between Best Practices and Troubleshooting (Troubleshooting stays last in that block):

```markdown
- [Notifications](Notifications.md)
```

  In `wiki/Home.md`, the `### Guides` block under `## Pages` uses bold-link-plus-em-dash entries. Insert this one after the **Best Practices** entry and before **Troubleshooting**, matching that style exactly:

```markdown
- **[Notifications](Notifications.md)** — personal email notifications you subscribe
  yourself to: uptime, error spikes, new issues and regressions, with environment
  filters, throttling, digests, quiet hours and one-click unsubscribe.
```

- [ ] **Step 3: Append the migration row to the upgrade runbook.** In `packaging/rpm/SETUP.md` §11 "Upgrading", add one row to the per-migration table:

```markdown
| 000037 | `notification_subscriptions` | `sauron-alerts` fails its subscription pass every tick. Tick failures are logged-and-swallowed by design, so it does this **quietly, forever**: no personal notification is ever evaluated, enqueued or delivered, and nothing in the dashboard indicates a problem. `POST /v1/me/notification-subscriptions` also 500s. |
```

- [ ] **Step 4: Write the release note about the environment-filter fix.** The repository has **no** changelog or release-notes file — `ls` of the repo root shows only `README.md`, `Makefile`, `LICENSE`, `plan.md` and the source directories, and `ls CHANGELOG* RELEASE* NEWS*` returns nothing. So the operator-facing home for this is `packaging/rpm/SETUP.md` §11 "Upgrading" (the section S0 created), immediately **below** the per-migration table you just appended to, under this heading:

```markdown
### One-off behaviour change in this release
```

  followed by:

```markdown
**Environment-filtered alert rules start firing again.** Since migration 33,
`alert_count_errors` and `alert_count_events` resolved an environment *name*
against the project-level `environments` catalogue, whose ids can never equal
the `app_environments` enrollment id an event row carries — so every
environment-narrowed alert rule had been counting zero and had never fired. This
release fixes the resolution, and those rules will fire for the first time on
the first tick after deploy. Each rule's own `throttle_seconds` bounds it to one
message per throttle period, but an operator with many environment-filtered
rules should expect a burst and may want to disable them for one tick. Rules
naming a *misspelled* environment resolve to an empty set and keep counting
zero, exactly as before — now deliberately.
```

- [ ] **Step 5: Run the full backend gate.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
  Expected: fmt clean, no clippy warnings, all tests pass (the DB tests print their skip notice without `TEST_DATABASE_URL`).

- [ ] **Step 6: Run the full backend gate WITH a database.**
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test --workspace`
  Expected: every test in `backend/crates/sauron-db/tests/notifications.rs` runs and passes, plus the pre-existing `env_scoping.rs` and `workflows.rs` suites.

- [ ] **Step 7: Run the full dashboard gate.**
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check && npm run test`
  Expected: 0 type errors, all vitest suites green.

- [ ] **Step 8: Confirm the config-documentation gate is satisfied.**
  `cd /home/splimter/projects/freelance/sauron && for k in NOTIFY_SUBS_TICK_SECS NOTIFY_SUBS_BATCH NOTIFY_SUBS_MAX_PROBES_PER_ORG NOTIFY_DRAIN_BUDGET_MS NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR NOTIFY_QUEUE_RETENTION_DAYS; do for f in .env.example README.md docker-compose.yml packaging/rpm/config/alerts.env; do grep -q "$k" "$f" || echo "MISSING $k in $f"; done; done`
  Expected: no output.

- [ ] **Step 9: Confirm the packaging surface is untouched.**
  `cd /home/splimter/projects/freelance/sauron && git diff --name-only -- packaging/rpm/binaries.txt packaging/rpm/sauron.spec packaging/rpm/build-rpm.sh packaging/rpm/systemd/`
  Expected: no output. S3 ships no new binary, so `binaries.txt`, the `%install` loop, `%files`, and the `%post`/`%preun`/`%postun` unit lists are all unchanged. `sauron-alerts.service` already loads `/etc/sauron/secret.env` (needed for the unsubscribe HMAC material) and already permits outbound AF_INET.

- [ ] **Step 10: Manual end-to-end walkthrough.** There is no mail sink in the automated harness and that is the accepted gap, so drive this by hand once:

  1. Apply the migration: `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`
  2. Start `sauron-api`, `sauron-alerts` and the dashboard with `SMTP_SINK=1 SAURON_DEV=1 DASHBOARD_URL=http://localhost:10002`.
  3. On `#/account`, create an `error_spike` subscription on a dev project with `min_count = 1` and `throttle_seconds = 0`.
  4. Send a burst of errors to that app's dev DSN.
  5. Within `NOTIFY_SUBS_TICK_SECS`, confirm a `notification_queue` row appears with the right `app_id`, and `notification_queue_envs` rows if you set an environment filter:
     `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "SELECT id, app_id, kind, status, deliver_after FROM notification_queue ORDER BY created_at DESC LIMIT 5;"`
  6. On the next tick, confirm the row flips to `sent` with a `message_id`, and that a `mail_outbox` row exists for your address.
  7. Copy the unsubscribe URL out of the `SMTP_SINK` log line, open it in the browser, and confirm the subscription flips to `enabled = false` / `disabled_reason = 'unsubscribed'` and a confirmation `mail_outbox` row appears.
  8. Remove your grant on that project and re-run the drain: confirm any remaining queued row lands in `dropped_no_access` with `title`, `body` and `link` all NULL.

---

## Deliberately NOT in this slice

Do not build these; each is recorded as an accepted gap or a follow-up in the design.

- A leader lock or any advisory lock. Evaluation double-runs on two `sauron-alerts` replicas today and this slice does not fix it — the Redis claim plus the partial unique dedup index make duplicate *enqueue* very unlikely, and delivery is genuinely exclusive via the `claimed` status, so the worst case is duplicate mail rather than duplicate work.
- A `List-Unsubscribe` / RFC 8058 header. S0's send signature does not accept custom headers.
- Personal delivery to Slack, Discord, webhook or Telegram. The queue is channel-agnostic by construction, but S3 ships email only.
- `scope_type='org'` subscriptions. One tick would fan out to every app in the org.
- An environment parameter for `alert_latency_metric`, and `monitors.app_id`. Both are real features and both belong with the monitors-app-id work.
- Fixing `alert_rules_for_monitor`'s own scoping bug (an app-narrowed rule fires for every monitor in its project). Noted, not fixed.
- Admin visibility into other users' subscriptions, org-level defaults, and mandatory subscriptions. Subscriptions are strictly personal.
- Migrating the org alerting engine onto `notification_queue`. Only the environment resolver is shared.

