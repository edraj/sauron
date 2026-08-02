# Session Management (S2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every login a durable session identity that survives refresh-token rotation, let a user list and end their own sessions, let an admin sign a member out of everything, and cut the residual access-token window from 900 seconds to about 5.

**Architecture:** A new `auth_sessions` table holds the stable identity, `refresh_tokens.session_id` points at it, and the access token carries the same id as a new `sid` claim. Every revoke path writes both tables in one data-modifying CTE and hands the revoked ids to a per-replica in-process `HashSet` (`SessionRevocations`) that a background Postgres poll refreshes every few seconds; the `AuthUser` extractor does a pure in-memory lookup on `sid` and 401s a revoked session. The dashboard gains an `#/account` page listing sessions, and the Members table gains an admin "Sign out" action gated on a new `member:credential` permission.

**Tech Stack:** Rust 1.82 (MSRV), axum 0.8, diesel + diesel-async + Postgres, `jsonwebtoken`, `woothee` 0.13 (User-Agent parsing), `uuid`, `chrono`, `tracing`; Svelte 5 runes + vitest on the dashboard.

## Global Constraints

- **NEVER run `git commit`, `git add`, or create a branch.** The repository owner commits manually. There is no commit step anywhere in this plan.
- Never use `conn.transaction(...)` — the workspace MSRV is 1.82 and diesel-async 0.9's signature needs async closures (1.85+). Multi-statement atomicity is **one data-modifying CTE** via `diesel::sql_query` + `.bind()`.
- `backend/crates/sauron-db/src/schema.rs` is **hand-maintained**. The diesel CLI must NEVER run. A new table means three hand edits: a `diesel::table!` block, a `diesel::joinable!` line if it has an FK, and the name inside `allow_tables_to_appear_in_same_query!`.
- Migrations live at `backend/migrations/YYYY-MM-DD-0000NN_slug/{up,down}.sql`. **Both files are required.** `up.sql` opens with a prose comment explaining WHY. A migration runs in ONE transaction, so `CONCURRENTLY` is unavailable and an index build on a partitioned parent locks every child.
- Enum-like columns are `TEXT` + `CHECK`, never a custom SQL type.
- All SQL lives in `backend/crates/sauron-db/src/repo.rs` as free `pub async fn name(conn: &mut AsyncPgConnection, ...) -> QueryResult<T>`. Handlers never build queries inline.
- Insertable-only structs must NOT gain a `Queryable` derive — `Queryable` decodes positionally and would silently bind fields to the wrong columns.
- Never hold a pooled `PgConn` across network I/O. The API pool is 16 connections for the whole process. `drop(conn)` first.
- Dashboard: house UI components only. There is **no** Select, Toggle, Tabs or Menu primitive in `dashboard/src/lib/components/ui/`. A new page needs three edits: the page file, `src/routes.ts`, and the `groups` array in `src/lib/components/layout/Sidebar.svelte`. Pure decision logic goes in `src/lib/models/*.ts` with a colocated `*.test.ts` — there is **no DOM test environment**.
- Svelte 5 runes. `$state` deep-proxies values so `===` never matches a raw value; use `$state.raw` when identity matters. `Set`s and `Record`s in `$state` are **replaced**, never mutated in place.
- Comments explain the failure mode that motivated the code, not what the code does. Match that register.
- `cargo clippy --all-targets` runs with `-D warnings` and `cargo fmt --all --check` is a hard gate.
- Migration number for this slice is **000035**, pinned by the programme (`docs/superpowers/specs/2026-08-01-notifications-security-analytics-programme-design.md` §5). The date prefix is the **landing** date. Re-check `ls backend/migrations | tail -1` before writing the SQL; a number gap (because S0's 000034 has not landed yet) is harmless — diesel runs whatever is pending, ordered by the full `YYYY-MM-DD-0000NN` string.
- Kill latency in user-facing copy is **"within a few seconds"**, never "immediately".

### Command cheat sheet (use verbatim)

- Rust check: `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`
- Rust unit test: `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p <crate> <testname>`
- Rust clippy: `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
- Rust fmt gate: `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check`
- Apply migrations: `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`
- DB-backed tests (skip silently when unset): prefix with `TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379`
- Dashboard tests: `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
- Dashboard typecheck: `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`

## File Structure

**Created**

| Path | Responsibility |
|---|---|
| `backend/migrations/2026-08-01-000035_auth_sessions/up.sql` | Creates `auth_sessions`, its two partial indexes, `refresh_tokens.session_id` + its partial index, backfills live sessions, seeds `member:credential` into custom roles holding `member:manage` |
| `backend/migrations/2026-08-01-000035_auth_sessions/down.sql` | Exact inverse, referencing column dropped before referenced table |
| `backend/crates/sauron-auth/src/revocations.rs` | `SessionRevocations`: the per-replica in-memory revoked-session snapshot, its poll, and the eviction rule |
| `backend/bins/sauron-api/src/tasks.rs` | `spawn_named` — the supervised background-loop runner. Boot never fails on a task |
| `backend/bins/sauron-api/src/routes/account.rs` | `/v1/me/sessions` list + revoke + revoke-others, `SessionView`, `parse_ua` |
| `backend/crates/sauron-db/tests/sessions.rs` | Postgres integration tests for every new repo fn |
| `backend/bins/sauron-api/tests/http_sessions.rs` | End-to-end tests over the real spawned binary |
| `dashboard/src/lib/api/account.ts` | Typed client for the four new endpoints |
| `dashboard/src/lib/models/account-sessions.ts` | Pure display/decision logic: `describeSession`, `sortSessions`, `otherSessionCount`, `hasCurrentSession`, `allSameIp` |
| `dashboard/src/lib/models/account-sessions.test.ts` | vitest cover for the above |
| `dashboard/src/pages/Account.svelte` | `#/account` — Profile card + Active sessions card |

**Modified**

| Path | Change |
|---|---|
| `backend/crates/sauron-db/src/schema.rs` | `auth_sessions` `table!` block, `session_id` on `refresh_tokens`, one `joinable!`, one `allow_tables_to_appear_in_same_query!` entry |
| `backend/crates/sauron-db/src/models.rs` | `AuthSession` model; `session_id` on `RefreshToken`; delete `NewRefreshToken` |
| `backend/crates/sauron-db/src/repo.rs` | Three new reason constants + `DELIBERATE_REVOKE_REASONS`; `start_or_continue_session`; four revoke/list fns; `revoked_session_ids`; `prune_auth_sessions`; widened `refresh_token_revocation`; delete `insert_refresh_token` |
| `backend/crates/sauron-auth/src/jwt.rs` | `Claims.sid`, `issue_access` signature |
| `backend/crates/sauron-auth/src/extractors.rs` | `SessionRevocations: FromRef<S>` bound + the snapshot check |
| `backend/crates/sauron-auth/src/lib.rs` | `pub mod revocations;` + re-export |
| `backend/crates/sauron-auth/src/rbac.rs` | `perm::MEMBER_CREDENTIAL`, `perm::ALL`, `ADMIN` bag, four preset assertions |
| `backend/crates/sauron-core/src/config.rs` | `auth_revocation_poll_secs` |
| `backend/bins/sauron-api/Cargo.toml` | `woothee.workspace = true`; `features = ["json"]` on the dev-dependency `reqwest` (the workspace default omits it) |
| `backend/bins/sauron-api/src/main.rs` | `mod tasks;`, `AppState.revocations`, `FromRef` impl, two spawned tasks, four routes |
| `backend/bins/sauron-api/src/routes/mod.rs` | `pub mod account;`, `SessionContext`, rewritten `issue_tokens`, `MAX_USER_AGENT_LEN`, `sanitize_ua`, `sanitize_ip`, the call-site pin test |
| `backend/bins/sauron-api/src/routes/auth.rs` | `pub(crate)` on `rate_limit`/`client_addr`, five `issue_tokens` call sites, session-aware `logout`, session-aware `change_password`, the deliberate-revocation branch in `refresh` |
| `backend/bins/sauron-api/src/routes/orgs.rs` | `guard_member_admin_action`, `set_member_active` refactor + conversion, `revoke_member_sessions` |
| `backend/bins/sauron-api/tests/http_workflows.rs` | Two `issue_access` call sites |
| `backend/bins/sauron-api/tests/http_env_scoping.rs` | Seven `issue_access` call sites |
| `dashboard/src/lib/models/index.ts` | `AccountSession`, `'member:credential'` in `Permission` |
| `dashboard/src/lib/models/permissions.ts` | `ALL_PERMISSIONS`, `PERMISSION_GROUPS`, `PERMISSION_LABELS` |
| `dashboard/src/routes.ts` | `'/account'` |
| `dashboard/src/lib/components/layout/Sidebar.svelte` | Account nav item in the Manage group |
| `dashboard/src/lib/components/members/MembersTable.svelte` | `onrevokesessions` / `revokingUserId` / `canRevokeSessions` props + the Sign out button |
| `dashboard/src/pages/Members.svelte` | Confirm dialog + handler for the above |
| `dashboard/src/pages/Docs.svelte` | Account page + corrected kill-latency wording |
| `.env.example`, `docker-compose.yml`, `packaging/rpm/config/api.env`, `README.md` | `AUTH_REVOCATION_POLL_SECS` |
| `packaging/rpm/SETUP.md`, `packaging/rpm/sauron.spec` | Upgrade gate for 000035 |

---

### Task 1: Migration 000035 — `auth_sessions`, `refresh_tokens.session_id`, backfill, permission seed

**Files:**
- Create `backend/migrations/2026-08-01-000035_auth_sessions/up.sql`
- Create `backend/migrations/2026-08-01-000035_auth_sessions/down.sql`

**Interfaces:**
- Consumes: nothing.
- Produces: table `auth_sessions` (columns in this exact order: `id`, `user_id`, `created_at`, `last_used_at`, `expires_at`, `user_agent`, `ip`, `revoked_at`, `revoked_reason`, `revoked_by`); column `refresh_tokens.session_id UUID NULL`; constraint name `auth_sessions_revoked_reason_check`; indexes `auth_sessions_user_live_idx`, `auth_sessions_revoked_idx`, `refresh_tokens_session_idx`; the permission string `member:credential` present on every custom role holding `member:manage`.

- [ ] **Step 0: Record the pre-migration baseline and seed the two probe roles.** The permission seed is the one statement in this migration whose result cannot be reconstructed afterwards — once it has run, "how many roles held `member:manage` before?" is unanswerable, because every one of them now holds `member:credential` too. Capture it first, and seed the negative case the assertion needs. Run:

```
psql postgres://sauron:sauron@172.20.0.2:5432/sauron \
  -c "INSERT INTO roles (org_id, name, description, permissions) VALUES
        ((SELECT id FROM organizations LIMIT 1), 'mig035-probe-with-manage',
         'migration 000035 probe: a custom role that DOES hold member:manage',
         '[\"member:read\",\"member:manage\"]'::jsonb),
        ((SELECT id FROM organizations LIMIT 1), 'mig035-probe-without-manage',
         'migration 000035 probe: a custom role that does NOT hold member:manage',
         '[\"member:read\",\"issue:read\"]'::jsonb);" \
  -c "SELECT count(*) AS roles_with_member_manage FROM roles WHERE permissions @> '[\"member:manage\"]'::jsonb;"
```

  `INSERT 0 2`, then a count. **Write that count down** — Step 5 compares against it. The two probe roles are org-scoped to whatever organization the dev database already has (`org_id` is nullable, so the insert still succeeds on an empty deployment). They exist because the migration's predicate is `member:manage` *holders*, not preset names: without a custom role on each side of that predicate, Step 5 would only ever prove that Owner and Admin — which `ensure_preset_roles` re-syncs from `rbac.rs` at every api boot anyway — came out right.

- [ ] **Step 1: Confirm the migration number is still free.** Run `ls /home/splimter/projects/freelance/sauron/backend/migrations | tail -3`. Expect the last entry to be `2026-07-30-000033_env_per_project` (or `..._000034_mail_outbox` if S0 landed first). If a `000035` directory already exists, stop and re-read the programme allocation table before continuing. Create the directory: `mkdir -p /home/splimter/projects/freelance/sauron/backend/migrations/2026-08-01-000035_auth_sessions`.

- [ ] **Step 2: Write `up.sql`.** Create `backend/migrations/2026-08-01-000035_auth_sessions/up.sql` with exactly this content:

```sql
-- Sessions get an identity of their own.
--
-- Today a "session" has no durable name. `refresh_tokens` rows are replaced wholesale on every
-- rotation -- new id, new token_hash, new created_at -- so after fifteen minutes there is nothing
-- left to point at. That makes three things impossible: showing a user where they are logged in,
-- ending one session without ending all of them, and recording who ended it. `auth_sessions.id`
-- is that missing identity, and it is what goes into the access token's new `sid` claim.
--
-- MAINTENANCE WINDOW. This migration is not a background change. `run_pending_migrations` runs the
-- whole file in ONE transaction, so `CONCURRENTLY` is unavailable (same constraint spelled out in
-- 2026-07-28-000028_issue_env_covering_index). `ALTER TABLE refresh_tokens ADD COLUMN` takes
-- AccessExclusiveLock and holds it to COMMIT, and `refresh_tokens` is written by every login,
-- refresh, logout and password change. The costs, largest first:
--   1. `refresh_tokens_session_idx` -- a full heap scan. The table has exactly one index today
--      (refresh_tokens_user_idx) and nothing has ever reaped it; a deployment live for a year with
--      50 active sessions holds roughly 1.7M rows. Making the index partial does not avoid the
--      scan, it bounds the resulting index to live sessions.
--   2. The backfill UPDATE -- same scan, negligible write volume: every rotated row is already
--      revoked, so `revoked_at IS NULL AND expires_at > now()` matches about one row per active
--      session.
--   3. The ALTER itself -- metadata-only for a nullable column with no default.
-- Do NOT try to bound the backfill with `created_at > now() - interval '30 days'`. It is redundant
-- under the default JWT_REFRESH_TTL_SECS (2592000 = 30 days, and expires_at = created_at + ttl) and
-- silently lossy if an operator raised that TTL: live tokens outside the window would keep
-- session_id IS NULL and their owners' current sessions would be unmanageable, with no error
-- anywhere. `expires_at > now()` is the correct liveness predicate and it is already minimal.
--
-- Column notes worth keeping:
--   * last_used_at is stamped on ROTATION, not per request, so "last used" is accurate only to
--     within JWT_ACCESS_TTL_SECS. A session used 30 seconds ago can display as "15 minutes ago".
--     Do not "fix" this by writing on every request -- that turns a read-only auth path into a
--     write on every API call.
--   * expires_at mirrors the newest refresh token's expiry (sliding, matching today's behaviour)
--     so liveness needs no join.
--   * revoked_by is ON DELETE SET NULL, not CASCADE: deleting the admin must not delete the
--     victim's audit row.
--   * The CHECK deliberately EXCLUDES 'rotated'. A rotation revokes a token, never a session, so
--     writing 'rotated' here is a bug the database catches. Note that refresh_tokens.revoked_reason
--     has no CHECK, so the two columns share a vocabulary and only one enforces it. THIS CHECK IS A
--     DEPLOY COUPLING: adding a reason in code without a widening migration produces a 500 on the
--     revoke path.
--   * 'password_reset' and 'reset_forced' are listed from day one even though nothing in this slice
--     writes them. They belong to the password-reset slice that lands next and revokes sessions on
--     both of its reset paths, one of them unauthenticated. Arriving with that slice instead, every
--     successful reset would 500 at the revoke step until a second migration caught up -- landing
--     on a user who has just proved they cannot get into their account. Widening the list costs
--     nothing while the table is created empty in this same transaction.
--
-- refresh_tokens.session_id is ON DELETE SET NULL, NOT CASCADE. CASCADE would pre-authorise a real
-- failure: deleting one auth_sessions row would take that session's whole token history with it,
-- and `refresh_token_revocation` -- which reads revoked rows regardless of state and is the entire
-- replay signal -- would then find nothing and treat a replayed token as "never existed": a plain
-- 401, no family kill, no WARN. The 30-day reaper deletes auth_sessions rows by design, so this is
-- not hypothetical.
--
-- auth_sessions_user_live_idx is (user_id) WHERE revoked_at IS NULL and deliberately does NOT
-- include last_used_at. Indexing it would make every rotation a non-HOT update -- the
-- ON CONFLICT DO UPDATE would rewrite the heap tuple AND both index entries, leaving two dead
-- versions for autovacuum on the hottest-updated column in the table. With only id and user_id
-- indexed and neither changing on rotation, the update is HOT-eligible and the page self-vacuums.
-- The ordering the index would have provided buys nothing: the query is scoped to one user_id,
-- capped at 200 rows, and a real account has single-digit live sessions.
--
-- TODO: `refresh_tokens` is unbounded and unreaped -- roughly 96 rows/day per active session. This
-- migration makes it materially worse (a second index means more write amplification and more disk
-- on the fastest-growing never-pruned table). A reaper must delete on EXPIRY only, never merely
-- because a row is revoked: revoked rows are the whole replay signal.

CREATE TABLE auth_sessions (
  id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_used_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at     TIMESTAMPTZ NOT NULL,
  user_agent     TEXT,
  ip             TEXT,
  revoked_at     TIMESTAMPTZ,
  revoked_reason TEXT,
  revoked_by     UUID REFERENCES users(id) ON DELETE SET NULL,
  CONSTRAINT auth_sessions_revoked_reason_check CHECK (
    revoked_reason IS NULL OR revoked_reason IN (
      'logout','user_revoked','user_revoked_others','admin_revoked',
      'password_changed','deactivated','reuse',
      'password_reset','reset_forced')
  )
);

CREATE INDEX auth_sessions_user_live_idx
  ON auth_sessions (user_id) WHERE revoked_at IS NULL;

CREATE INDEX auth_sessions_revoked_idx
  ON auth_sessions (revoked_at) WHERE revoked_at IS NOT NULL;

ALTER TABLE refresh_tokens
  ADD COLUMN session_id UUID REFERENCES auth_sessions(id) ON DELETE SET NULL;

INSERT INTO auth_sessions (id, user_id, created_at, last_used_at, expires_at, user_agent)
  SELECT r.id, r.user_id, r.created_at, r.created_at, r.expires_at, r.user_agent
    FROM refresh_tokens r
   WHERE r.revoked_at IS NULL AND r.expires_at > now();

UPDATE refresh_tokens r SET session_id = r.id
 WHERE r.revoked_at IS NULL AND r.expires_at > now();

CREATE INDEX refresh_tokens_session_idx
  ON refresh_tokens (session_id) WHERE session_id IS NOT NULL;

-- Seed the new `member:credential` permission. The predicate matches member:manage HOLDERS, not
-- the preset names: member:credential is carved OUT of member:manage rather than added beside it,
-- so every role that holds member:manage today can already sign a member out via
-- deactivate-then-reactivate. Matching on preset names would silently strip that from every custom
-- role an operator has built while leaving Owner and Admin whole. `ensure_preset_roles` re-syncs
-- Owner and Admin from rbac.rs at api startup regardless, so the presets are covered twice and the
-- custom roles only here.
UPDATE roles SET permissions = permissions || '["member:credential"]'::jsonb
 WHERE permissions @> '["member:manage"]'::jsonb
   AND NOT (permissions @> '["member:credential"]'::jsonb);
```

- [ ] **Step 3: Write `down.sql`.** Create `backend/migrations/2026-08-01-000035_auth_sessions/down.sql` with exactly this content:

```sql
-- Order is load-bearing: the referencing column must go before the referenced table.
-- This is a real inverse; it loses session history, which is acceptable because the pre-migration
-- system had none.
--
-- The permission is stripped unconditionally rather than only from member:manage holders, because
-- a role edited between the up and the down could hold one without the other.
UPDATE roles SET permissions = permissions - 'member:credential';
DROP INDEX IF EXISTS refresh_tokens_session_idx;
ALTER TABLE refresh_tokens DROP COLUMN IF EXISTS session_id;  -- drops the FK with it
DROP INDEX IF EXISTS auth_sessions_revoked_idx;
DROP INDEX IF EXISTS auth_sessions_user_live_idx;
DROP TABLE IF EXISTS auth_sessions;
```

- [ ] **Step 4: Apply the migration and see it succeed.** Run `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`. Expected: it prints the applied migration and exits 0. Time the run and note the number — it goes into the upgrade note written by Task 20 Step 5, which will not pass its own check until you have it.

- [ ] **Step 5: Verify the shape and the backfill against the live database.** Run:

```
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "\d auth_sessions" \
  -c "SELECT count(*) AS live_tokens_with_session FROM refresh_tokens WHERE session_id IS NOT NULL;" \
  -c "SELECT count(*) AS orphan_live FROM refresh_tokens WHERE revoked_at IS NULL AND expires_at > now() AND session_id IS NULL;" \
  -c "SELECT count(*) AS revoked_with_session FROM refresh_tokens WHERE revoked_at IS NOT NULL AND session_id IS NOT NULL;" \
  -c "SELECT count(*) AS roles_with_credential FROM roles WHERE permissions @> '[\"member:credential\"]'::jsonb;" \
  -c "SELECT name, permissions @> '[\"member:credential\"]'::jsonb AS gained FROM roles WHERE name LIKE 'mig035-probe-%' ORDER BY name;"
```

Expected: `\d auth_sessions` lists the ten columns in declared order plus the three constraints; `orphan_live` is `0`; `revoked_with_session` is `0`; `roles_with_credential` equals the `roles_with_member_manage` count you wrote down in Step 0; and the last query returns exactly two rows — `mig035-probe-with-manage | t` and `mig035-probe-without-manage | f`. That second row is the assertion that matters most: it is the only evidence that the predicate is `member:manage` holders and not "every role", and a migration that hands `member:credential` to every role in the deployment would pass every other check on this list.

- [ ] **Step 6: Prove `down.sql` is a real inverse.** Run `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -f /home/splimter/projects/freelance/sauron/backend/migrations/2026-08-01-000035_auth_sessions/down.sql` and confirm it exits 0 with no error. Then re-apply with `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`. Diesel records the version in `__diesel_schema_migrations`, which `down.sql` does not touch, so delete the row first if the re-run reports nothing pending: `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "DELETE FROM __diesel_schema_migrations WHERE version = '20260801000035';"` and run the migrate binary again. Expected end state: `\d auth_sessions` succeeds again and the counts from Step 5 hold — including the two probe rows, because `down.sql` strips `member:credential` unconditionally and the re-applied `up.sql` re-derives it from `member:manage`. That round trip is what proves the two statements are genuine inverses; if `mig035-probe-without-manage` comes back with `gained = t`, the down stripped nothing and the up matched too widely.

Then remove the probes: `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "DELETE FROM roles WHERE name LIKE 'mig035-probe-%';"`. Expected `DELETE 2`. They hold no grants, so nothing references them.

---

### Task 2: `schema.rs` and `models.rs` — hand-maintained mappings for the new table

**Files:**
- Modify `backend/crates/sauron-db/src/schema.rs` (insert a `table!` block at the top after the `// @generated` line; edit the `refresh_tokens` block at ~line 213; add a `joinable!` near line 486; add one name to `allow_tables_to_appear_in_same_query!` at ~line 503)
- Modify `backend/crates/sauron-db/src/models.rs` (add `session_id` to `RefreshToken` at ~line 503; add `AuthSession` after it)

**Interfaces:**
- Consumes: the columns produced by Task 1.
- Produces: `sauron_db::schema::auth_sessions`, `sauron_db::schema::refresh_tokens::session_id`, `sauron_db::models::AuthSession` with fields `id: Uuid`, `user_id: Uuid`, `created_at: DateTime<Utc>`, `last_used_at: DateTime<Utc>`, `expires_at: DateTime<Utc>`, `user_agent: Option<String>`, `ip: Option<String>`, `revoked_at: Option<DateTime<Utc>>`, `revoked_reason: Option<String>`, `revoked_by: Option<Uuid>`; and `RefreshToken.session_id: Option<Uuid>`.

- [ ] **Step 1: Add the `auth_sessions` `table!` block.** In `backend/crates/sauron-db/src/schema.rs`, immediately after the line `// @generated automatically by Diesel CLI.` and its blank line — i.e. **before** `diesel::table! { analytics_events (id) {` — insert:

```rust
diesel::table! {
    auth_sessions (id) {
        id -> Uuid,
        user_id -> Uuid,
        created_at -> Timestamptz,
        last_used_at -> Timestamptz,
        expires_at -> Timestamptz,
        user_agent -> Nullable<Text>,
        ip -> Nullable<Text>,
        revoked_at -> Nullable<Timestamptz>,
        revoked_reason -> Nullable<Text>,
        revoked_by -> Nullable<Uuid>,
    }
}

```

- [ ] **Step 2: Append `session_id` to the `refresh_tokens` block.** In the same file, inside `diesel::table! { refresh_tokens (id) { ... } }`, add `session_id -> Nullable<Uuid>,` as the **last** field, directly after `revoked_reason -> Nullable<Text>,`. Column order here must match the order the migration produced, because `Queryable` decodes positionally.

- [ ] **Step 3: Add the joinable and the same-query entry.** Directly above the existing line `diesel::joinable!(refresh_tokens -> users (user_id));` insert:

```rust
// Deliberately the only association declared for this table. diesel allows one association per
// table pair, no query in this slice joins auth_sessions to refresh_tokens in the DSL (all
// multi-table work is raw CTEs), and `revoked_by` would need a second users association diesel
// cannot express -- an unused joinable is a future ambiguous-join trap.
diesel::joinable!(auth_sessions -> users (user_id));
```

Then, inside `diesel::allow_tables_to_appear_in_same_query!(`, add `auth_sessions,` as the **second** entry, directly after `analytics_events,`.

- [ ] **Step 4: Check the schema edits alone, and expect a CLEAN build.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Expected: clean — **not** a compile error. Adding a column to a `diesel::table!` block does not invalidate a `Selectable` struct, and the only read of `RefreshToken` is `find_active_refresh_token`, which goes through `.select(RefreshToken::as_select())` (repo.rs:197) and therefore names its columns explicitly. A green check here means Steps 1-3 landed, not that they were skipped. There is no red-then-green signal available at this layer; Task 6's Postgres tests are what actually exercise the new column.

- [ ] **Step 5: Add `session_id` to `RefreshToken` and the new `AuthSession` model.** In `backend/crates/sauron-db/src/models.rs`, add the last field to `RefreshToken`:

```rust
    /// Why this token was revoked — see [`crate::repo::REVOKE_ROTATED`].
    pub revoked_reason: Option<String>,
    /// The `auth_sessions` row this token belongs to. Nullable because rows
    /// minted before migration 000035, and rows whose session the 30-day reaper
    /// has deleted, both have none — and the FK is ON DELETE SET NULL precisely
    /// so a reap cannot take the replay-detection history with it.
    pub session_id: Option<Uuid>,
}
```

Then, directly after the closing brace of `RefreshToken` and **before** `#[derive(Debug, Insertable)] pub struct NewRefreshToken`, insert:

```rust
/// A login that survives refresh-token rotation.
///
/// No `Serialize`, on purpose — the same discipline `RefreshToken` follows. The
/// API returns a hand-built `SessionView`; letting the model reach the wire is
/// how `revoked_by` (which admin ended your session) leaks to the member it was
/// used against.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = auth_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AuthSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub created_at: DateTime<Utc>,
    /// Stamped on rotation, not per request — so this is accurate only to within
    /// `JWT_ACCESS_TTL_SECS`. Writing it on every request would turn a read-only
    /// auth path into a write on every API call.
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_reason: Option<String>,
    pub revoked_by: Option<Uuid>,
}
```

- [ ] **Step 6: See it compile.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Expected: clean.

- [ ] **Step 7: Format and lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Expected: clean.

---

### Task 3: Mint `member:credential` across RBAC and the dashboard mirrors

**Files:**
- Modify `backend/crates/sauron-auth/src/rbac.rs` (`perm` module ~line 56, `perm::ALL` ~line 67, `ADMIN` ~line 110, test assertions at ~lines 812, 818, 836, 862, 875)
- Modify `dashboard/src/lib/models/index.ts` (`Permission` union ~line 182)
- Modify `dashboard/src/lib/models/permissions.ts` (`ALL_PERMISSIONS`, `PERMISSION_GROUPS`, `PERMISSION_LABELS`)

**Interfaces:**
- Consumes: the `UPDATE roles` in Task 1's migration (already applied).
- Produces: `sauron_auth::perm::MEMBER_CREDENTIAL: &str = "member:credential"`, `perm::ALL: [&str; 28]`, and the dashboard `Permission` variant `'member:credential'`.

These five edits **must ship together**: `dashboard/src/lib/models/permissions.test.ts` reads `backend/crates/sauron-auth/src/rbac.rs` at test time and compares the two catalogues in order, so a half-landed mirror fails the dashboard suite. Worse, the role editor submits its full checkbox state, so a permission missing from `ALL_PERMISSIONS` is silently stripped from every role on first save.

- [ ] **Step 1: Write the failing membership assertions first.** In `backend/crates/sauron-auth/src/rbac.rs`, inside the existing `#[cfg(test)] mod tests`, immediately after the `fn admin_is_all_except_org_manage` test, add:

```rust
    /// Counts alone cannot tell a Developer that accidentally gained a
    /// permission from a Developer that legitimately stayed at 18. Pin
    /// membership, because the failure the counts miss is a Developer who can
    /// sign any member out of their account.
    #[test]
    fn member_credential_reaches_only_owner_and_admin() {
        assert!(OWNER.permissions.contains(&perm::MEMBER_CREDENTIAL));
        assert!(ADMIN.permissions.contains(&perm::MEMBER_CREDENTIAL));
        assert!(!DEVELOPER.permissions.contains(&perm::MEMBER_CREDENTIAL));
        assert!(!VIEWER.permissions.contains(&perm::MEMBER_CREDENTIAL));
    }

    /// `member:credential` is carved OUT of `member:manage`, so it must never
    /// be granted to a role that cannot administer members at all.
    #[test]
    fn member_credential_never_appears_without_member_manage() {
        for preset in PRESETS {
            if preset.permissions.contains(&perm::MEMBER_CREDENTIAL) {
                assert!(
                    preset.permissions.contains(&perm::MEMBER_MANAGE),
                    "{} has member:credential without member:manage",
                    preset.name
                );
            }
        }
    }
```

- [ ] **Step 2: Run it and see it fail.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-auth member_credential`. Expected failure: `error[E0425]: cannot find value 'MEMBER_CREDENTIAL' in module 'perm'`.

- [ ] **Step 3: Declare the constant and add it to `perm::ALL`.** In the `pub mod perm` block, directly after `pub const MEMBER_MANAGE: &str = "member:manage";`, insert:

```rust
    /// Act on a member's *credentials*: end all of their sessions, and (in the
    /// password-reset slice) force a reset. Carved out of `MEMBER_MANAGE`
    /// rather than added beside it — "administer this org's membership" should
    /// not automatically confer the two verbs that take a person out of their
    /// own account, because control of the mail relay plus a forced reset is a
    /// path to account takeover. Routes gated on this ALSO re-check
    /// `MEMBER_MANAGE`; it narrows that permission, it does not stand in for it.
    pub const MEMBER_CREDENTIAL: &str = "member:credential";
```

Change the array declaration from `pub const ALL: [&str; 27] = [` to `pub const ALL: [&str; 28] = [`, and inside it insert `MEMBER_CREDENTIAL,` directly after `MEMBER_MANAGE,`.

- [ ] **Step 4: Add it to the `ADMIN` bag.** In `pub const ADMIN: PresetRole`, inside the `permissions: &[...]` list, insert `perm::MEMBER_CREDENTIAL,` directly after `perm::MEMBER_MANAGE,`. Do **not** touch `OWNER` (it is `&perm::ALL`), `DEVELOPER` or `VIEWER`.

- [ ] **Step 5: Re-pin the four preset counts.** Read each of these four assertions and change only the number where it moved:
  - in `fn owner_has_every_permission`: `assert_eq!(OWNER.permissions.len(), 27);` → `28`
  - in `fn admin_is_all_except_org_manage`: `assert_eq!(ADMIN.permissions.len(), 26);` → `27`
  - in `fn developer_can_write_issues_not_manage_members`: `assert_eq!(DEVELOPER.permissions.len(), 18);` → **stays 18**
  - in `fn viewer_is_read_only`: `assert_eq!(VIEWER.permissions.len(), 7);` → **stays 7**
  - in `fn all_permissions_are_unique`: `assert_eq!(perm::ALL.len(), 27);` → `28`

  Re-read all five rather than assuming the last two — a passing count is not evidence the permission landed in the right bag.

- [ ] **Step 6: Run the Rust tests and see them pass.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-auth`. Expected: all green, including `member_credential_reaches_only_owner_and_admin`.

- [ ] **Step 7: Run the dashboard suite and see it fail.** Run `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`. Expected failure in `permissions.test.ts`: the backend catalogue now has 28 entries and `ALL_PERMISSIONS` has 27, reported as a length or ordered-array mismatch naming `member:credential`.

- [ ] **Step 8: Mirror it in the dashboard.** In `dashboard/src/lib/models/index.ts`, in the `Permission` union, insert `| 'member:credential'` directly after `| 'member:manage'`. In `dashboard/src/lib/models/permissions.ts`:
  - in `ALL_PERMISSIONS`, insert `'member:credential',` directly after `'member:manage',` (order must match `perm::ALL`);
  - in `PERMISSION_GROUPS`, change the `Organization` entry's list to `['member:read', 'member:manage', 'member:credential', 'role:manage', 'org:manage']`;
  - in `PERMISSION_LABELS`, insert `'member:credential': 'Reset passwords and sign out devices',` directly after the `'member:manage'` entry.

- [ ] **Step 9: Run the dashboard suite and typecheck, and see them pass.** Run `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test` — expected green. Then `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check` — expected no new errors.

---

### Task 4: `sid` in the access token, and every call site of `issue_access`

**Files:**
- Modify `backend/crates/sauron-auth/src/jwt.rs` (`Claims` ~line 14, `issue_access` ~line 45, tests at ~lines 93, 103, 106, 111-145)
- Modify `backend/bins/sauron-api/src/routes/mod.rs` (line 105)
- Modify `backend/bins/sauron-api/tests/http_workflows.rs` (lines 378, 381)
- Modify `backend/bins/sauron-api/tests/http_env_scoping.rs` (lines 701, 704, 707, 710, 713, 1017, 2125)

**Interfaces:**
- Consumes: nothing.
- Produces: `sauron_auth::Claims { .., pub sid: Option<Uuid> }` and
  `JwtKeys::issue_access(&self, user_id: Uuid, must_change_password: bool, session_id: Option<Uuid>) -> anyhow::Result<(String, i64)>`.

The signature change breaks `cargo test --workspace` compilation before any test runs unless **all thirteen** call sites are updated in the same edit. Every test site passes `None`, and that is the semantically required choice, not the cheap one: those tests mint bearer tokens in-process and never create an `auth_sessions` row, so a synthetic `Some(uuid)` would produce a `sid` no row backs.

- [ ] **Step 1: Write the failing tests.** In `backend/crates/sauron-auth/src/jwt.rs`, inside `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn session_id_round_trips_as_the_sid_claim() {
        let keys = JwtKeys::new("test-secret-please-change-0000000000", 900);
        let uid = Uuid::new_v4();
        let sid = Uuid::new_v4();
        let (token, _) = keys.issue_access(uid, false, Some(sid)).unwrap();
        assert_eq!(keys.decode_access(&token).unwrap().sid, Some(sid));

        let (token, _) = keys.issue_access(uid, false, None).unwrap();
        assert_eq!(keys.decode_access(&token).unwrap().sid, None);
    }

    /// The property this whole slice exists to create: identity that survives a
    /// rotation. Two separately-minted tokens for the same session must name the
    /// same session — which is exactly why `sid` is not `jti` (`jti` is
    /// per-token and is regenerated on every call).
    #[test]
    fn two_tokens_for_one_session_carry_the_same_sid() {
        let keys = JwtKeys::new("test-secret-please-change-0000000000", 900);
        let uid = Uuid::new_v4();
        let sid = Uuid::new_v4();
        let (a, _) = keys.issue_access(uid, false, Some(sid)).unwrap();
        let (b, _) = keys.issue_access(uid, false, Some(sid)).unwrap();
        let (ca, cb) = (
            keys.decode_access(&a).unwrap(),
            keys.decode_access(&b).unwrap(),
        );
        assert_eq!(ca.sid, cb.sid);
        assert_ne!(ca.jti, cb.jti, "jti is per-token and must not be reused");
    }
```

Then, at the end of the existing `fn tokens_minted_before_the_flag_existed_still_decode`, after `assert!(!claims.must_change_password);`, add:

```rust
        // Same reason, one deploy later: a token minted before `sid` existed has
        // no such field. Rejecting it would sign out every logged-in user at
        // deploy — the exact failure this test was originally written to
        // prevent. A sid-less token is accepted, shows no "This device" badge,
        // and is refused by the two self-service revoke endpoints; the condition
        // clears within JWT_ACCESS_TTL_SECS because every login and refresh
        // mints a `sid`.
        assert_eq!(claims.sid, None);
```

- [ ] **Step 2: Run and see it fail.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-auth session_id_round_trips`. Expected failure: `error[E0061]: this method takes 2 arguments but 3 arguments were supplied` and `error[E0609]: no field 'sid' on type 'Claims'`.

- [ ] **Step 3: Add the claim and change the signature.** In `backend/crates/sauron-auth/src/jwt.rs`, add to `Claims` after `must_change_password`:

```rust
    /// The `auth_sessions.id` this token was minted for.
    ///
    /// `Option` + `serde(default)` because tokens issued before this field
    /// existed must keep decoding across the deploy — the same reason
    /// `must_change_password` is defaulted, and
    /// `tokens_minted_before_the_flag_existed_still_decode` is the pin.
    ///
    /// Deliberately not `jti`: `jti` is per-token and is regenerated on every
    /// rotation, so reusing it would destroy the identity-across-rotation
    /// property this field exists to create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<Uuid>,
```

Change `issue_access` to:

```rust
    /// Issue a signed access token; returns `(token, expires_at_unix)`.
    pub fn issue_access(
        &self,
        user_id: Uuid,
        must_change_password: bool,
        session_id: Option<Uuid>,
    ) -> anyhow::Result<(String, i64)> {
        let now = Utc::now().timestamp();
        let exp = now + self.access_ttl_secs;
        let claims = Claims {
            sub: user_id.to_string(),
            iat: now,
            exp,
            jti: sauron_core::ids::random_hex(8),
            typ: "access".to_string(),
            must_change_password,
            sid: session_id,
        };
        let token = encode(&Header::default(), &claims, &self.enc)
            .map_err(|e| anyhow::anyhow!("jwt encode: {e}"))?;
        Ok((token, exp))
    }
```

- [ ] **Step 4: Update the three in-file test call sites.** In the same file: `keys.issue_access(uid, false)` at ~line 93 becomes `keys.issue_access(uid, false, None)`; `keys.issue_access(uid, true)` at ~line 103 becomes `keys.issue_access(uid, true, None)`; `keys.issue_access(uid, false)` at ~line 106 becomes `keys.issue_access(uid, false, None)`.

- [ ] **Step 5: Update the production call site.** In `backend/bins/sauron-api/src/routes/mod.rs`, change `.issue_access(user_id, must_change_password)` to `.issue_access(user_id, must_change_password, None)`. This is a temporary shim — Task 8 rewrites the whole function and replaces the `None`.

- [ ] **Step 6: Update the nine integration-test call sites.** In `backend/bins/sauron-api/tests/http_workflows.rs`, change `.issue_access(owner.id, false)` → `.issue_access(owner.id, false, None)` and `.issue_access(no_event_read.id, false)` → `.issue_access(no_event_read.id, false, None)`. In `backend/bins/sauron-api/tests/http_env_scoping.rs`, apply the same `, None` to all seven sites: `owner.id`, `member.id`, `source_member.id`, `nav_member.id`, `org_owner.id`, `user_id`, `user.id`. Verify none were missed with `grep -rn "issue_access(" /home/splimter/projects/freelance/sauron/backend --include=*.rs` — every hit must have three arguments.

- [ ] **Step 7: Run the whole workspace check and the auth tests.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets` — expected clean. Then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-auth` — expected green, including `session_id_round_trips_as_the_sid_claim`, `two_tokens_for_one_session_carry_the_same_sid` and `tokens_minted_before_the_flag_existed_still_decode`.

---

### Task 5: The revocation-reason registry and its two pins

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (constants block at ~lines 205-215; add a `#[cfg(test)] mod tests` at the end of the file if none exists — check first with `grep -n "cfg(test)" backend/crates/sauron-db/src/repo.rs`)

**Interfaces:**
- Consumes: `up.sql` from Task 1 (read with `include_str!`).
- Produces: `repo::REVOKE_USER_REVOKED`, `repo::REVOKE_USER_REVOKED_OTHERS`, `repo::REVOKE_ADMIN`, `repo::DELIBERATE_REVOKE_REASONS: [&str; 3]`.

- [ ] **Step 1: Write the two failing tests.** At the very end of `backend/crates/sauron-db/src/repo.rs`, append:

```rust
#[cfg(test)]
mod revocation_reason_tests {
    use super::*;

    /// Every `REVOKE_*` constant must fall into exactly one of three buckets, so
    /// that a new reason cannot be added without someone choosing a bucket for
    /// it. The blanket formulation "everything except REVOKE_REUSE" is actively
    /// dangerous: it sweeps up `REVOKE_ROTATED` (which would send every ordinary
    /// rotation down the early-return path and break the 10-second multi-tab
    /// grace window) and `REVOKE_LOGOUT` (which would disable replay detection
    /// on exactly the tokens where a replay is most diagnostic).
    #[test]
    fn every_revocation_reason_is_classified() {
        /// Bucket two: `refresh` already handles these in a branch of its own.
        const HAS_ITS_OWN_BRANCH: [&str; 2] = [REVOKE_ROTATED, REVOKE_DEACTIVATED];
        /// Bucket three: presenting a token revoked for one of these IS worth
        /// the theft alarm, so they fall through to the family kill.
        /// `REVOKE_PASSWORD_CHANGED` belongs here and not in bucket one: the
        /// user changed their own password, every session died with it, and a
        /// token surfacing afterwards is exactly the replay the alarm is for.
        const FALLS_THROUGH_TO_THE_ALARM: [&str; 3] =
            [REVOKE_REUSE, REVOKE_LOGOUT, REVOKE_PASSWORD_CHANGED];

        const ALL_REASONS: [&str; 8] = [
            REVOKE_ROTATED,
            REVOKE_LOGOUT,
            REVOKE_REUSE,
            REVOKE_DEACTIVATED,
            REVOKE_PASSWORD_CHANGED,
            REVOKE_USER_REVOKED,
            REVOKE_USER_REVOKED_OTHERS,
            REVOKE_ADMIN,
        ];

        for reason in ALL_REASONS {
            let buckets = [
                DELIBERATE_REVOKE_REASONS.contains(&reason),
                HAS_ITS_OWN_BRANCH.contains(&reason),
                FALLS_THROUGH_TO_THE_ALARM.contains(&reason),
            ]
            .into_iter()
            .filter(|hit| *hit)
            .count();
            assert_eq!(
                buckets, 1,
                "{reason} must belong to exactly one bucket, not {buckets}"
            );
        }
    }

    /// The cheapest possible defence against the deploy coupling: a reason the
    /// `auth_sessions_revoked_reason_check` CHECK does not list makes the revoke
    /// path 500 in production, and nothing else in the build would notice.
    #[test]
    fn every_reason_that_can_revoke_a_session_is_in_the_check_constraint() {
        const UP_SQL: &str =
            include_str!("../../../migrations/2026-08-01-000035_auth_sessions/up.sql");

        // Every assertion below is scoped to the CHECK body, never to the whole
        // file. up.sql's prose comment names 'rotated' twice -- explaining why
        // it is EXCLUDED -- and names several of the accepted reasons as well,
        // so matching the file would test the comment: the positive assertions
        // would pass even against an empty constraint, and the negative one
        // would fail against a correct one.
        const MARKER: &str = "CONSTRAINT auth_sessions_revoked_reason_check CHECK (";
        let start = UP_SQL
            .find(MARKER)
            .expect("up.sql declares auth_sessions_revoked_reason_check by name")
            + MARKER.len();
        let after = &UP_SQL[start..];
        let end = after
            .find(");")
            .expect("the CHECK is the last item in CREATE TABLE auth_sessions");
        let check = &after[..end];

        for reason in [
            REVOKE_LOGOUT,
            REVOKE_REUSE,
            REVOKE_DEACTIVATED,
            REVOKE_PASSWORD_CHANGED,
            REVOKE_USER_REVOKED,
            REVOKE_USER_REVOKED_OTHERS,
            REVOKE_ADMIN,
        ] {
            assert!(
                check.contains(&format!("'{reason}'")),
                "'{reason}' is missing from auth_sessions_revoked_reason_check; the revoke path \
                 will 500 in production"
            );
        }

        // A rotation revokes a token, never a session. Listing it would make the
        // database stop catching that bug.
        assert!(
            !check.contains(&format!("'{REVOKE_ROTATED}'")),
            "'{REVOKE_ROTATED}' must NOT be accepted by auth_sessions_revoked_reason_check"
        );
    }
}
```

- [ ] **Step 2: Run and see it fail.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db revocation_reason_tests`. Expected failure: `error[E0425]: cannot find value 'REVOKE_USER_REVOKED' in this scope` (and two more like it) plus `error[E0425]: cannot find value 'DELIBERATE_REVOKE_REASONS'`.

- [ ] **Step 3: Add the three constants and the registry.** In `backend/crates/sauron-db/src/repo.rs`, directly after `pub const REVOKE_PASSWORD_CHANGED: &str = "password_changed";`, insert:

```rust
/// The owner ended one specific session from their own account page.
pub const REVOKE_USER_REVOKED: &str = "user_revoked";
/// The owner pressed "sign out other devices" — every session but the caller's.
pub const REVOKE_USER_REVOKED_OTHERS: &str = "user_revoked_others";
/// An administrator with `member:credential` signed a member out of everything.
pub const REVOKE_ADMIN: &str = "admin_revoked";

/// Reasons that mean a human deliberately ended the session. Presenting a token
/// revoked for one of these is NOT evidence of theft and must not trip the
/// family kill in `refresh`.
///
/// Without this, a device killed by "sign out other devices" presents its dead
/// token on its existing 15-minute timer, lands in the reuse branch, and kills
/// the user's WHOLE family — including the session they explicitly chose to
/// keep. The symptom reads as "sign out other devices logs me out too, about
/// fifteen minutes later", which looks like a flaky bug rather than a design
/// fault.
///
/// This is "deliberate, not theft" — not "every reason". `REVOKE_ROTATED` and
/// `REVOKE_REUSE` must never appear here: rotation would take the early-return
/// path on every ordinary refresh and break the 10-second multi-tab grace
/// window, and reuse is the theft signal itself. `REVOKE_LOGOUT` and
/// `REVOKE_PASSWORD_CHANGED` stay out too — a token surfacing after either is
/// exactly the replay the alarm exists for.
///
/// Adding a reason that `auth_sessions_revoked_reason_check` does not already
/// list ALSO needs a widening migration, or the revoke path 500s.
pub const DELIBERATE_REVOKE_REASONS: [&str; 3] =
    [REVOKE_USER_REVOKED, REVOKE_USER_REVOKED_OTHERS, REVOKE_ADMIN];
```

- [ ] **Step 4: Run and see it pass.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db revocation_reason_tests`. Expected: `2 passed`.

- [ ] **Step 5: Format and lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Expected: clean.

---

### Task 6: `repo::start_or_continue_session` and the Postgres integration harness

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (add after `insert_refresh_token`, which stays for now)
- Create `backend/crates/sauron-db/tests/sessions.rs`

**Interfaces:**
- Consumes: `auth_sessions` (Task 1), `refresh_tokens.session_id` (Task 2).
- Produces:

```rust
pub async fn start_or_continue_session(
    conn: &mut AsyncPgConnection,
    session_id: Uuid,
    user_id: Uuid,
    token_hash: String,
    expires_at: DateTime<Utc>,
    user_agent: Option<String>,
    ip: Option<String>,
) -> QueryResult<usize>
```

- [ ] **Step 1: Write the failing integration test.** Create `backend/crates/sauron-db/tests/sessions.rs`:

```rust
//! `auth_sessions` against a real Postgres: the mint/continue upsert, the four
//! revoke helpers, the list query and the reaper.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset, mirroring
//! `env_scoping.rs` and `workflows.rs`. CI has no database service.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use sauron_db::models::{AuthSession, RefreshToken};
use sauron_db::repo;
use sauron_db::schema::{auth_sessions, refresh_tokens};
use uuid::Uuid;

/// A real `users` row — every `auth_sessions` FK needs one.
async fn seed_user(conn: &mut sauron_db::PgConn) -> Uuid {
    let suffix = Uuid::new_v4().simple().to_string();
    repo::create_user(
        conn,
        &format!("sessions-{suffix}@example.com"),
        "not-a-real-hash",
        "Sessions Fixture",
    )
    .await
    .expect("create user")
    .id
}

async fn session_row(conn: &mut sauron_db::PgConn, id: Uuid) -> AuthSession {
    auth_sessions::table
        .find(id)
        .select(AuthSession::as_select())
        .first(conn)
        .await
        .expect("auth_sessions row")
}

async fn tokens_for_session(conn: &mut sauron_db::PgConn, id: Uuid) -> Vec<RefreshToken> {
    refresh_tokens::table
        .filter(refresh_tokens::session_id.eq(id))
        .select(RefreshToken::as_select())
        .order(refresh_tokens::created_at.asc())
        .load(conn)
        .await
        .expect("refresh_tokens rows")
}

#[tokio::test]
async fn a_fresh_session_id_creates_one_session_and_one_token() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let sid = Uuid::new_v4();

    let rows = repo::start_or_continue_session(
        &mut conn,
        sid,
        user_id,
        "hash-a".to_string(),
        Utc::now() + Duration::days(30),
        Some("Mozilla/5.0 (Macintosh)".to_string()),
        Some("203.0.113.7".to_string()),
    )
    .await
    .expect("start session");
    assert_eq!(rows, 1, "one refresh token inserted");

    let session = session_row(&mut conn, sid).await;
    assert_eq!(session.user_id, user_id);
    assert_eq!(session.user_agent.as_deref(), Some("Mozilla/5.0 (Macintosh)"));
    assert_eq!(session.ip.as_deref(), Some("203.0.113.7"));
    assert!(session.revoked_at.is_none());
    assert_eq!(tokens_for_session(&mut conn, sid).await.len(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn continuing_a_session_adds_a_token_and_keeps_the_login_time_facts() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let sid = Uuid::new_v4();
    let first_expiry = Utc::now() + Duration::days(10);

    repo::start_or_continue_session(
        &mut conn,
        sid,
        user_id,
        "hash-a".to_string(),
        first_expiry,
        Some("original-agent".to_string()),
        Some("203.0.113.7".to_string()),
    )
    .await
    .expect("start session");
    let before = session_row(&mut conn, sid).await;

    let second_expiry = Utc::now() + Duration::days(30);
    let rows = repo::start_or_continue_session(
        &mut conn,
        sid,
        user_id,
        "hash-b".to_string(),
        second_expiry,
        Some("attacker-agent".to_string()),
        Some("198.51.100.9".to_string()),
    )
    .await
    .expect("continue session");
    assert_eq!(rows, 1);

    let after = session_row(&mut conn, sid).await;
    assert_eq!(tokens_for_session(&mut conn, sid).await.len(), 2);
    assert!(after.last_used_at > before.last_used_at, "last_used_at bumped");
    assert!(after.expires_at > before.expires_at, "expires_at slides");
    // The UI renders these next to created_at as login-time facts ("was that
    // login me?"). Last-writer-wins would let whoever rotates the token destroy
    // the original login's evidence and leave the row looking unchanged.
    assert_eq!(after.user_agent.as_deref(), Some("original-agent"));
    assert_eq!(after.ip.as_deref(), Some("203.0.113.7"));
    assert_eq!(after.created_at, before.created_at);

    db.cleanup().await;
}

#[tokio::test]
async fn a_revoked_session_cannot_be_resurrected_by_a_rotation() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let sid = Uuid::new_v4();

    repo::start_or_continue_session(
        &mut conn,
        sid,
        user_id,
        "hash-a".to_string(),
        Utc::now() + Duration::days(30),
        None,
        None,
    )
    .await
    .expect("start session");

    diesel::update(auth_sessions::table.find(sid))
        .set((
            auth_sessions::revoked_at.eq(Utc::now()),
            auth_sessions::revoked_reason.eq(repo::REVOKE_USER_REVOKED_OTHERS),
        ))
        .execute(&mut conn)
        .await
        .expect("revoke session");

    // The centrepiece guard, asserted at the SQL layer where it lives: the
    // 10-second refresh-race grace window can otherwise resurrect a session the
    // user just killed. A Rust-side pre-check would be a TOCTOU race and must
    // not be substituted.
    let rows = repo::start_or_continue_session(
        &mut conn,
        sid,
        user_id,
        "hash-b".to_string(),
        Utc::now() + Duration::days(30),
        None,
        None,
    )
    .await
    .expect("continue a revoked session must not error");
    assert_eq!(rows, 0, "no token may be minted for a revoked session");
    assert_eq!(tokens_for_session(&mut conn, sid).await.len(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn a_session_id_belonging_to_another_user_mints_nothing() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let owner = seed_user(&mut conn).await;
    let stranger = seed_user(&mut conn).await;
    let sid = Uuid::new_v4();

    repo::start_or_continue_session(
        &mut conn,
        sid,
        owner,
        "hash-a".to_string(),
        Utc::now() + Duration::days(30),
        None,
        None,
    )
    .await
    .expect("start session");

    let rows = repo::start_or_continue_session(
        &mut conn,
        sid,
        stranger,
        "hash-b".to_string(),
        Utc::now() + Duration::days(30),
        None,
        None,
    )
    .await
    .expect("mis-threaded session id must not error");
    assert_eq!(rows, 0, "a mis-threaded session id must never cross-link users");

    db.cleanup().await;
}
```

- [ ] **Step 2: Check whether the harness exposes `cleanup`.** Run `grep -n "pub async fn cleanup" /home/splimter/projects/freelance/sauron/backend/crates/sauron-db/tests/common/mod.rs`. If the method is named differently, use that name in every `db.cleanup().await;` above. Do not skip it — the harness's `Drop` prints a warning and leaks the ephemeral database.

- [ ] **Step 3: Run and see it fail.** Run `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test sessions`. Expected failure: `error[E0425]: cannot find function 'start_or_continue_session' in module 'repo'`.

- [ ] **Step 4: Implement it.** In `backend/crates/sauron-db/src/repo.rs`, directly after `insert_refresh_token`, insert:

```rust
/// Mint a refresh token, starting a new session or continuing an existing one.
///
/// One data-modifying CTE rather than a transaction: `conn.transaction`'s
/// diesel-async 0.9 signature needs async closures (Rust 1.85) and this
/// workspace's MSRV is 1.82 per `packaging/rpm/sauron.spec`. Postgres runs both
/// statements atomically within the one statement anyway.
///
/// Returns the number of refresh-token rows inserted: `1` on success, `0` when
/// the session exists but is revoked or belongs to somebody else.
///
/// Three things in the SQL are load-bearing.
///
/// **`WHERE auth_sessions.revoked_at IS NULL` is the only thing stopping the
/// rotation grace window from resurrecting a killed session.**
/// `REFRESH_RACE_GRACE` is 10 seconds and exists because two dashboard tabs
/// share one localStorage refresh token. Concretely: session B rotates at T (old
/// token -> `rotated`); the user revokes B at T+2s; B's other tab presents the
/// old token at T+3s. The reason *is* `rotated`, it *is* inside the grace, and
/// `user_has_active_refresh_token` is *true* because the spared session A is
/// live — so the race path runs. Postgres's `DO UPDATE ... WHERE` skips the
/// update, `RETURNING` yields nothing, the outer INSERT inserts nothing, and
/// this returns 0. A Rust-side pre-check would be a TOCTOU race and must not be
/// substituted; if a refactor moves the session upsert out of this CTE, the hole
/// silently reopens.
///
/// **The COALESCE order is inverted from the obvious.** `EXCLUDED` holds the
/// *current rotator's* values, so `COALESCE(EXCLUDED.x, auth_sessions.x)` would
/// overwrite the row every 15 minutes. The UI renders these beside `created_at`
/// as login-time facts, and the sole justification for returning an unmasked IP
/// is "was that login me?" — so if an attacker steals a refresh token and
/// rotates it, last-writer-wins would destroy the original login IP and user
/// agent and leave the row visually unchanged. If a "currently seen from" value
/// is ever wanted, it gets its own columns and its own label.
///
/// **`user_id = $2` in the WHERE is defensive**: it makes a mis-threaded
/// `session_id` fail rather than cross-link two users' tokens.
///
/// The outer INSERT deliberately omits `user_agent`. Writing the sanitized UA to
/// `refresh_tokens` on every rotation (~96 times a day per session) persists
/// ~120 bytes nothing reads — `list_sessions` never touches that table and the
/// same string is already on the session row — on the workspace's
/// fastest-growing never-pruned table.
pub async fn start_or_continue_session(
    conn: &mut AsyncPgConnection,
    session_id: Uuid,
    user_id: Uuid,
    token_hash: String,
    expires_at: DateTime<Utc>,
    user_agent: Option<String>,
    ip: Option<String>,
) -> QueryResult<usize> {
    diesel::sql_query(
        "WITH s AS ( \
           INSERT INTO auth_sessions (id, user_id, expires_at, user_agent, ip) \
           VALUES ($1,$2,$3,$4,$5) \
           ON CONFLICT (id) DO UPDATE \
              SET last_used_at = now(), \
                  expires_at   = EXCLUDED.expires_at, \
                  user_agent   = COALESCE(auth_sessions.user_agent, EXCLUDED.user_agent), \
                  ip           = COALESCE(auth_sessions.ip, EXCLUDED.ip) \
            WHERE auth_sessions.revoked_at IS NULL AND auth_sessions.user_id = $2 \
           RETURNING id) \
         INSERT INTO refresh_tokens (user_id, token_hash, expires_at, session_id) \
         SELECT $2,$6,$3,s.id FROM s",
    )
    .bind::<SqlUuid, _>(session_id)
    .bind::<SqlUuid, _>(user_id)
    .bind::<Timestamptz, _>(expires_at)
    .bind::<Nullable<Text>, _>(user_agent)
    .bind::<Nullable<Text>, _>(ip)
    .bind::<Text, _>(token_hash)
    .execute(conn)
    .await
}
```

- [ ] **Step 5: Run and see it pass.** Run `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test sessions`. Expected: `4 passed`.

- [ ] **Step 6: Prove the tests really skip without a database.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test sessions` (no `TEST_DATABASE_URL`). Expected: `4 passed` with no database contacted — the `let Some(db) = ... else { return; }` guard makes each a no-op.

- [ ] **Step 7: Format and lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Expected: clean.

---

### Task 7: The session-aware revoke, list, poll and prune repo functions

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (after `start_or_continue_session`; also widen `refresh_token_revocation` at ~line 232 and extend `user_has_active_refresh_token`'s doc comment at ~line 250)
- Modify `backend/crates/sauron-db/tests/sessions.rs` (append tests)

**Interfaces:**
- Consumes: `AuthSession` (Task 2), the reason constants (Task 5).
- Produces:

```rust
pub const MAX_SESSIONS_LISTED: i64 = 200;
pub const AUTH_SESSION_RETENTION_DAYS: i64 = 30;

pub async fn revoke_session(conn: &mut AsyncPgConnection, session_id: Uuid, user_id: Uuid, reason: &str, actor: Option<Uuid>) -> QueryResult<Vec<Uuid>>;
pub async fn revoke_sessions_for_user(conn: &mut AsyncPgConnection, user_id: Uuid, except: Option<Uuid>, reason: &str, actor: Option<Uuid>) -> QueryResult<Vec<Uuid>>;
pub async fn revoke_refresh_token_and_session(conn: &mut AsyncPgConnection, token_hash: &str, reason: &str) -> QueryResult<Option<Uuid>>;
pub async fn list_sessions(conn: &mut AsyncPgConnection, user_id: Uuid, include_revoked: bool) -> QueryResult<Vec<AuthSession>>;
pub async fn revoked_session_ids(conn: &mut AsyncPgConnection, window_secs: i64) -> QueryResult<Vec<Uuid>>;
pub async fn prune_auth_sessions(conn: &mut AsyncPgConnection, days: i64) -> QueryResult<usize>;
pub async fn refresh_token_revocation(conn: &mut AsyncPgConnection, token_hash: &str)
    -> QueryResult<Option<(Uuid, Option<Uuid>, Option<DateTime<Utc>>, Option<String>)>>;  // (user_id, session_id, revoked_at, revoked_reason)
```

- [ ] **Step 1: Append the failing tests.** Add to the end of `backend/crates/sauron-db/tests/sessions.rs`:

```rust
/// Mint one session with one live token. Returns the session id.
async fn seed_session(conn: &mut sauron_db::PgConn, user_id: Uuid, hash: &str) -> Uuid {
    let sid = Uuid::new_v4();
    repo::start_or_continue_session(
        conn,
        sid,
        user_id,
        hash.to_string(),
        Utc::now() + Duration::days(30),
        Some("agent".to_string()),
        Some("203.0.113.7".to_string()),
    )
    .await
    .expect("seed session");
    sid
}

async fn live_token_count(conn: &mut sauron_db::PgConn, user_id: Uuid) -> i64 {
    refresh_tokens::table
        .filter(refresh_tokens::user_id.eq(user_id))
        .filter(refresh_tokens::revoked_at.is_null())
        .count()
        .get_result(conn)
        .await
        .expect("count live tokens")
}

#[tokio::test]
async fn revoke_session_is_scoped_to_its_owner_and_reports_the_session_not_the_tokens() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let owner = seed_user(&mut conn).await;
    let stranger = seed_user(&mut conn).await;
    let sid = seed_session(&mut conn, owner, "hash-a").await;

    // Absent, already-revoked and someone-else's all look identical, so the
    // handler can map every empty result to 404 and the response cannot be used
    // to probe which session ids exist.
    let foreign = repo::revoke_session(&mut conn, sid, stranger, repo::REVOKE_USER_REVOKED, Some(stranger))
        .await
        .expect("foreign revoke");
    assert!(foreign.is_empty(), "one user must never revoke another's session");
    assert!(session_row(&mut conn, sid).await.revoked_at.is_none());

    // A live session whose refresh tokens are all already dead must still report
    // itself as revoked. Reading the token UPDATE's row count instead would
    // answer 404 on a successful revoke, and the handler would then skip
    // `mark_revoked` — leaving the killed session's access token good for the
    // full 900s.
    diesel::update(refresh_tokens::table.filter(refresh_tokens::session_id.eq(sid)))
        .set((
            refresh_tokens::revoked_at.eq(Utc::now()),
            refresh_tokens::revoked_reason.eq(repo::REVOKE_ROTATED),
        ))
        .execute(&mut conn)
        .await
        .expect("kill the session's tokens");

    let ids = repo::revoke_session(&mut conn, sid, owner, repo::REVOKE_USER_REVOKED, Some(owner))
        .await
        .expect("owner revoke");
    assert_eq!(ids, vec![sid]);
    let row = session_row(&mut conn, sid).await;
    assert!(row.revoked_at.is_some());
    assert_eq!(row.revoked_reason.as_deref(), Some(repo::REVOKE_USER_REVOKED));
    assert_eq!(row.revoked_by, Some(owner));

    // Second call is a no-op, not an error.
    let again = repo::revoke_session(&mut conn, sid, owner, repo::REVOKE_USER_REVOKED, Some(owner))
        .await
        .expect("idempotent revoke");
    assert!(again.is_empty());

    db.cleanup().await;
}

#[tokio::test]
async fn revoke_sessions_for_user_spares_the_exception_and_leaves_no_live_token_behind() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let keep = seed_session(&mut conn, user_id, "hash-keep").await;
    let kill = seed_session(&mut conn, user_id, "hash-kill").await;

    // A session-less legacy token, minted before migration 000035. "Sign out my
    // other devices" means those too.
    diesel::insert_into(refresh_tokens::table)
        .values((
            refresh_tokens::user_id.eq(user_id),
            refresh_tokens::token_hash.eq("hash-legacy"),
            refresh_tokens::expires_at.eq(Utc::now() + Duration::days(30)),
        ))
        .execute(&mut conn)
        .await
        .expect("insert legacy token");

    let ids = repo::revoke_sessions_for_user(
        &mut conn,
        user_id,
        Some(keep),
        repo::REVOKE_USER_REVOKED_OTHERS,
        Some(user_id),
    )
    .await
    .expect("revoke others");
    assert_eq!(ids, vec![kill]);
    assert!(session_row(&mut conn, keep).await.revoked_at.is_none());
    assert!(session_row(&mut conn, kill).await.revoked_at.is_some());
    assert_eq!(
        live_token_count(&mut conn, user_id).await,
        1,
        "only the spared session's token may survive; the legacy token dies too"
    );

    // With no exception, nothing may be left alive — otherwise
    // `user_has_active_refresh_token` still returns true after an account-wide
    // kill and the grace window's invariant weakens.
    let ids = repo::revoke_sessions_for_user(
        &mut conn,
        user_id,
        None,
        repo::REVOKE_ADMIN,
        Some(user_id),
    )
    .await
    .expect("revoke all");
    assert_eq!(ids, vec![keep]);
    assert_eq!(live_token_count(&mut conn, user_id).await, 0);

    db.cleanup().await;
}

#[tokio::test]
async fn logout_takes_the_session_with_the_token_but_never_rewrites_a_rotation() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let sid = seed_session(&mut conn, user_id, "hash-a").await;

    let revoked = repo::revoke_refresh_token_and_session(&mut conn, "hash-a", repo::REVOKE_LOGOUT)
        .await
        .expect("logout");
    assert_eq!(revoked, Some(sid));
    assert!(session_row(&mut conn, sid).await.revoked_at.is_some());

    // Rotate a second session, then "log out" with the already-rotated token.
    // Rewriting `rotated` to `logout` would stop the 10-second multi-tab grace
    // window firing for the other tab.
    let sid2 = seed_session(&mut conn, user_id, "hash-b").await;
    repo::revoke_refresh_token(&mut conn, "hash-b", repo::REVOKE_ROTATED)
        .await
        .expect("rotate");
    let revoked = repo::revoke_refresh_token_and_session(&mut conn, "hash-b", repo::REVOKE_LOGOUT)
        .await
        .expect("logout with a rotated token");
    assert_eq!(revoked, None);
    let (_, _, _, reason) = repo::refresh_token_revocation(&mut conn, "hash-b")
        .await
        .expect("revocation metadata")
        .expect("row exists");
    assert_eq!(reason.as_deref(), Some(repo::REVOKE_ROTATED));
    assert!(session_row(&mut conn, sid2).await.revoked_at.is_none());

    db.cleanup().await;
}

#[tokio::test]
async fn list_sessions_hides_revoked_and_expired_rows_unless_asked() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let live = seed_session(&mut conn, user_id, "hash-live").await;
    let dead = seed_session(&mut conn, user_id, "hash-dead").await;
    let stale = seed_session(&mut conn, user_id, "hash-stale").await;

    repo::revoke_session(&mut conn, dead, user_id, repo::REVOKE_USER_REVOKED, Some(user_id))
        .await
        .expect("revoke");
    diesel::update(auth_sessions::table.find(stale))
        .set(auth_sessions::expires_at.eq(Utc::now() - Duration::days(1)))
        .execute(&mut conn)
        .await
        .expect("expire");

    let listed = repo::list_sessions(&mut conn, user_id, false)
        .await
        .expect("list live");
    assert_eq!(listed.iter().map(|s| s.id).collect::<Vec<_>>(), vec![live]);

    let listed = repo::list_sessions(&mut conn, user_id, true)
        .await
        .expect("list with history");
    let ids: Vec<Uuid> = listed.iter().map(|s| s.id).collect();
    assert!(ids.contains(&live));
    assert!(ids.contains(&dead), "a revocation inside 30 days is history the owner may see");
    assert!(!ids.contains(&stale), "an expired-but-never-revoked row is not history");

    db.cleanup().await;
}

#[tokio::test]
async fn revoked_session_ids_returns_only_the_recent_window() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let recent = seed_session(&mut conn, user_id, "hash-recent").await;
    let ancient = seed_session(&mut conn, user_id, "hash-ancient").await;

    repo::revoke_session(&mut conn, recent, user_id, repo::REVOKE_USER_REVOKED, Some(user_id))
        .await
        .expect("revoke recent");
    repo::revoke_session(&mut conn, ancient, user_id, repo::REVOKE_USER_REVOKED, Some(user_id))
        .await
        .expect("revoke ancient");
    diesel::update(auth_sessions::table.find(ancient))
        .set(auth_sessions::revoked_at.eq(Utc::now() - Duration::hours(6)))
        .execute(&mut conn)
        .await
        .expect("age the ancient revocation");

    let ids = repo::revoked_session_ids(&mut conn, 1020)
        .await
        .expect("poll");
    assert!(ids.contains(&recent));
    assert!(
        !ids.contains(&ancient),
        "a revocation older than the window cannot have a live access token left"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn the_reaper_deletes_old_sessions_and_orphans_their_tokens_rather_than_deleting_them() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let old = seed_session(&mut conn, user_id, "hash-old").await;
    let fresh = seed_session(&mut conn, user_id, "hash-fresh").await;

    repo::revoke_session(&mut conn, old, user_id, repo::REVOKE_USER_REVOKED, Some(user_id))
        .await
        .expect("revoke");
    diesel::update(auth_sessions::table.find(old))
        .set(auth_sessions::revoked_at.eq(Utc::now() - Duration::days(40)))
        .execute(&mut conn)
        .await
        .expect("age it");

    let deleted = repo::prune_auth_sessions(&mut conn, repo::AUTH_SESSION_RETENTION_DAYS)
        .await
        .expect("prune");
    assert_eq!(deleted, 1);
    assert!(session_row(&mut conn, fresh).await.revoked_at.is_none());

    // ON DELETE SET NULL, not CASCADE. Deleting the token history would make a
    // replayed token look like one that never existed: a plain 401, no family
    // kill, no WARN.
    let orphan: RefreshToken = refresh_tokens::table
        .filter(refresh_tokens::token_hash.eq("hash-old"))
        .select(RefreshToken::as_select())
        .first(&mut conn)
        .await
        .expect("token row survives the reap");
    assert_eq!(orphan.session_id, None);

    db.cleanup().await;
}
```

- [ ] **Step 2: Run and see it fail.** Run `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test sessions`. Expected failure: `error[E0425]: cannot find function 'revoke_session' in module 'repo'` plus the same for `revoke_sessions_for_user`, `revoke_refresh_token_and_session`, `list_sessions`, `revoked_session_ids`, `prune_auth_sessions`, and `error[E0308]` on the 4-tuple destructuring of `refresh_token_revocation`.

- [ ] **Step 3: Add the row type and the three revoke functions.** In `backend/crates/sauron-db/src/repo.rs`, directly after `start_or_continue_session`, insert:

```rust
/// One id returned by a revoke CTE. The primary query must always be
/// `SELECT id FROM s` — see the note on [`revoke_session`].
#[derive(Debug, QueryableByName)]
pub struct RevokedSessionRow {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
}

/// End one session the caller owns. Returns the ids actually revoked (0 or 1).
///
/// **The `auth_sessions` UPDATE must be the row-count-bearing statement.**
/// Postgres reports the command tag of the *outer* statement, so the obvious
/// shape — `WITH s AS (UPDATE auth_sessions ... RETURNING id) UPDATE
/// refresh_tokens ... FROM s`, read with `.execute()` — returns the number of
/// *token* rows touched. A live session that currently has no live refresh token
/// would then revoke successfully in the database while the handler answered
/// 404, and because the handler only updates its in-process snapshot on the
/// success branch, the killed session's access token would keep working for the
/// full 900s. That state is reachable and is *created* by this slice, because
/// deactivation used to kill tokens without touching sessions. So: session
/// UPDATE in one CTE arm, token UPDATE in a second, `SELECT id FROM s` as the
/// primary query. Data-modifying CTE arms execute exactly once and to completion
/// whether or not the primary query reads them, which is why the token arm can
/// be a bare `RETURNING r.id` nobody selects.
///
/// `user_id = $2` is the ownership check. It is why the handler needs no
/// separate SELECT, why there is no window between check and write, and why one
/// user can never revoke another's session by guessing a uuid. An empty result
/// means absent, already revoked, or someone else's — all mapped to 404, never
/// 403, so the response cannot be used to probe which session ids exist.
pub async fn revoke_session(
    conn: &mut AsyncPgConnection,
    session_id: Uuid,
    user_id: Uuid,
    reason: &str,
    actor: Option<Uuid>,
) -> QueryResult<Vec<Uuid>> {
    let rows: Vec<RevokedSessionRow> = diesel::sql_query(
        "WITH s AS ( \
           UPDATE auth_sessions SET revoked_at = now(), revoked_reason = $3, revoked_by = $4 \
            WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL \
           RETURNING id), \
         t AS ( \
           UPDATE refresh_tokens r SET revoked_at = now(), revoked_reason = $3 \
             FROM s WHERE r.session_id = s.id AND r.revoked_at IS NULL \
           RETURNING r.id) \
         SELECT id FROM s",
    )
    .bind::<SqlUuid, _>(session_id)
    .bind::<SqlUuid, _>(user_id)
    .bind::<Text, _>(reason)
    .bind::<Nullable<SqlUuid>, _>(actor)
    .get_results(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}

/// End every live session for a user, optionally sparing one. Returns the ids
/// revoked, which the caller must hand to the in-process revocation snapshot or
/// the kill is invisible until the next poll.
///
/// The token arm expresses the sparing rule **directly** rather than as
/// `session_id IN (SELECT id FROM s)`. The `IN` form silently skips live tokens
/// whose session was already revoked — reachable, because the refresh race path
/// issues a new token without revoking the presented one (so one session can
/// hold two live tokens) and `logout` revokes one hash while revoking the
/// session. Those leftovers are inert for minting, but they make
/// `user_has_active_refresh_token` return true after an account-wide kill,
/// weakening the invariant the grace window depends on. `IS DISTINCT FROM` also
/// gives the right NULL semantics: session-less legacy tokens are still killed
/// by "revoke others", which is correct — a user asking to kill their other
/// devices means those too.
pub async fn revoke_sessions_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    except: Option<Uuid>,
    reason: &str,
    actor: Option<Uuid>,
) -> QueryResult<Vec<Uuid>> {
    let rows: Vec<RevokedSessionRow> = diesel::sql_query(
        "WITH s AS ( \
           UPDATE auth_sessions SET revoked_at = now(), revoked_reason = $2, revoked_by = $3 \
            WHERE user_id = $1 AND revoked_at IS NULL \
              AND ($4::uuid IS NULL OR id <> $4) \
           RETURNING id), \
         t AS ( \
           UPDATE refresh_tokens r SET revoked_at = now(), revoked_reason = $2 \
            WHERE r.user_id = $1 AND r.revoked_at IS NULL \
              AND ($4::uuid IS NULL OR r.session_id IS DISTINCT FROM $4) \
           RETURNING r.id) \
         SELECT id FROM s",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<Text, _>(reason)
    .bind::<Nullable<SqlUuid>, _>(actor)
    .bind::<Nullable<SqlUuid>, _>(except)
    .get_results(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}

/// Revoke one refresh token by hash and take its session with it. Returns the
/// session id if one was ended.
///
/// Today's `logout` revokes the token and leaves the session live in the owner's
/// list forever — dead token, live row. The `AND revoked_at IS NULL` guard is a
/// small, deliberate behaviour change from `revoke_refresh_token`: without it,
/// logging out with an already-rotated token rewrites `revoked_reason` from
/// `rotated` to `logout`, and the 10-second grace window (which fires only on
/// `rotated`) stops firing for the other tab. This does not widen logout's
/// authorization surface — whoever holds the raw refresh token could already
/// revoke it.
pub async fn revoke_refresh_token_and_session(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
    reason: &str,
) -> QueryResult<Option<Uuid>> {
    let rows: Vec<RevokedSessionRow> = diesel::sql_query(
        "WITH t AS ( \
           UPDATE refresh_tokens SET revoked_at = now(), revoked_reason = $2 \
            WHERE token_hash = $1 AND revoked_at IS NULL \
           RETURNING session_id), \
         s AS ( \
           UPDATE auth_sessions a SET revoked_at = now(), revoked_reason = $2 \
             FROM t WHERE a.id = t.session_id AND a.revoked_at IS NULL \
           RETURNING a.id) \
         SELECT id FROM s",
    )
    .bind::<Text, _>(token_hash)
    .bind::<Text, _>(reason)
    .get_results(conn)
    .await?;
    Ok(rows.into_iter().next().map(|r| r.id))
}
```

- [ ] **Step 4: Add the list, poll and prune functions.** Immediately after the three above, insert:

```rust
/// Ceiling on how many session rows one account may render. A real account has
/// single-digit live sessions; this exists so a pathological one cannot turn a
/// user-facing page into an unbounded scan.
pub const MAX_SESSIONS_LISTED: i64 = 200;

/// How long a revoked or long-expired session row is kept.
///
/// A compile-time constant, not an environment variable: three files of
/// documentation for a value nobody tunes is how a config surface becomes
/// unmaintainable.
pub const AUTH_SESSION_RETENTION_DAYS: i64 = 30;

/// The caller's own sessions, live first.
///
/// **Never touches `refresh_tokens`.** That is the structural reason
/// `token_hash` cannot leak through the session endpoint: the column is not in
/// this query's source table, so no careless `select()` can reach it.
pub async fn list_sessions(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    include_revoked: bool,
) -> QueryResult<Vec<AuthSession>> {
    let now = Utc::now();
    let mut rows = auth_sessions::table
        .filter(auth_sessions::user_id.eq(user_id))
        .filter(auth_sessions::revoked_at.is_null())
        .filter(auth_sessions::expires_at.gt(now))
        .select(AuthSession::as_select())
        .order(auth_sessions::last_used_at.desc())
        .limit(MAX_SESSIONS_LISTED)
        .load(conn)
        .await?;

    if include_revoked {
        // Served by `auth_sessions_revoked_idx`. The reaper is what keeps that
        // partial index small enough that this needs no `user_id` support: the
        // whole index is the last 30 days of revocations.
        let cutoff = now - chrono::Duration::days(AUTH_SESSION_RETENTION_DAYS);
        let revoked = auth_sessions::table
            .filter(auth_sessions::user_id.eq(user_id))
            .filter(auth_sessions::revoked_at.ge(cutoff))
            .select(AuthSession::as_select())
            .order(auth_sessions::revoked_at.desc())
            .limit(MAX_SESSIONS_LISTED)
            .load(conn)
            .await?;
        rows.extend(revoked);
    }
    Ok(rows)
}

/// Session ids revoked within the last `window_secs`, for the per-replica
/// revocation snapshot.
///
/// The cutoff is computed **in the database**. Computing `Utc::now() - window`
/// in Rust would make the control depend on API-vs-Postgres clock skew:
/// `revoked_at` is written by Postgres `now()`, so an API host running ahead by
/// more than the slack would drop recently-revoked sessions from its snapshot
/// and silently re-enable their access tokens — invisibly, because the poll
/// succeeded.
///
/// The `LIMIT` is not decoration: without it a plausible
/// `JWT_ACCESS_TTL_SECS=604800` plus any bulk event (an offboarding script, a
/// deactivation sweep) has every replica materialising an enormous set 17 280
/// times a day and swapping it into a lock every authenticated request reads.
/// The caller logs at ERROR when the limit is hit — a silently truncated
/// snapshot is a security control that has stopped working while reporting
/// healthy.
pub async fn revoked_session_ids(
    conn: &mut AsyncPgConnection,
    window_secs: i64,
) -> QueryResult<Vec<Uuid>> {
    let rows: Vec<RevokedSessionRow> = diesel::sql_query(
        "SELECT id FROM auth_sessions \
          WHERE revoked_at >= now() - ($1 || ' seconds')::interval \
          LIMIT 50000",
    )
    .bind::<Text, _>(window_secs.to_string())
    .get_results(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}

/// Delete `auth_sessions` rows older than `days`.
///
/// This table is a permanent per-user record of where and on what device someone
/// signed in — a new PII class; `refresh_tokens` never had an `ip` column at
/// all. Nothing writes `revoked_at` when a session merely *expires*, so
/// `WHERE revoked_at IS NULL` excludes only explicitly-revoked sessions and
/// every abandoned session would otherwise stay indexed forever.
///
/// Deleting a row sets its tokens' `session_id` to NULL rather than deleting
/// them — that is exactly why the FK is `ON DELETE SET NULL`, and it is what
/// keeps replay detection intact through a reap.
pub async fn prune_auth_sessions(conn: &mut AsyncPgConnection, days: i64) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM auth_sessions \
          WHERE (revoked_at IS NOT NULL AND revoked_at < now() - ($1 || ' days')::interval) \
             OR expires_at < now() - ($1 || ' days')::interval",
    )
    .bind::<Text, _>(days.to_string())
    .execute(conn)
    .await
}
```

- [ ] **Step 5: Widen `refresh_token_revocation` and extend the neighbouring doc comment.** Replace the existing `refresh_token_revocation` with:

```rust
/// Revocation metadata for a token hash, whatever its state.
///
/// Returns `(user_id, session_id, revoked_at, revoked_reason)`. The handler
/// needs all four: the first three to tell a benign concurrent refresh from a
/// genuine replay, and `session_id` because the race path holds only the hash
/// and must continue the *same* session rather than starting a new one.
pub async fn refresh_token_revocation(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<(Uuid, Option<Uuid>, Option<DateTime<Utc>>, Option<String>)>> {
    refresh_tokens::table
        .filter(refresh_tokens::token_hash.eq(token_hash))
        .select((
            refresh_tokens::user_id,
            refresh_tokens::session_id,
            refresh_tokens::revoked_at,
            refresh_tokens::revoked_reason,
        ))
        .first(conn)
        .await
        .optional()
}
```

Then replace `user_has_active_refresh_token`'s doc comment with:

```rust
/// Whether the user still holds any usable refresh token.
///
/// After a family kill there are none, which is what stops the grace window from
/// resurrecting a session that was just revoked for replay.
///
/// That rationale stops holding the moment one session can be revoked while
/// others live: with a second session alive this returns true even though the
/// session being resurrected is dead. The real guard is now
/// `WHERE auth_sessions.revoked_at IS NULL` inside
/// [`start_or_continue_session`]; this check is a cheap first filter, not the
/// thing standing between a revoked session and a fresh token.
```

- [ ] **Step 6: Run and see it pass.** Run `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test sessions`. Expected: `10 passed`.

- [ ] **Step 7: Fix the one call site the widened tuple breaks, then check the workspace.** `backend/bins/sauron-api/src/routes/auth.rs` destructures the old 3-tuple at `if let Some((user_id, revoked_at, reason)) = repo::refresh_token_revocation(...)`. Change that pattern to `if let Some((user_id, session_id, revoked_at, reason))` and add `let _ = session_id;` on the next line as a placeholder — Task 8 Step 6 replaces it with the real use. Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Expected: clean.

- [ ] **Step 8: Format and lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Expected: clean.

---

### Task 8: `SessionContext`, the sanitizers, and a session-aware `issue_tokens`

**Files:**
- Modify `backend/bins/sauron-api/src/routes/mod.rs` (`issue_tokens` at lines 96-116)
- Modify `backend/bins/sauron-api/src/routes/auth.rs` (call sites at lines 243, 313, 383, 434, 545; `change_password` signature at line 468)
- Modify `backend/crates/sauron-db/src/repo.rs` (delete `insert_refresh_token`)
- Modify `backend/crates/sauron-db/src/models.rs` (delete `NewRefreshToken`)

**Interfaces:**
- Consumes: `repo::start_or_continue_session` (Task 6), `JwtKeys::issue_access(.., Option<Uuid>)` (Task 4), the widened `repo::refresh_token_revocation` (Task 7).
- Produces:

```rust
pub(crate) struct SessionContext { pub session_id: Option<Uuid>, pub user_agent: Option<String>, pub ip: Option<String> }
pub(crate) async fn issue_tokens(state: &AppState, conn: &mut AsyncPgConnection, user_id: Uuid, sess: SessionContext, must_change_password: bool) -> Result<TokenPair, ApiError>
pub(crate) const MAX_USER_AGENT_LEN: usize = 400;
pub(crate) fn sanitize_ua(headers: &axum::http::HeaderMap) -> Option<String>
pub(crate) fn sanitize_ip(raw: &str) -> Option<String>
pub(crate) fn client_addr(headers: &axum::http::HeaderMap, peer: &SocketAddr, state: &AppState) -> String  // visibility widened, unmoved
pub(crate) async fn rate_limit(state: &AppState, key: &str, limit: u32, window: u64) -> Result<(), ApiError>  // visibility widened, unmoved
```

- [ ] **Step 1: Write the failing sanitizer tests.** At the end of `backend/bins/sauron-api/src/routes/mod.rs`, append:

```rust
#[cfg(test)]
mod sanitize_tests {
    use super::*;
    use axum::http::header::USER_AGENT;
    use axum::http::HeaderMap;

    #[test]
    fn sanitize_ua_trims_rejects_empty_and_truncates() {
        let mut headers = HeaderMap::new();
        assert_eq!(sanitize_ua(&headers), None);

        headers.insert(USER_AGENT, "   ".parse().unwrap());
        assert_eq!(sanitize_ua(&headers), None);

        headers.insert(USER_AGENT, "  Mozilla/5.0  ".parse().unwrap());
        assert_eq!(sanitize_ua(&headers).as_deref(), Some("Mozilla/5.0"));

        let long = "a".repeat(MAX_USER_AGENT_LEN + 50);
        headers.insert(USER_AGENT, long.parse().unwrap());
        assert_eq!(sanitize_ua(&headers).map(|s| s.len()), Some(MAX_USER_AGENT_LEN));
    }

    #[test]
    fn sanitize_ip_stores_only_a_canonical_address() {
        // Not cosmetic: with API_TRUST_FORWARDED_HEADERS=1 this value comes from
        // a client-controlled X-Forwarded-For, so parsing it removes an
        // arbitrary-string-into-the-database vector.
        assert_eq!(sanitize_ip("203.0.113.7").as_deref(), Some("203.0.113.7"));
        assert_eq!(sanitize_ip("2001:0db8:0000:0000:0000:0000:0000:0001").as_deref(), Some("2001:db8::1"));
        assert_eq!(sanitize_ip("not-an-ip"), None);
        assert_eq!(sanitize_ip(""), None);
        assert_eq!(sanitize_ip("203.0.113.7, 198.51.100.1"), None);
    }
}
```

- [ ] **Step 2: Run and see it fail.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api sanitize_tests`. Expected failure: `error[E0425]: cannot find function 'sanitize_ua' in this scope`.

- [ ] **Step 3: Rewrite `issue_tokens` and add the sanitizers.** In `backend/bins/sauron-api/src/routes/mod.rs`, replace the whole `issue_tokens` function (lines 96-116) with:

```rust
/// Everything a token mint needs to know about *where* it is happening.
///
/// A struct rather than three more positional parameters: seven arguments sits
/// on `clippy::too_many_arguments`' threshold, and
/// `Option<Uuid>, Option<String>, Option<String>` in a row is a call-site
/// transposition waiting to happen.
pub(crate) struct SessionContext {
    /// `None` starts a new session; `Some` continues one across a rotation.
    pub session_id: Option<Uuid>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

/// Longest user agent stored. Long enough for every real browser string, short
/// enough that a caller cannot use the header as free storage.
pub(crate) const MAX_USER_AGENT_LEN: usize = 400;

/// The request's `User-Agent`, trimmed, non-empty, and bounded.
///
/// Truncation is by `chars`, not bytes: slicing a UTF-8 string mid-codepoint
/// panics, and the header is caller-controlled.
pub(crate) fn sanitize_ua(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())?
        .trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.chars().take(MAX_USER_AGENT_LEN).collect())
}

/// A caller address, canonicalised, or `None` if it is not an address at all.
///
/// With `API_TRUST_FORWARDED_HEADERS=1` this value comes from a
/// client-controlled `X-Forwarded-For`, so parsing it as an `IpAddr` and storing
/// the canonical form (else NULL) removes an arbitrary-string-into-the-database
/// vector. It is also what makes the stored value safe to render unmasked on the
/// owner's own account page.
pub(crate) fn sanitize_ip(raw: &str) -> Option<String> {
    raw.parse::<std::net::IpAddr>().ok().map(|a| a.to_string())
}

/// Issue an access token and a persisted (rotating) refresh token for a user,
/// starting or continuing the caller's session.
///
/// The order is deliberate and **changed** from the original: the session row is
/// written first and the JWT is minted last, so a token is never handed out for
/// a session that failed to persist. The session id is generated in Rust rather
/// than by the database default because the JWT needs it, and a `RETURNING`
/// round trip would not be atomic with the token insert.
pub(crate) async fn issue_tokens(
    state: &AppState,
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    sess: SessionContext,
    must_change_password: bool,
) -> Result<TokenPair, ApiError> {
    let continuing = sess.session_id.is_some();
    let session_id = sess.session_id.unwrap_or_else(Uuid::new_v4);
    let raw = sauron_core::ids::opaque_token();
    let hash = sauron_auth::hash_token(&raw);
    let expires_at = Utc::now() + Duration::seconds(state.cfg.jwt_refresh_ttl_secs);

    let rows = sauron_db::repo::start_or_continue_session(
        conn,
        session_id,
        user_id,
        hash,
        expires_at,
        sess.user_agent,
        sess.ip,
    )
    .await?;
    if rows == 0 {
        // Continuing: the session was revoked (or re-owned) between the caller
        // presenting its refresh token and this write. That is a 401, and it is
        // the guard that stops the 10-second refresh-race window from
        // resurrecting a session the user just killed.
        // Starting: a fresh INSERT with a brand-new uuid cannot conflict, so
        // zero rows means something is genuinely wrong.
        return Err(if continuing {
            ApiError::Auth(sauron_auth::AuthError::InvalidToken)
        } else {
            ApiError::Internal("new session insert affected no rows".into())
        });
    }

    let (access, exp) = state
        .keys
        .issue_access(user_id, must_change_password, Some(session_id))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(TokenPair {
        access_token: access,
        refresh_token: raw,
        expires_at: exp,
    })
}
```

- [ ] **Step 4: Widen `rate_limit` and `client_addr` in place, and document the key convention.** In `backend/bins/sauron-api/src/routes/auth.rs`, change `fn client_addr(` to `pub(crate) fn client_addr(` and `async fn rate_limit(` to `pub(crate) async fn rate_limit(`. They stay in `auth.rs` — do not move them to a new shared module; later slices call them from `routes/orgs.rs` and elsewhere, and a move would land three slices on the same refactor. Append to `rate_limit`'s existing doc comment:

```rust
/// Limiter keys follow `sauron:{area}:{action}:{principal}` — e.g.
/// `sauron:auth:login:ada@example.com`, `sauron:auth:sessions:{user_id}`. The
/// principal is the thing being protected *from*: a user id for authenticated
/// routes, `client_addr(..)` for anonymous ones. Pick one and keep it stable;
/// changing a key silently resets everyone's budget.
```

- [ ] **Step 5: Update `register` and `login`.** In `backend/bins/sauron-api/src/routes/auth.rs`, change the import line `use super::{db, issue_tokens, slugify, TokenPair};` to `use super::{db, issue_tokens, sanitize_ip, sanitize_ua, slugify, SessionContext, TokenPair};`. In `register`, replace `issue_tokens(&state, &mut conn, user.id, None, false).await?` with:

```rust
    let tokens = issue_tokens(
        &state,
        &mut conn,
        user.id,
        SessionContext {
            // A new session per login.
            session_id: None,
            user_agent: sanitize_ua(&headers),
            ip: sanitize_ip(&client_addr(&headers, &peer, &state)),
        },
        false,
    )
    .await?;
```

In `login`, replace `issue_tokens(&state, &mut conn, user.id, None, user.must_change_password).await?` with the same block but `user.must_change_password` as the last argument.

- [ ] **Step 6: Update the two `refresh` call sites.** In `refresh`, the destructuring from Task 7 Step 7 becomes load-bearing. Replace `let _ = session_id;` and the `raced` block's `issue_tokens` call with:

```rust
            if raced {
                tracing::info!(
                    %user_id,
                    "concurrent refresh of a just-rotated token; re-issuing instead of \
                     revoking the family"
                );
                // Load the user for the forced-change flag, and refuse a
                // deactivated account here too: otherwise a member an admin just
                // disabled keeps minting fresh access tokens from the refresh
                // token still sitting in localStorage, and the deactivation
                // never takes effect.
                let user = repo::get_user(&mut conn, user_id)
                    .await?
                    .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
                if !user.is_active {
                    return Err(ApiError::Auth(AuthError::AccountDeactivated));
                }
                // A pre-migration token has no session to continue, and letting
                // it degrade to a fresh login defeats the whole guard: the
                // `WHERE revoked_at IS NULL` check inside
                // `start_or_continue_session` is only reachable when a session
                // id is present. The reachable case is a rolling upgrade — the
                // RPM ships api and dashboard as separate subpackages — where an
                // old replica mints a session-less token after 000035 has run,
                // rotates it at T, the user presses "sign out other devices" at
                // T+2s, and at T+5s that device's other tab lands in the grace
                // window and gets a brand-new live session. The kill would have
                // silently failed for that device, and the new session would
                // appear in the list looking like a legitimate login. Rejecting
                // cannot harm the legitimate pre-migration case: a row revoked
                // before the migration is by definition more than 10 seconds old
                // and never satisfies the grace condition.
                let Some(sid) = session_id else {
                    return Err(ApiError::Auth(AuthError::InvalidToken));
                };
                let tokens = issue_tokens(
                    &state,
                    &mut conn,
                    user_id,
                    SessionContext {
                        session_id: Some(sid),
                        user_agent: sanitize_ua(&headers),
                        ip: sanitize_ip(&client_addr(&headers, &peer, &state)),
                    },
                    user.must_change_password,
                )
                .await?;
                return Ok(Json(tokens));
            }
```

Then, on the rotate path at the bottom of `refresh`, replace the `issue_tokens(...)` call with:

```rust
    let tokens = issue_tokens(
        &state,
        &mut conn,
        token.user_id,
        SessionContext {
            session_id: token.session_id,
            user_agent: sanitize_ua(&headers),
            ip: sanitize_ip(&client_addr(&headers, &peer, &state)),
        },
        user.must_change_password,
    )
    .await?;
```

- [ ] **Step 7: Give `change_password` the two extractors it needs and update its call site.** Change the signature to:

```rust
pub async fn change_password(
    auth: AuthUser,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChangePasswordReq>,
) -> Result<Json<AuthResponse>, ApiError> {
```

`ConnectInfo` and `HeaderMap` are both `FromRequestParts`, so any order before the `Json` body extractor is valid. Without them the user's post-password-change session shows "Unknown device" in their own list — which is the exact row they will look at first. Then replace `issue_tokens(&state, &mut conn, auth.user_id, None, false).await?` with:

```rust
    let tokens = issue_tokens(
        &state,
        &mut conn,
        auth.user_id,
        SessionContext {
            session_id: None,
            user_agent: sanitize_ua(&headers),
            ip: sanitize_ip(&client_addr(&headers, &peer, &state)),
        },
        false,
    )
    .await?;
```

- [ ] **Step 8: Delete the session-blind mint path.** In `backend/crates/sauron-db/src/repo.rs`, delete the whole `pub async fn insert_refresh_token(...)` function. In `backend/crates/sauron-db/src/models.rs`, delete the whole `#[derive(Debug, Insertable)] #[diesel(table_name = refresh_tokens)] pub struct NewRefreshToken { ... }` block. These go **together**: leaving a second, session-blind mint path is how the two tables come to disagree.

- [ ] **Step 9: Check the workspace and run the sanitizer tests.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets` — expected clean. Then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api sanitize_tests` — expected `2 passed`.

- [ ] **Step 10: Format and lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Expected: clean.

---

### Task 9: `SessionRevocations` — the per-replica revocation snapshot

**Files:**
- Create `backend/crates/sauron-auth/src/revocations.rs`
- Modify `backend/crates/sauron-auth/src/lib.rs`

**Interfaces:**
- Consumes: `repo::revoked_session_ids` (Task 7), `sauron_db::PgPool`, `sauron_db::conn`.
- Produces: `sauron_auth::SessionRevocations` with `new()`, `contains(&self, sid: &Uuid) -> bool`, `mark_revoked(&self, ids: &[Uuid])`, `replace(&self, ids: HashSet<Uuid>, poll_started_at: Instant)`, `age(&self) -> Option<Duration>`, `async refresh(&self, pool: &sauron_db::PgPool, window_secs: i64) -> anyhow::Result<usize>`.

`age()` has **no consumer in this slice** — that is deliberate and stated in its doc comment. It exists for the email-foundation slice's task supervisor, whose named tasks carry a `last_success` that `/health` renders. Task 11's fallback supervisor (used only when that slice has not landed) tracks no `last_success`, and `main.rs:147` keeps `/health` as a static `"ok"`. If the poller stops refreshing before that supervisor exists, the only signal is the repeated `background task failed; retrying` ERROR line. Wiring a staleness field into `/health` is not in scope here; do not add it as a side quest.

- [ ] **Step 1: Create the module with its failing tests.** Create `backend/crates/sauron-auth/src/revocations.rs`:

```rust
//! Per-replica snapshot of revoked session ids.
//!
//! An access token stays valid until its own `exp` — `JWT_ACCESS_TTL_SECS`,
//! default 900s — so without this, nothing anyone revokes takes effect for up to
//! fifteen minutes: not a logout, not a deactivation, not a password change, not
//! a family kill. This closes that to one poll interval, default 5 seconds.
//!
//! **Any binary that wants the `AuthUser` extractor must now supply one of
//! these, and supplying a permanently-empty one compiles and silently disables
//! revocation for that service.** There is no way for the type system to catch
//! that; it is why this sentence is here rather than only in a design document.
//!
//! Rejected alternatives, so nobody re-proposes them:
//! - A Redis denylist. Fail-open silently disables the control; fail-closed 401s
//!   the whole API on a blip. The shared Redis connection is built with
//!   `set_response_timeout(None)` and is measured at 9-19s per call when Redis is
//!   dead, which is a stall on every authenticated request.
//! - A `users.tokens_valid_from` column. Cannot express per-session granularity,
//!   and adds a pool checkout in front of every handler on a 16-connection pool.
//! - Shortening `JWT_ACCESS_TTL_SECS`. Fifteen times the refresh traffic, and it
//!   still leaves a window.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use uuid::Uuid;

#[derive(Default)]
struct Snapshot {
    /// Ids the last successful poll returned.
    polled: HashSet<Uuid>,
    /// Ids this replica revoked itself, with the instant it did so. These cover
    /// the gap between a local revoke and the next poll that can see it.
    local: HashMap<Uuid, Instant>,
    refreshed_at: Option<Instant>,
}

/// A cloneable handle onto one process-wide revoked-session snapshot.
#[derive(Clone, Default)]
pub struct SessionRevocations {
    inner: Arc<RwLock<Snapshot>>,
}

impl SessionRevocations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Is this session revoked? Pure memory read — no I/O, ever.
    ///
    /// Runs inside `AuthUser::from_request_parts`, i.e. on every authenticated
    /// request in every route file. The poisoned-lock recovery matches
    /// `local_rate_limit_ok` in `routes/auth.rs` and exists for the same reason:
    /// a naive `.unwrap()` would turn one transient panic under the write guard
    /// into a total API outage.
    pub fn contains(&self, sid: &Uuid) -> bool {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        guard.polled.contains(sid) || guard.local.contains_key(sid)
    }

    /// Record ids this replica just revoked, so the kill takes effect here
    /// immediately rather than at the next poll.
    pub fn mark_revoked(&self, ids: &[Uuid]) {
        if ids.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut guard = self.inner.write().unwrap_or_else(|p| p.into_inner());
        for id in ids {
            guard.local.insert(*id, now);
        }
    }

    /// Swap in a fresh polled set and evict the local entries it has superseded.
    ///
    /// **The eviction rule is the subtle part.** Expressing retention as
    /// wall-clock age against the poll interval is wrong: a locally-marked id is
    /// only certain to be in a poll's result if that poll's *query started
    /// after* the mark. A poll that begins at T-1s, a revocation at T, and a slow
    /// finish at T+6s would evict a 5-second-old local entry using a snapshot
    /// that never contained it — and the revoked session's access token would be
    /// honoured again on this replica until the next poll. A security control
    /// silently ceasing to hold, on exactly the axis this module exists to
    /// establish. So the caller records `Instant::now()` *before* issuing the
    /// query and hands it here.
    ///
    /// The old set is dropped outside the guard, so freeing a large allocation
    /// does not block request tasks parked on `contains`.
    pub fn replace(&self, ids: HashSet<Uuid>, poll_started_at: Instant) {
        let old = {
            let mut guard = self.inner.write().unwrap_or_else(|p| p.into_inner());
            let old = std::mem::replace(&mut guard.polled, ids);
            guard.local.retain(|_, marked| *marked >= poll_started_at);
            guard.refreshed_at = Some(Instant::now());
            old
        };
        drop(old);
    }

    /// Time since the last **successful** poll; `None` before the first one.
    ///
    /// A failed poll deliberately leaves this stale — the age is the signal that
    /// the control has stopped refreshing.
    ///
    /// **Nothing in this slice reads it.** It exists for the email-foundation
    /// slice's task supervisor, which renders a per-task `last_success` age on
    /// `/health`; the fallback supervisor in Task 11 Step 2 tracks no such thing,
    /// and `/health` stays the static `"ok"` it is today. Until that lands, a
    /// poller that has silently stopped refreshing is visible only as repeated
    /// `background task failed; retrying` lines in the log. Do not delete this as
    /// dead code — the pin below is what keeps it compiling and correct.
    pub fn age(&self) -> Option<Duration> {
        let guard = self.inner.read().unwrap_or_else(|p| p.into_inner());
        guard.refreshed_at.map(|at| at.elapsed())
    }

    /// One poll. Returns the number of ids in the new snapshot.
    ///
    /// Checks out a pooled connection, runs the one query, and **drops the
    /// connection before** swapping the snapshot: the API pool is 16 for the
    /// whole process and a background task must never hold a slot across work it
    /// does not need one for.
    pub async fn refresh(
        &self,
        pool: &sauron_db::PgPool,
        window_secs: i64,
    ) -> anyhow::Result<usize> {
        /// Must match the `LIMIT` in `repo::revoked_session_ids`.
        const POLL_LIMIT: usize = 50_000;

        // Recorded before the query is issued — see `replace`.
        let started_at = Instant::now();
        let ids = {
            let mut conn = sauron_db::conn(pool).await?;
            sauron_db::repo::revoked_session_ids(&mut conn, window_secs).await?
        };
        let count = ids.len();
        if count >= POLL_LIMIT {
            // A silently truncated snapshot is a security control that has
            // stopped working while reporting healthy.
            tracing::error!(
                count,
                "revocation snapshot hit its row limit; sessions revoked beyond it keep working \
                 until their access tokens expire"
            );
        }
        self.replace(ids.into_iter().collect(), started_at);
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locally_marked_session_is_revoked_immediately() {
        let revs = SessionRevocations::new();
        let sid = Uuid::new_v4();
        assert!(!revs.contains(&sid));
        revs.mark_revoked(&[sid]);
        assert!(revs.contains(&sid));
    }

    #[test]
    fn replace_evicts_only_marks_older_than_the_polls_start() {
        let revs = SessionRevocations::new();
        let before = Uuid::new_v4();
        revs.mark_revoked(&[before]);

        // The poll's query starts here — after `before` was marked, so a
        // snapshot taken now legitimately supersedes it.
        std::thread::sleep(Duration::from_millis(2));
        let poll_started_at = Instant::now();
        std::thread::sleep(Duration::from_millis(2));

        let after = Uuid::new_v4();
        revs.mark_revoked(&[after]);

        revs.replace(HashSet::new(), poll_started_at);

        assert!(
            !revs.contains(&before),
            "a mark the poll could see is superseded by the poll's result"
        );
        assert!(
            revs.contains(&after),
            "a mark made after the poll started was never in its result and must survive; \
             evicting it un-revokes a killed session until the next poll"
        );
    }

    #[test]
    fn a_polled_id_is_revoked_and_a_later_poll_can_clear_it() {
        let revs = SessionRevocations::new();
        let sid = Uuid::new_v4();
        revs.replace(HashSet::from([sid]), Instant::now());
        assert!(revs.contains(&sid));
        // Once the id ages out of the poll window its access tokens have expired
        // on their own `exp`, so dropping it is correct.
        revs.replace(HashSet::new(), Instant::now());
        assert!(!revs.contains(&sid));
    }

    #[test]
    fn age_is_none_before_the_first_poll_and_some_after() {
        let revs = SessionRevocations::new();
        assert!(revs.age().is_none());
        revs.replace(HashSet::new(), Instant::now());
        assert!(revs.age().is_some());
    }

    #[test]
    fn a_failed_poll_leaves_the_last_good_snapshot_intact() {
        // `refresh` only calls `replace` on the success path, so a failure is
        // expressed here as "no replace happened". The snapshot must not be
        // cleared, or a Postgres blip would re-enable every revoked session.
        let revs = SessionRevocations::new();
        let polled = Uuid::new_v4();
        let local = Uuid::new_v4();
        revs.replace(HashSet::from([polled]), Instant::now());
        revs.mark_revoked(&[local]);

        assert!(revs.contains(&polled));
        assert!(revs.contains(&local));
    }
}
```

- [ ] **Step 2: Export it.** In `backend/crates/sauron-auth/src/lib.rs`, add `pub mod revocations;` after `pub mod rbac;`, and add `pub use revocations::SessionRevocations;` after the `pub use jwt::{...};` line.

- [ ] **Step 3: Run the tests and see them pass.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-auth revocations`. Expected: `5 passed`.

- [ ] **Step 4: Format and lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Expected: clean. If clippy flags `drop_non_drop` on `drop(old)`, that is a signal the type changed — do not silence it, re-read `replace`.

---

### Task 10: Wire the snapshot into `AppState` and enforce it in the extractor

**Files:**
- Modify `backend/crates/sauron-core/src/config.rs` (struct at ~line 10, constructor at ~line 171)
- Modify `backend/crates/sauron-auth/src/extractors.rs` (impl bound at line 121-125, body at ~line 142)
- Modify `backend/bins/sauron-api/src/main.rs` (`AppState` at line 43, `FromRef` at line 57, state construction at line 108)

**Interfaces:**
- Consumes: `SessionRevocations` (Task 9), `Claims.sid` (Task 4).
- Produces: `Config::auth_revocation_poll_secs: u64`; `AppState::revocations: sauron_auth::SessionRevocations`; `impl FromRef<AppState> for SessionRevocations`; the extractor bound `SessionRevocations: FromRef<S>`.

- [ ] **Step 1: Add the config field.** In `backend/crates/sauron-core/src/config.rs`, add to the `Config` struct directly after `pub jwt_refresh_ttl_secs: i64,`:

```rust
    /// How often each API replica refreshes its revoked-session snapshot, in
    /// seconds. **This is the real kill latency** — a revoked session's access
    /// token keeps working on a replica until that replica's next poll.
    ///
    /// Clamped at the use site to 1..=60, so a fat-fingered `0` cannot spin the
    /// poller and a `3600` cannot silently restore a one-hour revocation window.
    pub auth_revocation_poll_secs: u64,
```

and to the `Ok(Self { ... })` literal directly after `jwt_refresh_ttl_secs: parse("JWT_REFRESH_TTL_SECS", 2_592_000),`:

```rust
            auth_revocation_poll_secs: parse("AUTH_REVOCATION_POLL_SECS", 5),
```

- [ ] **Step 2: Change the extractor bound and add the check.** In `backend/crates/sauron-auth/src/extractors.rs`, add `use crate::revocations::SessionRevocations;` beside the existing `use crate::jwt::{Claims, JwtKeys};`. Change the impl header to:

```rust
impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    JwtKeys: FromRef<S>,
    SessionRevocations: FromRef<S>,
{
```

Inside `from_request_parts`, between `let user_id = Uuid::parse_str(&claims.sub)...;` and the `password_change_gate(...)` call, insert:

```rust
        // Before the password gate, not after: a revoked session must 401 on
        // EVERY path including `/v1/auth/password`, or a revoked temp-password
        // holder could still change the password. `AuthError::InvalidToken` is
        // the right code and needs no dashboard change — the 401 interceptor
        // calls `runRefreshOnce()`, whose refresh row is also revoked, so
        // `/v1/auth/refresh` 401s and `onRefreshFailure()` sends the user to
        // `#/login`.
        //
        // A token with no `sid` predates migration 000035 and is accepted
        // unchanged; that cannot last more than `JWT_ACCESS_TTL_SECS` past the
        // deploy, because `validate_exp` is on and every login and refresh mints
        // one. Rejecting them instead would sign out every logged-in user at
        // deploy.
        if let Some(sid) = claims.sid {
            if SessionRevocations::from_ref(state).contains(&sid) {
                return Err(AuthError::InvalidToken);
            }
        }
```

Leave `password_change_allowed_path` and its pinned test `password_change_allowlist_is_exactly_two_paths` **unchanged** — no new path joins that allowlist.

- [ ] **Step 3: Run and see the whole workspace fail to compile.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Expected failure: `error[E0277]: the trait bound 'SessionRevocations: FromRef<AppState>' is not satisfied` reported at every handler in `sauron-api` that takes `AuthUser`.

- [ ] **Step 4: Give `AppState` the field and the `FromRef` impl.** In `backend/bins/sauron-api/src/main.rs`, add to the `AppState` struct after `pub alerts: sauron_alerts::AlertEngine,`:

```rust
    /// Revoked sessions this replica knows about. Read by the `AuthUser`
    /// extractor on every authenticated request; refreshed by the
    /// `revocation-poll` background task.
    pub revocations: sauron_auth::SessionRevocations,
```

Directly after the existing `impl FromRef<AppState> for JwtKeys` block, add:

```rust
impl FromRef<AppState> for sauron_auth::SessionRevocations {
    fn from_ref(state: &AppState) -> sauron_auth::SessionRevocations {
        state.revocations.clone()
    }
}
```

In the `let state = AppState { ... };` literal, add `revocations: sauron_auth::SessionRevocations::new(),` after `alerts,`.

- [ ] **Step 5: See it compile.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Expected: clean. No handler signature changed — `AuthUser` is only ever used with `sauron-api`'s `AppState`, so this is two files.

- [ ] **Step 6: Run the auth suite.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-auth`. Expected green, including `password_change_allowlist_is_exactly_two_paths`.

- [ ] **Step 7: Format and lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Expected: clean.

---

### Task 11: The background-task supervisor, the revocation poller and the session reaper

**Files:**
- Create *or* extend `backend/bins/sauron-api/src/tasks.rs`
- Modify `backend/bins/sauron-api/src/main.rs` (`mod` list at line 7-11; after `let state = AppState { .. };`)

**Interfaces:**
- Consumes: `SessionRevocations::refresh` (Task 9), `repo::prune_auth_sessions` + `repo::AUTH_SESSION_RETENTION_DAYS` (Task 7), `Config::auth_revocation_poll_secs` (Task 10).
- Produces: `tasks::spawn_named(name: &'static str, interval: Duration, body: F)`; two running loops named `revocation-poll` and `auth-session-reaper`.

- [ ] **Step 1: Check whether the supervisor already exists.** Run `ls /home/splimter/projects/freelance/sauron/backend/bins/sauron-api/src/tasks.rs`. **If it exists** (the email-foundation slice landed first and built it), skip Step 2 and Step 3; instead read the file, find its "register a named task" entry point, and use it in Step 4 in place of `tasks::spawn_named`, keeping the two task bodies and the window arithmetic exactly as written. **If it does not exist**, continue with Step 2.

- [ ] **Step 2: Create the supervisor.** Create `backend/bins/sauron-api/src/tasks.rs`:

```rust
//! Supervised background loops for `sauron-api`.
//!
//! `main()` had no spawned loops before this and exactly one DB touch at boot.
//! One runner, so the next slice that wants a timer does not mint a second
//! pattern.
//!
//! **No task's initialization may `?` out of `main()`.** The blast radius is
//! exact: `packaging/rpm/systemd/sauron-migrate.service` has no `[Install]`
//! section (run on demand only), while `sauron.spec`'s `%postun server` runs
//! `%systemd_postun_with_restart sauron-api.service` — so `dnf upgrade` restarts
//! the new binary against the old schema every time. A `?` on
//! `relation "auth_sessions" does not exist` propagates out of `main`,
//! `sauron-api.service` is `Restart=on-failure` with `RestartSec=2` and no
//! `StartLimit*` override, systemd's default burst is exhausted in ~10 seconds,
//! and the unit lands in `failed` and stays there. The operator loses `/health`,
//! every read route and the whole dashboard backend — the exact surface they
//! would use to diagnose it. So: log at ERROR, back off, keep going.

use std::future::Future;
use std::time::Duration;

/// Longest gap between retries after a failing iteration.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Spawn a named loop that runs `body` every `interval`, restarting it on error
/// **or panic** with capped exponential backoff.
///
/// Each iteration runs inside its own `tokio::spawn` so a panic surfaces as a
/// `JoinError` here rather than silently killing the loop. Without that, one
/// unwrap in a task body permanently disables it and nothing says so.
pub fn spawn_named<F, Fut>(name: &'static str, interval: Duration, body: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        let mut backoff = interval;
        loop {
            match tokio::spawn(body()).await {
                Ok(Ok(())) => {
                    backoff = interval;
                    tokio::time::sleep(interval).await;
                }
                Ok(Err(e)) => {
                    tracing::error!(task = name, error = %e, "background task failed; retrying");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
                Err(join) => {
                    tracing::error!(task = name, error = %join, "background task panicked; respawning");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    });
}
```

- [ ] **Step 3: Declare the module.** In `backend/bins/sauron-api/src/main.rs`, add `mod tasks;` to the module list, in alphabetical position after `mod routes;`.

- [ ] **Step 4: Mount the two loops.** In `backend/bins/sauron-api/src/main.rs`, directly after the `let state = AppState { ... };` literal, insert:

```rust
    // Floored at 900 on purpose. The correctness argument — "a token minted
    // before a revocation older than the access TTL has already expired on its
    // own exp" — only holds if the TTL never DECREASES. An operator hardening
    // 900 -> 120 and restarting leaves pre-restart tokens alive for 900s against
    // a 240s window: ~11 minutes of accepted-but-revoked access, with no error
    // and no log. Clamped above because JWT_ACCESS_TTL_SECS is an unvalidated
    // i64 from the environment; `parse()` has no floor, no ceiling and no sign
    // check, and a negative value cast to u64 wraps to ~1.8e19.
    let revocation_window_secs = state.cfg.jwt_access_ttl_secs.clamp(900, 86_400) + 120;
    let revocation_poll = Duration::from_secs(state.cfg.auth_revocation_poll_secs.clamp(1, 60));

    // Deliberately NOT preceded by a synchronous `revocations.refresh(..).await?`
    // before the listener binds — see tasks.rs. The snapshot starts empty and the
    // supervisor retries; one poll interval of stale revocation data on a cold
    // start is strictly smaller than the 900-second window that exists today.
    {
        let revocations = state.revocations.clone();
        let pool = state.pool.clone();
        tasks::spawn_named("revocation-poll", revocation_poll, move || {
            let revocations = revocations.clone();
            let pool = pool.clone();
            async move {
                revocations.refresh(&pool, revocation_window_secs).await?;
                Ok(())
            }
        });
    }

    {
        // `auth_sessions` is a permanent per-user record of where and on what
        // device someone signed in, and its partial index is proportional to
        // lifetime logins, not to live sessions — nothing writes `revoked_at`
        // when a session merely expires. The reaper lives here because the rule
        // is that a table's reaper runs in the process that owns its write path.
        let pool = state.pool.clone();
        tasks::spawn_named(
            "auth-session-reaper",
            Duration::from_secs(86_400),
            move || {
                let pool = pool.clone();
                async move {
                    let mut conn = sauron_db::conn(&pool).await?;
                    let deleted = sauron_db::repo::prune_auth_sessions(
                        &mut conn,
                        sauron_db::repo::AUTH_SESSION_RETENTION_DAYS,
                    )
                    .await?;
                    // The API pool is 16 for the whole process; never hold a slot
                    // across work that does not need one.
                    drop(conn);
                    if deleted > 0 {
                        tracing::info!(deleted, "pruned expired and long-revoked auth_sessions");
                    }
                    Ok(())
                }
            },
        );
    }
```

- [ ] **Step 5: Check and lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets` — expected clean. Then `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` and `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings` — expected clean.

- [ ] **Step 6: Prove boot survives a missing table.** Point the binary at a database that has *not* had 000035 applied and confirm it still serves `/health` instead of dying:

```
cd /home/splimter/projects/freelance/sauron/backend && \
  psql postgres://sauron:sauron@172.20.0.2:5432/postgres -c 'CREATE DATABASE sauron_boot_probe;' && \
  DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu \
  DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron_boot_probe \
  REDIS_URL=redis://172.20.0.3:6379 \
  JWT_SECRET=boot-probe-secret-0000000000000000000000 \
  API_PORT=8099 CORS_ALLOWED_ORIGINS=http://localhost:3000 RUST_LOG=error \
  cargo run --bin sauron-api
```

The database is empty, so `ensure_preset_roles` will fail at boot for a reason unrelated to this task; if it does, run `DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron_boot_probe cargo run --bin sauron-migrate` first, then **delete only the 000035 row** with `psql postgres://sauron:sauron@172.20.0.2:5432/sauron_boot_probe -c "DROP TABLE auth_sessions CASCADE; DELETE FROM __diesel_schema_migrations WHERE version='20260801000035';"` and re-run. Expected: the process stays up, `curl -s localhost:8099/health` returns `ok`, and stderr repeats `background task failed; retrying` naming `revocation-poll`. Then stop it and `psql postgres://sauron:sauron@172.20.0.2:5432/postgres -c 'DROP DATABASE sauron_boot_probe WITH (FORCE);'`.

---

### Task 12: `routes/account.rs` — list, revoke one, revoke others

**Files:**
- Create `backend/bins/sauron-api/src/routes/account.rs`
- Modify `backend/bins/sauron-api/src/routes/mod.rs` (module list at the top)
- Modify `backend/bins/sauron-api/Cargo.toml` (dependencies)
- Modify `backend/bins/sauron-api/src/main.rs` (router, after the `/v1/me` line)

**Interfaces:**
- Consumes: `repo::list_sessions`, `repo::revoke_session`, `repo::revoke_sessions_for_user`, `repo::MAX_SESSIONS_LISTED` (Task 7); `repo::REVOKE_USER_REVOKED`, `repo::REVOKE_USER_REVOKED_OTHERS` (Task 5); `auth::rate_limit`, `super::db` (Task 8); `state.revocations` (Task 10).
- Produces: `routes::account::list_sessions`, `routes::account::revoke_session`, `routes::account::revoke_other_sessions`, and the JSON shape `SessionView { id, created_at, last_used_at, expires_at, current, user_agent, browser, os, device_kind, ip, revoked_at, revoked_reason }`.

- [ ] **Step 1: Declare the woothee dependency.** In `backend/bins/sauron-api/Cargo.toml`, add `woothee.workspace = true` to `[dependencies]`, directly after `uuid.workspace = true`. It is already a workspace dependency at `backend/Cargo.toml:119` (declared only by `sauron-pipeline`), so this is a declaration, not a new third-party dependency, and the RPM's vendored-crate story is unchanged. Using it here rather than a second UA parser is deliberate: `sauron_pipeline::enrich::enrich_context` parses every ingested UA with woothee, and a second vocabulary would give two different names for the same browser in two places in the same UI.

- [ ] **Step 2: Create the module with its `parse_ua` tests.** Create `backend/bins/sauron-api/src/routes/account.rs`:

```rust
//! The caller's own account: which sessions exist, and how to end them.
//!
//! Everything here is gated on `AuthUser` and nothing else — a user's own
//! sessions are the definition of "any authenticated user", the same class as
//! `/v1/me`. The admin surface deliberately returns no session data at all.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{AuthError, AuthUser};
use sauron_db::models::AuthSession;
use sauron_db::repo;

use super::auth::rate_limit;
use super::db;
use crate::error::ApiError;
use crate::AppState;

/// Session actions allowed per user per window. Generous enough for a user
/// working through a list of devices, tight enough that the endpoint is not a
/// free write loop.
const SESSION_ACTIONS_PER_MIN: u32 = 20;

/// One row of the caller's session list.
///
/// Hand-built, never the `AuthSession` model: `revoked_by` is **never**
/// serialized. Surfacing it would tell a member which admin signed them out.
/// Surfacing `revoked_at` and `revoked_reason` is what makes the destructive
/// action observable to the person it happened to, which is the whole point of
/// writing those columns.
#[derive(Debug, Serialize)]
pub struct SessionView {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Marked server-side so the dashboard never has to decode a JWT — it has no
    /// decoder dependency and should not gain one.
    pub current: bool,
    pub user_agent: Option<String>,
    pub browser: Option<String>,
    pub os: Option<String>,
    pub device_kind: Option<String>,
    /// Returned **unmasked**, not through the telemetry IP masker. This is the
    /// caller's own data, and `192.168.x.x` defeats the entire "was that login
    /// me?" purpose.
    pub ip: Option<String>,
    /// Present only when `?include_revoked=1`; NULL for live rows.
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    /// Accepts `1` or `true`. Deliberately a `String` and not an `Option<bool>`:
    /// axum's `Query` deserializes through `serde_urlencoded`, which rejects `1`
    /// for a bool and 400s the whole request — and `?include_revoked=1` is the
    /// form the docs use.
    #[serde(default)]
    pub include_revoked: Option<String>,
}

fn truthy(v: Option<&String>) -> bool {
    matches!(v.map(String::as_str), Some("1") | Some("true"))
}

/// woothee returns the literal string `"UNKNOWN"` (and sometimes `""`) for
/// fields it cannot determine, so without this every unrecognised UA renders as
/// "UNKNOWN on UNKNOWN".
fn norm(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() || t == "UNKNOWN" {
        None
    } else {
        Some(t.to_string())
    }
}

/// `(browser, os, device_kind)` for a user-agent string.
///
/// Pure, so it is unit tested here. Instantiating the parser per call is fine at
/// <=200 rows on a rarely-hit endpoint; do not add a `OnceLock` for it.
pub fn parse_ua(ua: Option<&str>) -> (Option<String>, Option<String>, Option<String>) {
    let Some(ua) = ua else {
        return (None, None, None);
    };
    let parser = woothee::parser::Parser::new();
    match parser.parse(ua) {
        Some(r) => (norm(r.name), norm(r.os), norm(r.category)),
        None => (None, None, None),
    }
}

fn to_view(row: AuthSession, current_sid: Option<Uuid>) -> SessionView {
    let (browser, os, device_kind) = parse_ua(row.user_agent.as_deref());
    SessionView {
        id: row.id,
        created_at: row.created_at,
        last_used_at: row.last_used_at,
        expires_at: row.expires_at,
        current: current_sid == Some(row.id),
        user_agent: row.user_agent,
        browser,
        os,
        device_kind,
        ip: row.ip,
        revoked_at: row.revoked_at,
        revoked_reason: row.revoked_reason,
    }
}

/// `GET /v1/me/sessions`
pub async fn list_sessions(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<ListSessionsQuery>,
) -> Result<Json<Vec<SessionView>>, ApiError> {
    let include_revoked = truthy(q.include_revoked.as_ref());
    let mut conn = db(&state).await?;
    let rows = repo::list_sessions(&mut conn, auth.user_id, include_revoked).await?;
    drop(conn);
    let sid = auth.claims.sid;
    Ok(Json(rows.into_iter().map(|r| to_view(r, sid)).collect()))
}

/// `DELETE /v1/me/sessions/{session_id}`
pub async fn revoke_session(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    rate_limit(
        &state,
        &format!("sauron:auth:sessions:{}", auth.user_id),
        SESSION_ACTIONS_PER_MIN,
        60,
    )
    .await?;

    // Refused rather than treated as a logout. Defensible either way, but an
    // identical-looking button in an identical-looking row would do something
    // categorically different, and Log out already exists in the Topbar.
    if auth.claims.sid == Some(session_id) {
        return Err(ApiError::Conflict(
            "cannot revoke the session you are using — use Log out instead".into(),
        ));
    }

    let mut conn = db(&state).await?;
    let ids = repo::revoke_session(
        &mut conn,
        session_id,
        auth.user_id,
        repo::REVOKE_USER_REVOKED,
        Some(auth.user_id),
    )
    .await?;
    drop(conn);

    // Absent, already revoked, or someone else's — all 404, never 403, so the
    // response cannot be used to probe which session ids exist.
    if ids.is_empty() {
        return Err(ApiError::NotFound);
    }

    state.revocations.mark_revoked(&ids);
    // Not optional. This is a destructive account action, and without the log
    // the only trace of a session ending is a row that stops being listed.
    tracing::warn!(user_id = %auth.user_id, %session_id, "session revoked by user");
    Ok(Json(serde_json::json!({ "ok": true, "revoked": 1 })))
}

/// `POST /v1/me/sessions/revoke-others`
///
/// No request body — the dashboard sends `{}` and axum needs no `Json`
/// extractor to ignore it.
pub async fn revoke_other_sessions(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    rate_limit(
        &state,
        &format!("sauron:auth:sessions:{}", auth.user_id),
        SESSION_ACTIONS_PER_MIN,
        60,
    )
    .await?;

    // A sid-less legacy token cannot name the session to spare, and sparing
    // nothing would log the caller out of the tab they are looking at. This
    // cannot outlive `JWT_ACCESS_TTL_SECS` past the deploy.
    let Some(sid) = auth.claims.sid else {
        return Err(ApiError::BadRequest(
            "your session predates this feature; reload the dashboard and try again".into(),
        ));
    };

    let mut conn = db(&state).await?;
    let ids = repo::revoke_sessions_for_user(
        &mut conn,
        auth.user_id,
        Some(sid),
        repo::REVOKE_USER_REVOKED_OTHERS,
        Some(auth.user_id),
    )
    .await?;
    drop(conn);

    state.revocations.mark_revoked(&ids);
    tracing::warn!(user_id = %auth.user_id, revoked = ids.len(), "user revoked other sessions");
    Ok(Json(serde_json::json!({ "ok": true, "revoked": ids.len() })))
}

/// Silences an unused-import warning when the module compiles without touching
/// `AuthError` directly; the type is part of this module's error vocabulary via
/// `ApiError::Auth`.
const _: fn(AuthError) -> ApiError = ApiError::Auth;

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME_MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                              (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
    const SAFARI_IOS: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2 like Mac OS X) \
                              AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 \
                              Mobile/15E148 Safari/604.1";

    #[test]
    fn parse_ua_names_a_real_desktop_browser() {
        let (browser, os, kind) = parse_ua(Some(CHROME_MAC));
        assert_eq!(browser.as_deref(), Some("Chrome"));
        assert_eq!(os.as_deref(), Some("Mac OSX"));
        assert_eq!(kind.as_deref(), Some("pc"));
    }

    #[test]
    fn parse_ua_names_a_real_mobile_browser() {
        let (browser, os, kind) = parse_ua(Some(SAFARI_IOS));
        assert_eq!(browser.as_deref(), Some("Safari"));
        assert_eq!(os.as_deref(), Some("iPhone"));
        assert_eq!(kind.as_deref(), Some("smartphone"));
    }

    #[test]
    fn woothees_unknown_sentinels_become_none() {
        // The gotcha this function exists for: woothee answers with the literal
        // string "UNKNOWN" rather than an absence, so a naive mapping renders
        // every unrecognised device as "UNKNOWN on UNKNOWN".
        for ua in [Some("qwertyuiop"), Some(""), None] {
            let (browser, os, kind) = parse_ua(ua);
            assert_eq!(browser, None, "browser for {ua:?}");
            assert_eq!(os, None, "os for {ua:?}");
            assert_eq!(kind, None, "device_kind for {ua:?}");
        }
    }

    #[test]
    fn include_revoked_accepts_both_spellings_and_nothing_else() {
        assert!(truthy(Some(&"1".to_string())));
        assert!(truthy(Some(&"true".to_string())));
        assert!(!truthy(Some(&"0".to_string())));
        assert!(!truthy(Some(&"yes".to_string())));
        assert!(!truthy(None));
    }
}
```

- [ ] **Step 3: Declare the module.** In `backend/bins/sauron-api/src/routes/mod.rs`, add `pub mod account;` to the module list, in alphabetical position — before `pub mod admin;`.

- [ ] **Step 4: Run the unit tests and see them fail then pass.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api account`. If `parse_ua_names_a_real_desktop_browser` or `..._mobile_browser` fails, the assertion is wrong, not the code — print the actual triple and pin **those** values, because they are woothee's vocabulary and matching the ingest pipeline is the whole reason for using it.

- [ ] **Step 5: Mount the three routes.** In `backend/bins/sauron-api/src/main.rs`, directly after `.route("/v1/me", get(routes::auth::me))`, add:

```rust
        // --- the caller's own account ---
        // Not `/v1/sessions` — that name is taken by product telemetry
        // (`GET /v1/apps/{app_id}/sessions`). None of these match
        // `/v1/apps/{app_id}/...`, so `routes::scope::reject_environment_id` is
        // not required and the env-scoping router enumeration does not see them.
        .route("/v1/me/sessions", get(routes::account::list_sessions))
        .route(
            "/v1/me/sessions/{session_id}",
            delete(routes::account::revoke_session),
        )
        .route(
            "/v1/me/sessions/revoke-others",
            post(routes::account::revoke_other_sessions),
        )
```

Note the ordering: axum 0.8 matches the literal `revoke-others` ahead of the `{session_id}` capture regardless of registration order, but the `DELETE`/`POST` split makes them unambiguous anyway.

- [ ] **Step 6: Check, format, lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets` — expected clean. Then `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` and `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings` — expected clean. If clippy flags the `const _: fn(AuthError) -> ApiError = ApiError::Auth;` line as useless, delete that line **and** the `AuthError` import together.

- [ ] **Step 7: Drive it by hand.** Start the API against the live database (`cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron REDIS_URL=redis://172.20.0.3:6379 JWT_SECRET=local-dev-secret-000000000000000000000000 API_PORT=8080 CORS_ALLOWED_ORIGINS=http://localhost:3000 cargo run --bin sauron-api`), log in with `curl -s -XPOST localhost:8080/v1/auth/login -H 'content-type: application/json' -d '{"email":"...","password":"..."}'`, then `curl -s localhost:8080/v1/me/sessions -H "authorization: Bearer $TOKEN"`. Expected: one row, `"current": true`, a populated `browser`/`os` when you pass `-A 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'` on the login call, and **no** `token_hash` and **no** `revoked_by` anywhere in the body.

---

### Task 13: Extract `guard_member_admin_action` and refactor `set_member_active` onto it

**Files:**
- Modify `backend/bins/sauron-api/src/routes/orgs.rs` (imports at lines 1-24; `set_member_active` at lines 702-786)

**Interfaces:**
- Consumes: `authorize_org`, `repo::get_user`, `repo::user_grants_in_org`, `repo::count_user_grants_outside_org`, `grants_from_rows`, `union_permissions`, `check_no_escalation`, `effective_at_org` — all already imported or reachable in this file.
- Produces:

```rust
async fn guard_member_admin_action(
    conn: &mut AsyncPgConnection,
    caller_id: Uuid,
    org_id: Uuid,
    target_user_id: Uuid,
    allow_self: bool,
) -> Result<Vec<(String, Uuid, Value)>, ApiError>
```

No new route, and no change to which callers are allowed through — but **not a byte-for-byte refactor either**, and the two deltas are named here rather than discovered in review. `set_member_active` currently carries six distinct guards and roughly 35 lines of load-bearing why-comment, and two more admin member actions are about to want the same stack. Three verbatim copies is three places for the next guard to be forgotten.

The two behaviour deltas, both 409-message-only:

1. **The cross-org refusal loses its two `is_active`-specific wordings.** "…cannot be reactivated from here" / "…cannot be deactivated from here" become the helper's single "this member belongs to another organization and cannot be administered from here". Intended — the helper is shared by three endpoints now — and `Members.svelte`'s `toggleActive` prints backend 409 text verbatim, so Step 4 checks nothing in the repo pins the old strings.
2. **Self-deactivation is refused after the escalation and cross-org guards rather than before them.** Same status, same message in the ordinary case; the one observable difference is a caller deactivating *themselves* who also holds a grant in another org, who now gets the cross-org 409 instead of "you cannot deactivate your own account". The escalation check cannot change the outcome for a self-target — the caller's permissions are trivially a superset of their own.

What this refactor deliberately does **not** change: self-*reactivation* still succeeds. Today's self-check is guarded by `!req.is_active`, which is why `set_member_active` passes `allow_self: true` in Step 3 and keeps its own 409 rather than delegating to the helper's. The design doc (`docs/superpowers/specs/2026-08-01-session-management-design.md`, "Extract the guard stack first") says `set_member_active` passes `allow_self: false`; that line was written without noticing the `!req.is_active` guard on the existing self-check, and taking it literally silently turns self-reactivation into a 409 reading "use your account page to manage your own sessions". `revoke_member_sessions` (Task 15) and S1's reset still pass `false`, exactly as the design says.

- [ ] **Step 1: Add the import the helper needs.** In `backend/bins/sauron-api/src/routes/orgs.rs`, change `use sauron_db::repo;` to:

```rust
use sauron_db::repo;
use sauron_db::AsyncPgConnection;
```

- [ ] **Step 2: Write the helper.** Directly above `pub async fn set_member_active`, insert:

```rust
/// The guard stack every destructive admin action against another member's
/// *account* must pass, in order, before it touches anything.
///
/// Returns the target's grant rows in this org so callers do not re-query — the
/// same rows the escalation check reads, which is why the membership test and
/// the escalation input are one query rather than two.
///
/// Exactly one of the six guards is waivable, because exactly one caller
/// genuinely differs: `set_member_active` passes `allow_self: true` and keeps
/// its own narrower 409, because self-*reactivation* is legal there and the
/// refusal it does need ("you cannot deactivate your own account") is not the
/// sentence below. `allow_self` stays a parameter rather than a hard-coded
/// refusal because self-target is an *ergonomic* rule about which surface owns a
/// verb, and a future admin action may legitimately want the other answer. The
/// cross-org refusal is not a parameter: that is a blast-radius boundary, and a
/// flag there is an invitation — the next slice wanting the easy answer sets it
/// to `true` and the refusal quietly stops applying to the account it most
/// protects.
///
/// The last-`org:manage` guard deliberately stays **outside** this helper. That
/// concern is specific to deactivation: it is irreversible without an admin,
/// whereas a forced logout is reversible by the victim simply logging in again
/// and so cannot orphan an org.
async fn guard_member_admin_action(
    conn: &mut AsyncPgConnection,
    caller_id: Uuid,
    org_id: Uuid,
    target_user_id: Uuid,
    allow_self: bool,
) -> Result<Vec<(String, Uuid, Value)>, ApiError> {
    // Org-scoped by construction, so a project-scoped Admin cannot reach it.
    authorize_org(conn, caller_id, org_id, perm::MEMBER_MANAGE).await?;

    let _user = repo::get_user(conn, target_user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    // The target must actually be a member of this org, or any admin could act
    // on any account in the deployment by guessing a uuid. The rows are also
    // what the escalation check below reads, so this is one query, not two.
    let target_grants = repo::user_grants_in_org(conn, target_user_id, org_id).await?;
    if target_grants.is_empty() {
        return Err(ApiError::NotFound);
    }

    // Refused before anything else so it always gets the explanatory 409 rather
    // than tripping one of the general guards below.
    if !allow_self && target_user_id == caller_id {
        return Err(ApiError::Conflict(
            "use your account page to manage your own sessions".into(),
        ));
    }

    // You may not act on someone who outranks you — the same rule delete_grant
    // and update_grant_handler already apply to a single grant, and this is
    // strictly more severe than either: it reaches the whole account rather than
    // one scope. Without it an Admin (member:manage, no org:manage) could work
    // through every Owner in turn.
    //
    // The target's side is the union over every grant they hold here, not their
    // org-scoped subset, because the account is not scoped either. The caller's
    // side is deliberately their *org*-scope permissions: an account-global act
    // takes org-level standing, which a project grant does not confer.
    let target_perms = union_permissions(&grants_from_rows(target_grants.clone()));
    let caller = sauron_auth::effective_at_org(conn, caller_id, org_id).await?;
    check_no_escalation(&caller, &target_perms).map_err(ApiError::Auth)?;

    // member:manage is org-scoped; the account is global. An org-A admin acting
    // on a member who is also an org-B Owner is reaching outside their blast
    // radius, and no caller of this helper has a reason to.
    if repo::count_user_grants_outside_org(conn, target_user_id, org_id).await? > 0 {
        return Err(ApiError::Conflict(
            "this member belongs to another organization and cannot be administered from here"
                .into(),
        ));
    }

    Ok(target_grants)
}
```

- [ ] **Step 3: Refactor `set_member_active` onto it.** Replace everything in `set_member_active` from `authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_MANAGE).await?;` down to and including the `count_user_grants_outside_org` `if` block with:

```rust
    // `allow_self: true`, and the self-check stays BELOW this call. Both halves
    // are load-bearing. The self-check this endpoint has always carried is
    // guarded by `!req.is_active`, so self-REACTIVATION succeeds today; passing
    // `false` here would refuse it with the helper's generic "use your account
    // page to manage your own sessions", which is not advice about a
    // reactivation and which `Members.svelte`'s `toggleActive` prints verbatim.
    // And hoisting the self-check above this call would answer a caller holding
    // no `member:manage` in this org with 409 instead of 403 -- deciding
    // something about the target before authorizing the caller at all.
    let _target_grants =
        guard_member_admin_action(&mut conn, auth.user_id, org_id, user_id, true).await?;

    // Self-deactivation gets its own 409 rather than the helper's generic one,
    // because the honest advice differs: there is no "manage your own sessions"
    // answer to "I tried to disable my own login".
    if !req.is_active && user_id == auth.user_id {
        return Err(ApiError::Conflict(
            "you cannot deactivate your own account".into(),
        ));
    }
```

Keep everything after it — the last-`org:manage` guard, `repo::set_user_active`, and the token revocation — exactly as it is. Task 14 converts the revocation.

- [ ] **Step 4: Verify the two 409 messages the dashboard shows verbatim.** `Members.svelte`'s `toggleActive` surfaces the backend's 409 text unchanged, so the wording matters. The cross-org message changes from the two `is_active`-specific variants to one generic sentence; that is intended (the helper is now shared) and the message still names the cause. Confirm nothing else in the repo asserts the old strings: run `grep -rn "cannot be reactivated from here\|cannot be deactivated from here" /home/splimter/projects/freelance/sauron --include=*.rs --include=*.ts --include=*.svelte`. Expected: no hits after the edit. Then confirm the ordering delta landed the right way round by reading the refactored function top to bottom: `guard_member_admin_action(..., true)` first, the `!req.is_active && user_id == auth.user_id` 409 second. If those two are swapped, an unauthorized caller gets 409 instead of 403; if `true` became `false`, self-reactivation starts failing with session advice.

- [ ] **Step 5: Check, format, lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets` — expected clean. Then `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` and `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings` — expected clean. If clippy flags the unused `_target_grants`, leave the underscore prefix; the binding documents that the helper returns rows a future caller will want.

- [ ] **Step 6: Re-run the existing HTTP suites to prove nothing beyond the two named deltas moved.** Run `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api`. Expected: green. Note that green here is a weak signal for the two deltas specifically — no test in the tree asserts either 409 string (the Step 4 grep is what establishes that), so the suite passing does not mean the messages are right. Read them.

---

### Task 14: Convert every revocation site, and stop the theft alarm from poisoning itself

**Files:**
- Modify `backend/bins/sauron-api/src/routes/auth.rs` (`refresh` family kill at line 413, `logout` at line 456, `change_password` at line 538)
- Modify `backend/bins/sauron-api/src/routes/orgs.rs` (`set_member_active` deactivate at line 778)
- Modify `backend/bins/sauron-api/src/routes/mod.rs` (append the call-site pin test)

**Interfaces:**
- Consumes: `repo::revoke_sessions_for_user`, `repo::revoke_refresh_token_and_session` (Task 7); `repo::DELIBERATE_REVOKE_REASONS` (Task 5); `state.revocations.mark_revoked` (Task 10).
- Produces: no new public names. Ends with **zero** uses of the session-blind mass-revoke helpers anywhere in `backend/bins/sauron-api`.

The invariant is that `auth_sessions` and `refresh_tokens` can never disagree, which means **every** site that revokes tokens moves to a session-aware function. **The deactivation conversion is the one most likely to be dropped and the most important**: `AuthUser` reads claims, not `users.is_active`, so a deactivated member's access token otherwise keeps full API access for up to 900 seconds — while the reversible, strictly-less-severe "Sign out" added by this same slice closes that window in ~5 seconds. It also leaves the victim's `auth_sessions` rows live for up to 30 days, so `list_sessions` reports phantom sessions. The `'deactivated'` value in the new CHECK is written by nothing else; if it stays dead, the conversion was dropped.

- [ ] **Step 1: Write the failing pin test.** At the end of `backend/bins/sauron-api/src/routes/mod.rs`, append:

```rust
#[cfg(test)]
mod revocation_call_site_tests {
    /// `auth_sessions` and `refresh_tokens` must never disagree, so every site
    /// in this crate that ends someone's tokens goes through a session-aware
    /// repo function. A sixth site added later would desync the two tables
    /// silently: the session list would show rows with no token behind them, and
    /// the revocation snapshot would never learn about them.
    ///
    /// The needle is assembled at runtime so this test does not match its own
    /// source file.
    #[test]
    fn no_session_blind_mass_revoke_remains_in_this_crate() {
        let needle = format!("revoke_all_{}", "refresh_tokens_for_user");
        let mut offenders = Vec::new();
        let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).expect("read source file");
                for (i, line) in src.lines().enumerate() {
                    if line.contains(&needle) && !line.trim_start().starts_with("//") {
                        offenders.push(format!("{}:{}", path.display(), i + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these call sites revoke tokens without revoking their sessions: {offenders:?}"
        );
    }
}
```

- [ ] **Step 2: Run and see it fail.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api no_session_blind_mass_revoke`. Expected failure: the assertion lists three call sites — `src/routes/auth.rs` twice and `src/routes/orgs.rs` once.

- [ ] **Step 3: Convert the `refresh` family kill and add the deliberate-revocation branch.** In `backend/bins/sauron-api/src/routes/auth.rs`, replace the family-kill lines:

```rust
            let revoked = repo::revoke_all_refresh_tokens_for_user(&mut conn, user_id).await?;
            tracing::warn!(
                %user_id,
                peer = %peer.ip(),
                revoked,
                "refresh token reuse detected; revoked all sessions for the user"
            );
```

with:

```rust
            // A session the user or an admin deliberately ended is not evidence
            // of theft. Without this, the killed device's next refresh lands
            // here on its existing 15-minute timer, trips the family kill, and
            // logs the user out of the session they explicitly chose to KEEP —
            // turning "sign out my other devices" into "sign out all my devices,
            // on a delay". The comment above records this exact class of bug
            // happening before, with routine deactivations.
            //
            // The scope is surgical: only the three deliberate reasons.
            // REVOKE_LOGOUT keeps its current family-kill behaviour — changing
            // it is a separate decision this slice does not make — and
            // REVOKE_DEACTIVATED keeps its existing re-check branch above.
            if reason
                .as_deref()
                .is_some_and(|r| repo::DELIBERATE_REVOKE_REASONS.contains(&r))
            {
                return Err(ApiError::Auth(AuthError::InvalidToken));
            }

            let revoked = repo::revoke_sessions_for_user(
                &mut conn,
                user_id,
                None,
                repo::REVOKE_REUSE,
                // No actor: nobody chose this, replay detection did.
                None,
            )
            .await?;
            state.revocations.mark_revoked(&revoked);
            tracing::warn!(
                %user_id,
                peer = %peer.ip(),
                revoked = revoked.len(),
                "refresh token reuse detected; revoked all sessions for the user"
            );
```

The new `if` must sit **after** the `REVOKE_DEACTIVATED` re-check block and **before** the family kill.

- [ ] **Step 4: Convert `logout`.** Replace the body of `logout` with:

```rust
pub async fn logout(
    State(state): State<AppState>,
    Json(req): Json<LogoutReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let hash = hash_token(&req.refresh_token);
    let mut conn = db(&state).await?;
    // Takes the session with the token. Without this the logged-out session
    // stays live in the owner's own list forever — dead token, live row.
    // Deliberately still unauthenticated: this revokes purely by token hash, and
    // whoever holds the raw refresh token could already revoke it.
    let revoked = repo::revoke_refresh_token_and_session(&mut conn, &hash, repo::REVOKE_LOGOUT)
        .await?;
    drop(conn);
    if let Some(sid) = revoked {
        state.revocations.mark_revoked(&[sid]);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}
```

- [ ] **Step 5: Convert `change_password`.** Replace the `repo::revoke_all_refresh_tokens_for_user_with_reason(...)` call — **keeping the whole comment block above it, which explains the revoke-then-set-then-issue ordering and is still valid** — with:

```rust
    let revoked = repo::revoke_sessions_for_user(
        &mut conn,
        auth.user_id,
        // No exception: the caller's own session dies too. Keeping it would not
        // work anyway — its access token still carries must_change_password, so
        // the extractor gate would keep rejecting the user until it expired.
        None,
        repo::REVOKE_PASSWORD_CHANGED,
        Some(auth.user_id),
    )
    .await?;
    state.revocations.mark_revoked(&revoked);
```

- [ ] **Step 6: Convert the deactivation.** In `backend/bins/sauron-api/src/routes/orgs.rs`, replace:

```rust
    if !req.is_active {
        repo::revoke_all_refresh_tokens_for_user_with_reason(
            &mut conn,
            user_id,
            repo::REVOKE_DEACTIVATED,
        )
        .await?;
    }
```

with:

```rust
    if !req.is_active {
        // Session-aware, not token-only. `AuthUser` reads claims, not
        // `users.is_active`, so a token-only revoke leaves the deactivated
        // member with full API access for up to 900 seconds — making the most
        // severe admin action the weakest one, next to a reversible "Sign out"
        // in the same UI that takes effect in about five. It would also leave
        // their `auth_sessions` rows live for up to 30 days, so their own
        // session list would report devices that cannot actually refresh.
        let revoked = repo::revoke_sessions_for_user(
            &mut conn,
            user_id,
            None,
            repo::REVOKE_DEACTIVATED,
            Some(auth.user_id),
        )
        .await?;
        state.revocations.mark_revoked(&revoked);
    }
```

- [ ] **Step 7: Run the pin test and see it pass.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api no_session_blind_mass_revoke`. Expected: `1 passed`. Keep the repo functions themselves — deleting them is a separate change.

- [ ] **Step 8: Check, format, lint, and re-run the suites.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`, then `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all`, then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`, then `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test --workspace`. All expected green.

---

### Task 15: `POST /v1/orgs/{org_id}/members/{user_id}/revoke-sessions`

**Files:**
- Modify `backend/bins/sauron-api/src/routes/orgs.rs` (append after `set_member_active`)
- Modify `backend/bins/sauron-api/src/main.rs` (router, after the existing `/v1/orgs/{org_id}/members/{user_id}` line)

**Interfaces:**
- Consumes: `guard_member_admin_action` (Task 13), `repo::revoke_sessions_for_user` + `repo::REVOKE_ADMIN` (Tasks 5, 7), `perm::MEMBER_CREDENTIAL` (Task 3), `state.revocations` (Task 10).
- Produces: `routes::orgs::revoke_member_sessions`, returning `{"ok": true, "revoked": <n>}`.

- [ ] **Step 1: Write the handler.** In `backend/bins/sauron-api/src/routes/orgs.rs`, directly after `set_member_active`, insert:

```rust
/// Sign a member out of every device.
///
/// Gated on **both** `member:credential` (checked here, first) and
/// `member:manage` (re-checked inside the shared guard stack). That is the
/// carve-out working as intended: `member:credential` narrows `member:manage`,
/// it does not stand in for it, and a role that can end a member's sessions
/// without otherwise being able to see or administer that member is not a shape
/// anyone asked for.
///
/// Deliberately omits the last-`org:manage` guard `set_member_active` carries:
/// deactivation is irreversible without an admin, whereas a forced logout is
/// reversible by the victim simply logging in again, so it cannot orphan an org.
///
/// Does **not** set `must_change_password` — "force login" is not "force
/// password reset", and `repo::set_user_password` clears that flag
/// unconditionally anyway — and does not touch `is_active`.
///
/// `allow_self` is `false`: this endpoint passes `except: None`, so a
/// self-target would log the admin out of the page they are standing on. "Sign
/// out my other devices" is a different verb, lives on `/account`, and spares
/// the current session.
pub async fn revoke_member_sessions(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_CREDENTIAL).await?;
    let _target_grants =
        guard_member_admin_action(&mut conn, auth.user_id, org_id, user_id, false).await?;

    let ids = repo::revoke_sessions_for_user(
        &mut conn,
        user_id,
        None,
        repo::REVOKE_ADMIN,
        Some(auth.user_id),
    )
    .await?;
    drop(conn);

    state.revocations.mark_revoked(&ids);
    tracing::warn!(
        actor = %auth.user_id,
        %user_id,
        %org_id,
        revoked = ids.len(),
        "admin revoked all sessions for a member"
    );
    Ok(Json(serde_json::json!({ "ok": true, "revoked": ids.len() })))
}
```

- [ ] **Step 2: Mount the route.** In `backend/bins/sauron-api/src/main.rs`, directly after the `.route("/v1/orgs/{org_id}/members/{user_id}", patch(routes::orgs::set_member_active))` block, add:

```rust
        .route(
            "/v1/orgs/{org_id}/members/{user_id}/revoke-sessions",
            post(routes::orgs::revoke_member_sessions),
        )
```

- [ ] **Step 3: Check, format, lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`, then `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all`, then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Expected: clean.

- [ ] **Step 4: Drive the happy path and the self-target refusal by hand.** With the API running against the live database (command in Task 12 Step 7), as an Owner: `curl -s -o /dev/null -w '%{http_code}\n' -XPOST "localhost:8080/v1/orgs/$ORG/members/$SELF/revoke-sessions" -H "authorization: Bearer $TOKEN"` — expected `409`. Then against another member of the same org — expected `200` with `{"ok":true,"revoked":N}`, and that member's previously-working access token starts returning 401 within a few seconds.

---

### Task 16: End-to-end tests over the real binary

**Files:**
- Modify `backend/bins/sauron-api/Cargo.toml` (the `reqwest` line in `[dev-dependencies]`, line 41)
- Create `backend/bins/sauron-api/tests/http_sessions.rs`

**Interfaces:**
- Consumes: every endpoint and conversion from Tasks 8-15.
- Produces: nothing other crates use.

These are the tests nothing else can see: the rotation-grace interaction, the measured residual window, and the admin guard matrix over HTTP where it is actually enforced.

- [ ] **Step 0: Turn on reqwest's `json` feature for sauron-api's tests.** The helpers below use `.json(&body)` and `resp.json()`, and neither compiles today: `backend/Cargo.toml:59` declares `reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "gzip"] }`, sauron-api inherits it verbatim, and the only crate that adds `"json"` is `bins/sauron-monitor` — which cannot help, because the workspace is `resolver = "2"` and `cargo test -p sauron-api --test http_sessions` never builds it. In `backend/bins/sauron-api/Cargo.toml`, replace the `[dev-dependencies]` block's comment and `reqwest` line with:

```toml
[dev-dependencies]
# Drives the real, compiled `sauron-api` binary over HTTP in
# `tests/http_env_scoping.rs`, `tests/http_workflows.rs` and
# `tests/http_sessions.rs` — the only way to exercise the actual extractor stack
# a unit test cannot see. The "json" feature is test-only: reqwest is not a
# normal dependency of this crate, so nothing in the shipped binary gains it.
reqwest = { workspace = true, features = ["json"] }
```

  Then run `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check -p sauron-api --all-targets`. Expected: clean — the existing two HTTP suites still compile.

- [ ] **Step 1: Copy the harness.** Create `backend/bins/sauron-api/tests/http_sessions.rs` and copy into it, **verbatim from `backend/bins/sauron-api/tests/http_workflows.rs`**, the following items with their doc comments: `swap_database`, `free_port`, the `TestServer` struct, its `impl` (`start`, `conn`, `shutdown`), and the `impl Drop for TestServer`. Do not copy `percent_encode_segment` or the workflow fixtures. Then make these four changes to the copy:
  - the file's own module doc comment becomes the one in Step 2;
  - `const JWT_SECRET: &str = "http-sessions-test-secret-000000000000000";`
  - the db name discriminator changes from `wf` to `sn`: `format!("sauron_test_{}_sn{}", Utc::now().timestamp(), Uuid::new_v4().simple())`. **Segment order is load-bearing** — timestamp FIRST, discriminator glued to the uuid — because `sauron-db`'s stale-database reaper parses the first underscore-delimited segment after `sauron_test_` as a timestamp and silently skips anything else, leaking every database it cannot parse.
  - add `.env("AUTH_REVOCATION_POLL_SECS", "1")` to the child process env, directly after `.env("CORS_ALLOWED_ORIGINS", ...)`, so timing assertions are seconds rather than minutes.

  Also add these three helper methods to the `TestServer` impl (the copied one only has `get`/`get_status`/`get_json`):

```rust
    async fn post_json(&self, path: &str, token: Option<&str>, body: Value) -> reqwest::Response {
        let mut req = self.client.post(format!("{}{path}", self.base)).json(&body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        req.send()
            .await
            .unwrap_or_else(|e| panic!("POST {path} failed: {e}"))
    }

    async fn delete(&self, path: &str, token: &str) -> reqwest::Response {
        self.client
            .delete(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|e| panic!("DELETE {path} failed: {e}"))
    }

    /// Log in over HTTP with a chosen `User-Agent`, returning
    /// `(access_token, refresh_token)`.
    async fn login(&self, email: &str, password: &str, ua: &str) -> (String, String) {
        let resp = self
            .client
            .post(format!("{}/v1/auth/login", self.base))
            .header(reqwest::header::USER_AGENT, ua)
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .expect("login request");
        let status = resp.status();
        let body: Value = resp.json().await.expect("login body");
        assert!(status.is_success(), "login failed ({status}): {body}");
        (
            body["access_token"].as_str().expect("access_token").to_string(),
            body["refresh_token"].as_str().expect("refresh_token").to_string(),
        )
    }
```

- [ ] **Step 2: Write the module header, imports and fixture.** At the top of the file, above the copied harness:

```rust
//! End-to-end session management against the real compiled `sauron-api`
//! binary: the `/v1/me/sessions` surface, the admin force-logout guard matrix,
//! and the three behaviours no unit test can observe — the rotation-grace
//! interaction, the measured residual access-token window, and the fact that
//! "sign out other devices" does not fire the theft alarm fifteen minutes later.
//!
//! Runs with `AUTH_REVOCATION_POLL_SECS=1` in the child env so the timing
//! assertions are seconds.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset.

use std::cell::Cell;
use std::process::Stdio;
use std::time::Duration as StdDuration;

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::perm;
use sauron_db::models::NewRoleGrant;
use sauron_db::repo;
```

Then, below the harness, add:

```rust
const PASSWORD: &str = "correct-horse-battery-staple";
const UA_CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const UA_FIREFOX: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0";

/// Register an org owner over HTTP and return `(email, user_id, org_id)`.
async fn register_owner(server: &TestServer, label: &str) -> (String, Uuid, Uuid) {
    let email = format!("{label}-{}@example.com", Uuid::new_v4().simple());
    let resp = server
        .post_json(
            "/v1/auth/register",
            None,
            json!({
                "email": email,
                "password": PASSWORD,
                "name": label,
                "org_name": format!("{label} org"),
            }),
        )
        .await;
    let status = resp.status();
    let body: Value = resp.json().await.expect("register body");
    assert!(status.is_success(), "register failed ({status}): {body}");
    let user_id: Uuid = body["user"]["id"].as_str().unwrap().parse().unwrap();

    let mut conn = server.conn().await;
    let orgs = repo::list_orgs_for_user(&mut conn, user_id)
        .await
        .expect("list orgs");
    let org_id = orgs.first().expect("owner has an org").id;
    drop(conn);
    (email, user_id, org_id)
}

/// Create a member of `org_id` with `role_perms`, log them in, and return
/// `(email, user_id, access_token, refresh_token)`.
async fn seed_member(
    server: &TestServer,
    org_id: Uuid,
    label: &str,
    role_perms: &[&str],
) -> (String, Uuid, String, String) {
    let mut conn = server.conn().await;
    let email = format!("{label}-{}@example.com", Uuid::new_v4().simple());
    let hash = sauron_auth::hash_password(PASSWORD).expect("hash password");
    let user = repo::create_user(&mut conn, &email, &hash, label)
        .await
        .expect("create member");
    // repo.rs:403 — `create_role(conn, org_id: Uuid, name: &str,
    // description: &str, permissions: Value)`. The description is a plain
    // `&str`, not an `Option`, and the permissions are an owned `Value`.
    let role = repo::create_role(
        &mut conn,
        org_id,
        &format!("{label}-role-{}", Uuid::new_v4().simple()),
        "http_sessions fixture",
        json!(role_perms),
    )
    .await
    .expect("create role");
    repo::create_grant(
        &mut conn,
        NewRoleGrant {
            org_id,
            user_id: user.id,
            role_id: role.id,
            scope_type: "org".to_string(),
            scope_id: org_id,
        },
    )
    .await
    .expect("grant role");
    drop(conn);

    let (access, refresh) = server.login(&email, PASSWORD, UA_FIREFOX).await;
    (email, user.id, access, refresh)
}

/// Poll `GET /v1/me` with `token` until it stops returning 2xx, up to `secs`.
/// Returns the elapsed seconds, or panics with the last status.
async fn seconds_until_token_dies(server: &TestServer, token: &str, secs: u64) -> u64 {
    for elapsed in 0..=secs {
        let status = server.get_status("/v1/me", token).await;
        if status == 401 {
            return elapsed;
        }
        tokio::time::sleep(StdDuration::from_secs(1)).await;
    }
    panic!("access token still worked after {secs}s; the revocation snapshot never saw it");
}
```

- [ ] **Step 3: Confirm the fixture helpers still match the real repo signatures.** Run `grep -n -A 7 "pub async fn create_role\|pub async fn create_user\|pub async fn list_orgs_for_user\|pub async fn create_grant\|pub fn hash_password" /home/splimter/projects/freelance/sauron/backend/crates/sauron-db/src/repo.rs /home/splimter/projects/freelance/sauron/backend/crates/sauron-auth/src/password.rs`. Expected, as of this plan: `create_role(conn, org_id: Uuid, name: &str, description: &str, permissions: Value)`, `create_user(conn, email: &str, password_hash: &str, name: &str)`, `list_orgs_for_user(conn, user_id: Uuid)`, `create_grant(conn, grant: NewRoleGrant)`, `hash_password(password: &str) -> anyhow::Result<String>`. The two `create_role` calls in this task (Step 2's `seed_member` and Step 5's outside-org role) are written against exactly that list — if a signature has moved since, follow the tree and adjust both call sites, not just the first.

- [ ] **Step 4: Write the self-service tests.** Append:

```rust
#[tokio::test]
async fn two_logins_produce_two_sessions_with_exactly_one_current() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (email, _user_id, _org_id) = register_owner(&server, "twosessions").await;
    let (access_a, _refresh_a) = server.login(&email, PASSWORD, UA_CHROME).await;
    let (_access_b, _refresh_b) = server.login(&email, PASSWORD, UA_FIREFOX).await;

    let body = server.get_json("/v1/me/sessions", &access_a).await;
    let rows = body.as_array().expect("array of sessions");
    // register + two logins = three sessions.
    assert_eq!(rows.len(), 3, "one session per login: {body}");
    assert_eq!(
        rows.iter().filter(|r| r["current"] == json!(true)).count(),
        1,
        "exactly one row is the caller's own session: {body}"
    );
    let current = rows.iter().find(|r| r["current"] == json!(true)).unwrap();
    assert_eq!(current["browser"], json!("Chrome"));

    // The structural guarantee: `list_sessions` never touches `refresh_tokens`,
    // so a token hash cannot leak through this endpoint.
    let raw = body.to_string();
    assert!(!raw.contains("token_hash"), "session list leaked a token hash");
    assert!(!raw.contains("revoked_by"), "session list leaked the revoking admin");

    server.shutdown().await;
}

#[tokio::test]
async fn revoking_the_session_you_are_using_is_refused() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (email, _user_id, _org_id) = register_owner(&server, "selfrevoke").await;
    let (access, _refresh) = server.login(&email, PASSWORD, UA_CHROME).await;

    let body = server.get_json("/v1/me/sessions", &access).await;
    let current = body
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["current"] == json!(true))
        .expect("a current session");
    let sid = current["id"].as_str().unwrap();

    let resp = server.delete(&format!("/v1/me/sessions/{sid}"), &access).await;
    assert_eq!(resp.status().as_u16(), 409);

    // An unknown id is 404, never 403 — a 403 would confirm the id exists.
    let resp = server
        .delete(&format!("/v1/me/sessions/{}", Uuid::new_v4()), &access)
        .await;
    assert_eq!(resp.status().as_u16(), 404);

    server.shutdown().await;
}

#[tokio::test]
async fn revoke_others_spares_the_caller_and_does_not_fire_the_theft_alarm() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (email, _user_id, _org_id) = register_owner(&server, "revokeothers").await;
    let (access_keep, refresh_keep) = server.login(&email, PASSWORD, UA_CHROME).await;
    let (access_kill, refresh_kill) = server.login(&email, PASSWORD, UA_FIREFOX).await;

    let resp = server
        .post_json("/v1/me/sessions/revoke-others", Some(&access_keep), json!({}))
        .await;
    assert_eq!(resp.status().as_u16(), 200);

    let listed = server.get_json("/v1/me/sessions", &access_keep).await;
    assert_eq!(listed.as_array().unwrap().len(), 1, "only the spared session remains");

    // THE REGRESSION TEST. The killed device presents its dead token; without the
    // DELIBERATE_REVOKE_REASONS branch this trips the family kill and the spared
    // session dies on the next line.
    let resp = server
        .post_json("/v1/auth/refresh", None, json!({ "refresh_token": refresh_kill }))
        .await;
    assert_eq!(resp.status().as_u16(), 401);

    let resp = server
        .post_json("/v1/auth/refresh", None, json!({ "refresh_token": refresh_keep }))
        .await;
    assert_eq!(
        resp.status().as_u16(),
        200,
        "the spared session must still refresh AFTER the killed device knocks"
    );

    // The measured residual window, not a claim in prose.
    let elapsed = seconds_until_token_dies(&server, &access_kill, 10).await;
    assert!(elapsed <= 5, "revoked access token survived {elapsed}s");

    server.shutdown().await;
}

#[tokio::test]
async fn a_revoked_session_cannot_be_resurrected_inside_the_rotation_grace_window() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (email, _user_id, _org_id) = register_owner(&server, "gracewindow").await;
    // Session A stays live, so `user_has_active_refresh_token` is true and the
    // grace condition is genuinely reachable.
    let (access_a, _refresh_a) = server.login(&email, PASSWORD, UA_CHROME).await;
    let (_access_b, refresh_b) = server.login(&email, PASSWORD, UA_FIREFOX).await;

    // Rotate B, so its old token's reason is exactly `rotated`.
    let resp = server
        .post_json("/v1/auth/refresh", None, json!({ "refresh_token": refresh_b }))
        .await;
    assert_eq!(resp.status().as_u16(), 200);

    // Kill B from A, inside the 10-second grace.
    let listed = server.get_json("/v1/me/sessions", &access_a).await;
    let other = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["current"] == json!(false))
        .expect("session B");
    let sid_b = other["id"].as_str().unwrap();
    let resp = server.delete(&format!("/v1/me/sessions/{sid_b}"), &access_a).await;
    assert_eq!(resp.status().as_u16(), 200);

    // B's other tab now presents the pre-rotation token: reason IS `rotated`,
    // it IS inside the grace, and the user DOES still hold a live token. Only
    // `WHERE auth_sessions.revoked_at IS NULL` inside the mint CTE stops this.
    let resp = server
        .post_json("/v1/auth/refresh", None, json!({ "refresh_token": refresh_b }))
        .await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "the grace window resurrected a session the user had just revoked"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn deactivating_a_member_kills_their_access_token_within_the_poll_interval() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, _owner_id, org_id) = register_owner(&server, "deactivate").await;
    let (owner_access, _owner_refresh) = server.login(&owner_email, PASSWORD, UA_CHROME).await;
    let (_email, member_id, member_access, _member_refresh) =
        seed_member(&server, org_id, "victim", &[perm::ISSUE_READ]).await;

    assert!(server.get_status("/v1/me", &member_access).await < 400);

    let resp = server
        .client
        .patch(format!("{}/v1/orgs/{org_id}/members/{member_id}", server.base))
        .bearer_auth(&owner_access)
        .json(&json!({ "is_active": false }))
        .send()
        .await
        .expect("deactivate");
    assert_eq!(resp.status().as_u16(), 200);

    // The conversion this test exists for: without it the deactivated member
    // keeps full API access for up to 900 seconds.
    let elapsed = seconds_until_token_dies(&server, &member_access, 10).await;
    assert!(elapsed <= 5, "deactivated member kept access for {elapsed}s");

    server.shutdown().await;
}
```

- [ ] **Step 5: Write the admin guard matrix.** Append:

```rust
#[tokio::test]
async fn the_admin_force_logout_guard_matrix_holds_over_http() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, owner_id, org_id) = register_owner(&server, "adminkill").await;
    let (owner_access, _owner_refresh) = server.login(&owner_email, PASSWORD, UA_CHROME).await;

    // A custom role holding member:manage but NOT member:credential — the exact
    // role the carve-out exists to make possible.
    let (_a_email, _a_id, manage_only_access, _a_refresh) = seed_member(
        &server,
        org_id,
        "manageonly",
        &[perm::MEMBER_READ, perm::MEMBER_MANAGE],
    )
    .await;
    let (_v_email, victim_id, victim_access, _v_refresh) =
        seed_member(&server, org_id, "target", &[perm::ISSUE_READ]).await;

    let path = format!("/v1/orgs/{org_id}/members/{victim_id}/revoke-sessions");

    // 403: member:manage without member:credential. Both are required.
    let resp = server.post_json(&path, Some(&manage_only_access), json!({})).await;
    assert_eq!(resp.status().as_u16(), 403);

    // 404: a real user with no grants in this org.
    let stranger = {
        let mut conn = server.conn().await;
        let hash = sauron_auth::hash_password(PASSWORD).expect("hash");
        let u = repo::create_user(
            &mut conn,
            &format!("stranger-{}@example.com", Uuid::new_v4().simple()),
            &hash,
            "stranger",
        )
        .await
        .expect("create stranger");
        drop(conn);
        u.id
    };
    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/{stranger}/revoke-sessions"),
            Some(&owner_access),
            json!({}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 404);

    // 409: self-target. `except: None` would log the admin out of the page they
    // are standing on.
    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/{owner_id}/revoke-sessions"),
            Some(&owner_access),
            json!({}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 409);

    // 200 happy path, and the member is ejected within the poll interval.
    let resp = server.post_json(&path, Some(&owner_access), json!({})).await;
    assert_eq!(resp.status().as_u16(), 200);
    let elapsed = seconds_until_token_dies(&server, &victim_access, 10).await;
    assert!(elapsed <= 5, "force-logged-out member kept access for {elapsed}s");

    // "Force login" is not "force password reset", and it is not deactivation.
    let mut conn = server.conn().await;
    let victim = repo::get_user(&mut conn, victim_id)
        .await
        .expect("get user")
        .expect("victim exists");
    drop(conn);
    assert!(!victim.must_change_password, "force-logout must not force a reset");
    assert!(victim.is_active, "force-logout must not deactivate");

    server.shutdown().await;
}

#[tokio::test]
async fn admin_force_logout_refuses_a_target_who_outranks_or_reaches_outside_the_org() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let (owner_email, owner_id, org_id) = register_owner(&server, "outrank").await;
    let (_owner_access, _owner_refresh) = server.login(&owner_email, PASSWORD, UA_CHROME).await;

    // An Admin-shaped caller: member:manage + member:credential, no org:manage.
    let (_admin_email, _admin_id, admin_access, _admin_refresh) = seed_member(
        &server,
        org_id,
        "adminish",
        &[perm::MEMBER_READ, perm::MEMBER_MANAGE, perm::MEMBER_CREDENTIAL],
    )
    .await;

    // 403: the target is the Owner, who holds org:manage.
    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/{owner_id}/revoke-sessions"),
            Some(&admin_access),
            json!({}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 403);

    // 409: the target also holds a grant in another org — outside this caller's
    // blast radius.
    let (_multi_email, multi_id, _multi_access, _multi_refresh) =
        seed_member(&server, org_id, "multiorg", &[perm::ISSUE_READ]).await;
    let (_other_email, _other_id, other_org_id) = register_owner(&server, "otherorg").await;
    {
        let mut conn = server.conn().await;
        let role = repo::create_role(
            &mut conn,
            other_org_id,
            &format!("outside-{}", Uuid::new_v4().simple()),
            "http_sessions fixture",
            json!([perm::ISSUE_READ]),
        )
        .await
        .expect("create outside role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: other_org_id,
                user_id: multi_id,
                role_id: role.id,
                scope_type: "org".to_string(),
                scope_id: other_org_id,
            },
        )
        .await
        .expect("grant outside role");
        drop(conn);
    }
    let resp = server
        .post_json(
            &format!("/v1/orgs/{org_id}/members/{multi_id}/revoke-sessions"),
            Some(&admin_access),
            json!({}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 409);

    server.shutdown().await;
}
```

- [ ] **Step 6: Run the suite.** Run `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api --test http_sessions -- --test-threads=2`. Expected: `7 passed`. Each test spawns its own binary and its own database, so a high thread count multiplies both.

- [ ] **Step 7: Confirm no ephemeral databases leaked.** Run `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "SELECT datname FROM pg_database WHERE datname LIKE 'sauron_test_%';"`. Expected: no rows, or only rows a concurrently-running suite owns. If a `_sn` database is left behind, `shutdown()` was not reached on some path — fix that rather than dropping it by hand.

- [ ] **Step 8: Format and lint.** Run `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Expected: clean.

---

### Task 17: `AccountSession` and the pure session-display logic

**Files:**
- Modify `dashboard/src/lib/models/index.ts` (append near the other view types)
- Create `dashboard/src/lib/models/account-sessions.ts`
- Create `dashboard/src/lib/models/account-sessions.test.ts`

**Interfaces:**
- Consumes: the `SessionView` JSON shape from Task 12.
- Produces: `AccountSession`; `describeSession(s: AccountSession): string`; `sortSessions(list: AccountSession[]): AccountSession[]`; `otherSessionCount(list: AccountSession[]): number`; `hasCurrentSession(list: AccountSession[]): boolean`; `allSameIp(list: AccountSession[]): boolean`.

**The name `AccountSession` is mandatory, not stylistic.** `AuthSession` is already taken in `models/index.ts` (it extends `AuthTokens` and is the login response); shadowing it would compile in some files while silently changing the meaning of the auth store's types in others.

- [ ] **Step 1: Add the type.** In `dashboard/src/lib/models/index.ts`, directly above `export type Permission =`, insert:

```ts
/**
 * One row of GET /v1/me/sessions — a login of the current user that has
 * survived refresh-token rotation.
 *
 * NOT `AuthSession`: that name is taken above by the login *response*, and
 * shadowing it compiles while silently changing the auth store's types.
 *
 * `revoked_by` is deliberately absent — the API never serializes it, because it
 * would tell a member which admin signed them out.
 */
export interface AccountSession {
  id: string;
  created_at: string;
  last_used_at: string;
  expires_at: string;
  /** Marked server-side; the dashboard has no JWT decoder and should not gain one. */
  current: boolean;
  user_agent: string | null;
  browser: string | null;
  os: string | null;
  device_kind: string | null;
  ip: string | null;
  /** Only ever set on rows returned with `?include_revoked=1`. */
  revoked_at: string | null;
  revoked_reason: string | null;
}
```

- [ ] **Step 2: Write the failing tests.** Create `dashboard/src/lib/models/account-sessions.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import {
  allSameIp,
  describeSession,
  hasCurrentSession,
  otherSessionCount,
  sortSessions,
} from './account-sessions';
import type { AccountSession } from './index';

function session(over: Partial<AccountSession> = {}): AccountSession {
  return {
    id: 'a',
    created_at: '2026-08-01T10:00:00Z',
    last_used_at: '2026-08-01T10:00:00Z',
    expires_at: '2026-08-31T10:00:00Z',
    current: false,
    user_agent: null,
    browser: null,
    os: null,
    device_kind: null,
    ip: null,
    revoked_at: null,
    revoked_reason: null,
    ...over,
  };
}

describe('describeSession', () => {
  it('prefers browser and os together', () => {
    expect(describeSession(session({ browser: 'Chrome', os: 'Mac OSX' }))).toBe('Chrome on Mac OSX');
  });

  it('falls back to whichever half it has', () => {
    expect(describeSession(session({ browser: 'Safari' }))).toBe('Safari');
    expect(describeSession(session({ os: 'Windows 11' }))).toBe('Windows 11');
  });

  it('falls back to a truncated raw user agent', () => {
    const raw = 'x'.repeat(80);
    const out = describeSession(session({ user_agent: raw }));
    expect(out).toHaveLength(61); // 60 characters plus the ellipsis
    expect(out.endsWith('…')).toBe(true);
    expect(describeSession(session({ user_agent: 'curl/8.5.0' }))).toBe('curl/8.5.0');
  });

  it('falls back to Unknown device when there is nothing at all', () => {
    expect(describeSession(session())).toBe('Unknown device');
    // The server normalises woothee's "UNKNOWN" sentinel to null, but a
    // whitespace-only string must not render as a blank cell either.
    expect(describeSession(session({ browser: '  ', os: '', user_agent: '   ' }))).toBe(
      'Unknown device',
    );
  });
});

describe('sortSessions', () => {
  it('puts the current session first, then most recently used', () => {
    const a = session({ id: 'a', last_used_at: '2026-08-01T09:00:00Z' });
    const b = session({ id: 'b', last_used_at: '2026-08-01T11:00:00Z' });
    const c = session({ id: 'c', last_used_at: '2026-08-01T08:00:00Z', current: true });
    expect(sortSessions([a, b, c]).map((s) => s.id)).toEqual(['c', 'b', 'a']);
  });

  it('does not mutate its input', () => {
    const list = [session({ id: 'a' }), session({ id: 'b', current: true })];
    sortSessions(list);
    expect(list.map((s) => s.id)).toEqual(['a', 'b']);
  });
});

describe('otherSessionCount and hasCurrentSession', () => {
  it('are zero and false on an empty list', () => {
    expect(otherSessionCount([])).toBe(0);
    expect(hasCurrentSession([])).toBe(false);
  });

  it('counts only live, non-current rows', () => {
    const list = [
      session({ id: 'a', current: true }),
      session({ id: 'b' }),
      session({ id: 'c', revoked_at: '2026-08-01T10:30:00Z' }),
    ];
    expect(otherSessionCount(list)).toBe(1);
    expect(hasCurrentSession(list)).toBe(true);
  });

  it('reports no current session for a legacy token, which is what disables the UI', () => {
    const list = [session({ id: 'a' }), session({ id: 'b' })];
    expect(hasCurrentSession(list)).toBe(false);
    expect(otherSessionCount(list)).toBe(2);
  });
});

describe('allSameIp', () => {
  it('is true only when two or more live rows share one address', () => {
    expect(allSameIp([session({ ip: '10.0.0.5' }), session({ ip: '10.0.0.5' })])).toBe(true);
  });

  it('is false for mixed, single-row, empty and null-bearing lists', () => {
    expect(allSameIp([session({ ip: '10.0.0.5' }), session({ ip: '10.0.0.6' })])).toBe(false);
    expect(allSameIp([session({ ip: '10.0.0.5' })])).toBe(false);
    expect(allSameIp([])).toBe(false);
    expect(allSameIp([session({ ip: '10.0.0.5' }), session({ ip: null })])).toBe(false);
  });

  it('ignores revoked rows, which may legitimately come from anywhere', () => {
    const list = [
      session({ id: 'a', ip: '10.0.0.5' }),
      session({ id: 'b', ip: '10.0.0.5' }),
      session({ id: 'c', ip: '203.0.113.9', revoked_at: '2026-08-01T10:30:00Z' }),
    ];
    expect(allSameIp(list)).toBe(true);
  });
});
```

- [ ] **Step 3: Run and see it fail.** Run `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`. Expected failure: `Failed to resolve import "./account-sessions"`.

- [ ] **Step 4: Write the module.** Create `dashboard/src/lib/models/account-sessions.ts`:

```ts
import type { AccountSession } from './index';

/** Longest raw user-agent string rendered before it is elided. */
const MAX_RAW_UA = 60;

function clean(v: string | null): string | null {
  const t = v?.trim();
  return t ? t : null;
}

function isLive(s: AccountSession): boolean {
  return s.revoked_at === null;
}

/**
 * How to *phrase* a device.
 *
 * The client half of a deliberate split: the server answers the data question
 * (what does this UA string mean, using the same woothee vocabulary the ingest
 * pipeline uses), and this answers the copy question.
 */
export function describeSession(s: AccountSession): string {
  const browser = clean(s.browser);
  const os = clean(s.os);
  if (browser && os) return `${browser} on ${os}`;
  if (browser) return browser;
  if (os) return os;
  const raw = clean(s.user_agent);
  if (raw) return raw.length > MAX_RAW_UA ? `${raw.slice(0, MAX_RAW_UA)}…` : raw;
  return 'Unknown device';
}

/**
 * Current session first, then most recently used.
 *
 * Returns a new array: the caller holds this list in `$state`, and sorting in
 * place would mutate a proxied array during a derivation.
 */
export function sortSessions(list: AccountSession[]): AccountSession[] {
  return [...list].sort((a, b) => {
    if (a.current !== b.current) return a.current ? -1 : 1;
    return Date.parse(b.last_used_at) - Date.parse(a.last_used_at);
  });
}

/** Live sessions that are not the caller's own — what "Sign out other devices" reaches. */
export function otherSessionCount(list: AccountSession[]): number {
  return list.filter((s) => isLive(s) && !s.current).length;
}

/**
 * Does the caller's own access token name a session in this list?
 *
 * False means a legacy token minted before the session feature shipped: the
 * server refuses `revoke-others` for it (it has nothing to spare), so the UI
 * disables both revoke affordances rather than offering an action that 400s.
 */
export function hasCurrentSession(list: AccountSession[]): boolean {
  return list.some((s) => isLive(s) && s.current);
}

/**
 * Do all live rows report one address?
 *
 * On both shipped topologies they will: `API_TRUST_FORWARDED_HEADERS` defaults
 * to false in `config.rs`, in `packaging/rpm/config/api.env` and in
 * docker-compose, and the shipped nginx sits in front — so every session records
 * the proxy. Detecting it client-side turns a column that looks broken into a
 * legible configuration message, with no new API surface.
 *
 * A single row is not evidence of anything, and a null address is not an
 * address, so both answer false.
 */
export function allSameIp(list: AccountSession[]): boolean {
  const ips = list.filter(isLive).map((s) => s.ip);
  if (ips.length < 2) return false;
  if (ips.some((ip) => ip === null)) return false;
  return ips.every((ip) => ip === ips[0]);
}
```

- [ ] **Step 5: Run and see it pass.** Run `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`. Expected: green, including the new `account-sessions` describe blocks.

- [ ] **Step 6: Typecheck.** Run `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`. Expected: no new errors.

---

### Task 18: The `#/account` page

**Files:**
- Create `dashboard/src/lib/api/account.ts`
- Create `dashboard/src/pages/Account.svelte`
- Modify `dashboard/src/routes.ts`
- Modify `dashboard/src/lib/components/layout/Sidebar.svelte`

**Interfaces:**
- Consumes: `AccountSession` and the five pure functions (Task 17); the three `/v1/me/sessions` endpoints (Task 12).
- Produces: `listMySessions(includeRevoked?: boolean)`, `revokeMySession(id)`, `revokeMyOtherSessions()`, `revokeMemberSessions(orgId, userId)` (the last is consumed by Task 19); the route `'/account'`.

Built as a **stack of cards from day one** — Profile, Active sessions — so the notification-preferences slice adds a card rather than restructuring the page.

- [ ] **Step 1: Write the API client.** Create `dashboard/src/lib/api/account.ts`:

```ts
import { api } from './client';
import type { AccountSession } from '../models';

/**
 * The `/v1/me/*` namespace. Bearer-authenticated, so this goes through `api`
 * and never `bareClient` — these calls must participate in the 401
 * refresh-and-replay.
 *
 * `api/scope.ts`'s `computeScopeParams` only matches `/^\/v1\/apps\/[^/]+/`, so
 * none of these paths pick up an `environment_id` param and no
 * `BACKEND_REJECTS_ENVIRONMENT_ID` entry is needed — which is what keeps the
 * Rust-side router enumeration in `http_env_scoping.rs` green.
 */
export async function listMySessions(includeRevoked = false): Promise<AccountSession[]> {
  const { data } = await api.get<AccountSession[]>('/v1/me/sessions', {
    params: includeRevoked ? { include_revoked: 1 } : undefined,
  });
  return data;
}

export async function revokeMySession(sessionId: string): Promise<void> {
  await api.delete(`/v1/me/sessions/${sessionId}`);
}

export async function revokeMyOtherSessions(): Promise<number> {
  const { data } = await api.post<{ ok: boolean; revoked: number }>(
    '/v1/me/sessions/revoke-others',
    {},
  );
  return data.revoked;
}

/** Admin force-logout. Requires `member:credential` AND `member:manage`. */
export async function revokeMemberSessions(orgId: string, userId: string): Promise<number> {
  const { data } = await api.post<{ ok: boolean; revoked: number }>(
    `/v1/orgs/${orgId}/members/${userId}/revoke-sessions`,
    {},
  );
  return data.revoked;
}
```

- [ ] **Step 2: Write the page.** Create `dashboard/src/pages/Account.svelte`:

```svelte
<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import { authStore } from '../lib/stores/auth.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { errorMessage } from '../lib/api/client';
  import { listMySessions, revokeMyOtherSessions, revokeMySession } from '../lib/api/account';
  import { formatDateTime, relativeTime } from '../lib/utils/format';
  import {
    allSameIp,
    describeSession,
    hasCurrentSession,
    otherSessionCount,
    sortSessions,
  } from '../lib/models/account-sessions';
  import type { AccountSession } from '../lib/models';

  let sessions = $state<AccountSession[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let showRevoked = $state(false);
  let busy = $state(false);

  // One dialog for both verbs, matching Members.svelte's requestToggle /
  // confirmDeactivate shape. `$state.raw` because this is replaced wholesale and
  // nothing reads through a proxy.
  let pending = $state.raw<{ kind: 'one'; id: string; label: string } | { kind: 'all' } | null>(
    null,
  );

  const rows = $derived(sortSessions(sessions));
  const live = $derived(rows.filter((s) => s.revoked_at === null));
  const revoked = $derived(rows.filter((s) => s.revoked_at !== null));
  const otherCount = $derived(otherSessionCount(sessions));
  const hasCurrent = $derived(hasCurrentSession(sessions));
  const proxied = $derived(allSameIp(sessions));

  async function load() {
    loading = true;
    error = null;
    try {
      sessions = await listMySessions(showRevoked);
    } catch (err) {
      error = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  async function toggleHistory() {
    showRevoked = !showRevoked;
    await load();
  }

  function requestRevokeOne(s: AccountSession) {
    pending = { kind: 'one', id: s.id, label: describeSession(s) };
  }

  function requestRevokeAll() {
    pending = { kind: 'all' };
  }

  async function confirmPending() {
    const target = pending;
    if (!target) return;
    busy = true;
    try {
      if (target.kind === 'one') {
        await revokeMySession(target.id);
        toastStore.success('That device will be signed out within a few seconds.');
      } else {
        const n = await revokeMyOtherSessions();
        toastStore.success(
          n === 1 ? 'One other device signed out.' : `${n} other devices signed out.`,
        );
      }
      pending = null;
      await load();
    } catch (err) {
      // The backend's 409/400/404 bodies carry the actionable text — surface it
      // verbatim rather than a generic failure.
      toastStore.error(errorMessage(err));
    } finally {
      busy = false;
    }
  }

  function reasonLabel(reason: string | null): string {
    switch (reason) {
      case 'logout':
        return 'Logged out';
      case 'user_revoked':
        return 'Signed out from your account page';
      case 'user_revoked_others':
        return 'Signed out with "other devices"';
      case 'admin_revoked':
        return 'Signed out by an administrator';
      case 'password_changed':
        return 'Password changed';
      case 'deactivated':
        return 'Account deactivated';
      case 'reuse':
        return 'Security: token replay detected';
      default:
        return 'Ended';
    }
  }

  $effect(() => {
    void load();
  });
</script>

<AppShell requireProject={false}>
  <div class="head">
    <div>
      <h1 class="page-title">Account</h1>
      <p class="sub muted">Your profile and the devices signed in to it.</p>
    </div>
    <RefreshButton onclick={() => void load()} loading={loading} />
  </div>

  {#if error}
    <div class="err-banner" role="alert">
      <Icon name="triangle-alert" size={15} />
      <span>{error}</span>
    </div>
  {/if}

  <div class="cards">
    <Card title="Profile">
      <dl class="profile">
        <dt>Name</dt>
        <dd>{authStore.user?.name || '—'}</dd>
        <dt>Email</dt>
        <dd class="cell-mono">{authStore.user?.email ?? '—'}</dd>
        <dt>Last sign-in</dt>
        <dd>{authStore.user?.last_login_at ? formatDateTime(authStore.user.last_login_at) : '—'}</dd>
      </dl>
      <div class="profile-actions">
        <Button variant="secondary" href="#/change-password">Change password</Button>
      </div>
    </Card>

    <Card title="Active sessions" padding="none">
      {#snippet actions()}
        <Button variant="ghost" size="sm" onclick={() => void toggleHistory()}>
          {showRevoked ? 'Hide recent sign-outs' : 'Show recent sign-outs'}
        </Button>
        <Button
          variant="danger"
          size="sm"
          disabled={otherCount === 0 || !hasCurrent}
          onclick={requestRevokeAll}
        >
          Sign out other devices
        </Button>
      {/snippet}

      {#if !hasCurrent && !loading && live.length > 0}
        <div class="err-banner inset" role="status">
          <Icon name="info" size={15} />
          <span>Reload the dashboard to manage your devices.</span>
        </div>
      {/if}

      {#if loading}
        <div class="center"><Spinner size={24} /></div>
      {:else if live.length === 0}
        <div class="pad">
          <EmptyState title="No active sessions" description="Sign in again to see this device." />
        </div>
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <th>Device</th>
              <th>IP</th>
              <th>Signed in</th>
              <th>Last used</th>
              <th aria-label="actions"></th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each live as s (s.id)}
              <tr>
                <td>
                  <span class="device">
                    {describeSession(s)}
                    {#if s.current}<Badge tone="primary" size="sm">This device</Badge>{/if}
                  </span>
                </td>
                <td class="cell-mono cell-muted">{s.ip ?? '—'}</td>
                <td title={formatDateTime(s.created_at)}>{relativeTime(s.created_at)}</td>
                <td title={formatDateTime(s.last_used_at)}>{relativeTime(s.last_used_at)}</td>
                <td class="col-act">
                  {#if !s.current}
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={!hasCurrent}
                      onclick={() => requestRevokeOne(s)}
                    >
                      Sign out
                    </Button>
                  {/if}
                </td>
              </tr>
            {/each}
            {#each revoked as s (s.id)}
              <tr class="dim">
                <td>{describeSession(s)}</td>
                <td class="cell-mono cell-muted">{s.ip ?? '—'}</td>
                <td title={formatDateTime(s.created_at)}>{relativeTime(s.created_at)}</td>
                <td title={s.revoked_at ? formatDateTime(s.revoked_at) : ''}>
                  Signed out {s.revoked_at ? relativeTime(s.revoked_at) : ''}
                </td>
                <td class="col-act cell-muted">{reasonLabel(s.revoked_reason)}</td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>

        {#if proxied}
          <p class="hint muted">
            All sessions show the same address — the API is behind a proxy and
            <code>API_TRUST_FORWARDED_HEADERS</code> is not set.
          </p>
        {/if}
      {/if}
    </Card>
  </div>
</AppShell>

<ConfirmDialog
  danger
  open={pending !== null}
  title={pending?.kind === 'all' ? 'Sign out other devices' : 'Sign out this device'}
  message={pending?.kind === 'all'
    ? 'Every device except this one will be signed out. You will stay logged in here.'
    : `${pending?.kind === 'one' ? pending.label : 'That device'} will be signed out within a few seconds and will have to log in again.`}
  confirmLabel="Sign out"
  loading={busy}
  onconfirm={() => void confirmPending()}
  oncancel={() => (pending = null)}
/>

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 18px;
  }
  .cards {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .profile {
    display: grid;
    grid-template-columns: 130px 1fr;
    row-gap: 8px;
    column-gap: 12px;
    margin: 0;
    font-size: 13.5px;
  }
  .profile dt {
    color: var(--text-faint);
  }
  .profile dd {
    margin: 0;
  }
  .profile-actions {
    margin-top: 16px;
  }
  .device {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .col-act {
    text-align: right;
    width: 1%;
    white-space: nowrap;
  }
  tr.dim td {
    opacity: 0.55;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 36px 0;
  }
  .pad {
    padding: 18px;
  }
  .hint {
    margin: 10px 16px 14px;
    font-size: 12px;
  }
  .err-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    margin-bottom: 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-2);
    font-size: 13px;
  }
  .err-banner.inset {
    margin: 14px 16px 0;
  }
</style>
```

- [ ] **Step 3: Register the route.** In `dashboard/src/routes.ts`, add `import Account from './pages/Account.svelte';` beside the other page imports, and add `'/account': guarded(Account as Component<never>),` to the `Settings` group of the `routes` object, directly above `'/members'`.

- [ ] **Step 4: Add the sidebar entry.** In `dashboard/src/lib/components/layout/Sidebar.svelte`, add to the `Manage` group's `items` array, as the **first** entry:

```ts
        { href: '#/account', label: 'Account', icon: 'user', match: (p) => p.startsWith('/account') },
```

**No `show:` gate** — every authenticated user has an account, unlike Members / Storage / Source Maps. `user` is already in `Icon.svelte`'s registry.

- [ ] **Step 5: Typecheck and see what the compiler says about the user shape.** Run `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`. If it reports that `last_login_at` is not on `User`, run `grep -n "interface User" -A 15 /home/splimter/projects/freelance/sauron/dashboard/src/lib/models/index.ts` and use the real field name (or drop that `<dt>/<dd>` pair if there is none). Expected end state: no new errors.

- [ ] **Step 6: Run the dashboard suite.** Run `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`. Expected: green.

- [ ] **Step 7: Drive it in a browser.** With the API running (Task 12 Step 7) and `cd /home/splimter/projects/freelance/sauron/dashboard && npm run dev`, open `http://localhost:3000/#/account` in two different browsers logged in as the same user. Confirm: both sessions listed, exactly one badged "This device" in each, correct device labels, the proxy hint line present when both rows share an address, and that "Sign out other devices" is enabled. Revoke one from the other and confirm the revoked browser is bounced to `#/login` on its next API call rather than showing a broken page — that exercises the 401 → `runRefreshOnce()` → refresh-401 → `onRefreshFailure()` chain end to end. Then press "Show recent sign-outs" and confirm the dimmed row reads "Signed out …" with a human reason.

---

### Task 19: The admin "Sign out" action on the Members table

**Files:**
- Modify `dashboard/src/lib/components/members/MembersTable.svelte` (`Props` interface, the `$props()` destructure, the `.row-actions` div at ~line 142)
- Modify `dashboard/src/pages/Members.svelte` (imports at lines 17-21, state at ~line 105, derived at ~line 143, handlers at ~line 300, `<MembersTable>` usage at line 404, dialog block at line 495)

**Interfaces:**
- Consumes: `revokeMemberSessions(orgId, userId)` (Task 18), `'member:credential'` in the `Permission` union (Task 3), `POST /v1/orgs/{org}/members/{user}/revoke-sessions` (Task 15).
- Produces: three new `MembersTable` props — `onrevokesessions: (member: Member) => void`, `revokingUserId: string | null`, `canRevokeSessions: boolean`.

The gate is a **new prop, not the existing `canManage`.** `Members.svelte` derives `canManage` from `sessionStore.can('member:manage')`, and reusing it would show a Sign out button to the holder of a custom role that has `member:manage` without `member:credential` — the exact role the carve-out exists to make possible — where every click 403s. The column header stays behind `canManage`; a role that can do neither still gets no actions column.

The button stays **inline**. `dashboard/src/lib/components/ui/` has fourteen components and none of them is a Menu primitive; building one properly (outside-click, focus trap, keyboard navigation, escape handling) is a real component, and doing it badly inside an auth slice is worse than three inline buttons. This takes the row from two buttons to three, which still fits. The password-reset slice builds the overflow menu, because its fourth action is where a row stops working.

- [ ] **Step 1: Add the three props to `MembersTable`.** In `dashboard/src/lib/components/members/MembersTable.svelte`, add to the `Props` interface after `togglingUserId: string | null;`:

```ts
    /** `member:credential`, NOT `canManage`. A custom role can hold
        `member:manage` without it — that is the whole point of the carve-out —
        and showing the button to that role means every click 403s. */
    canRevokeSessions: boolean;
    /** User id whose force-logout is in flight. */
    revokingUserId: string | null;
    onrevokesessions: (member: Member) => void;
```

and add `canRevokeSessions,`, `revokingUserId,`, `onrevokesessions,` to the `$props()` destructure in the same positions.

- [ ] **Step 2: Add the button.** In the `.row-actions` div, between the Edit button and the Deactivate/Reactivate button, insert:

```svelte
                  {#if canRevokeSessions && member.user_id !== authStore.user?.id}
                    <Button
                      size="sm"
                      variant="ghost"
                      loading={revokingUserId === member.user_id}
                      onclick={() => onrevokesessions(member)}
                    >
                      Sign out
                    </Button>
                  {/if}
```

Hidden for self because the backend 409s that case — the UI must not offer an action the server refuses. Add the import this needs at the top of the `<script>` block: `import { authStore } from '../../stores/auth.svelte';`.

- [ ] **Step 3: Typecheck and see it fail.** Run `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`. Expected failure: `Members.svelte` does not pass `canRevokeSessions`, `revokingUserId` or `onrevokesessions` to `<MembersTable>`.

- [ ] **Step 4: Wire the page.** In `dashboard/src/pages/Members.svelte`:

  Add the import beside the other API imports: `import { revokeMemberSessions } from '../lib/api/account';`

  Add state beside `deactivateTarget`:

```ts
  let revokingUserId = $state<string | null>(null);
  let pendingRevoke = $state<Member | null>(null);
```

  Add the derived permission beside `canManage`:

```ts
  // Deliberately not `canManage`: a custom role may hold `member:manage`
  // without `member:credential`, and showing this button to that role means
  // every click 403s.
  const canRevokeSessions = $derived(sessionStore.can('member:credential'));
```

  Add the handlers beside `confirmDeactivate`:

```ts
  function requestRevokeSessions(member: Member) {
    pendingRevoke = member;
  }

  async function confirmRevokeSessions() {
    const member = pendingRevoke;
    pendingRevoke = null;
    const org = sessionStore.currentOrg;
    if (!member || !org) return;
    revokingUserId = member.user_id;
    try {
      const n = await revokeMemberSessions(org.id, member.user_id);
      toastStore.success(
        `${member.email} was signed out of ${n === 1 ? '1 device' : `${n} devices`}.`,
      );
    } catch (err) {
      // The backend's 403/404/409s carry the actionable text (outranks you,
      // not a member here, belongs to another organization, self-target) —
      // surface it verbatim.
      toastStore.error(errorMessage(err));
    } finally {
      revokingUserId = null;
    }
  }
```

  Pass the three props on `<MembersTable>`, after `{togglingUserId}`:

```svelte
      {canRevokeSessions}
      {revokingUserId}
      onrevokesessions={requestRevokeSessions}
```

  And add a second dialog directly after the existing `{#if deactivateTarget}` block:

```svelte
  {#if pendingRevoke}
    <ConfirmDialog
      open
      title="Sign out all sessions"
      message={`${pendingRevoke.name || pendingRevoke.email} will be signed out on every device and will have to log in again. Their account stays active.`}
      confirmLabel="Sign out"
      danger
      onconfirm={() => void confirmRevokeSessions()}
      oncancel={() => (pendingRevoke = null)}
    />
  {/if}
```

**"Their account stays active" is load-bearing copy** — an admin reaching for this button is one click away from Deactivate and the two are easy to confuse.

- [ ] **Step 5: Typecheck and test.** Run `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check` — expected no new errors. Then `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test` — expected green.

- [ ] **Step 6: Drive it as two users.** With the API and dashboard running, log in as an Owner in one browser and as a plain member in another. From `#/members`, press Sign out on the member's row, confirm the dialog copy reads exactly as above, confirm the toast reports a device count, and confirm the member's browser is ejected to `#/login` within a few seconds. Then log in as a user holding a custom role with `member:manage` but **not** `member:credential` and confirm the Sign out button is absent while Edit and Deactivate remain.

---

### Task 20: Configuration, packaging, upgrade gate and docs

**Files:**
- Modify `/home/splimter/projects/freelance/sauron/.env.example`
- Modify `/home/splimter/projects/freelance/sauron/docker-compose.yml`
- Modify `/home/splimter/projects/freelance/sauron/packaging/rpm/config/api.env`
- Modify `/home/splimter/projects/freelance/sauron/README.md`
- Modify `/home/splimter/projects/freelance/sauron/packaging/rpm/SETUP.md`
- Modify `/home/splimter/projects/freelance/sauron/packaging/rpm/sauron.spec`
- Modify `/home/splimter/projects/freelance/sauron/dashboard/src/pages/Docs.svelte`

**Interfaces:**
- Consumes: `AUTH_REVOCATION_POLL_SECS` (Task 10), migration 000035 (Task 1), the measured migration runtime from Task 1 Step 4.
- Produces: no code interfaces.

- [ ] **Step 1: Add the key to `.env.example`.** In `/home/splimter/projects/freelance/sauron/.env.example`, in the `# --- Auth ---` section directly after the `JWT_SECRET=` line, add:

```
# How long a revoked session can still be used, in seconds — this is the real kill
# latency. Each API replica refreshes its revoked-session snapshot on this interval.
# Clamped to 1..60 at the use site.
AUTH_REVOCATION_POLL_SECS=5
```

- [ ] **Step 2: Add it to `docker-compose.yml`.** In the `api:` service's `environment:` block, directly after `JWT_REFRESH_TTL_SECS: "2592000"`, add:

```yaml
      AUTH_REVOCATION_POLL_SECS: "5"
```

- [ ] **Step 3: Add it to the RPM config.** In `/home/splimter/projects/freelance/sauron/packaging/rpm/config/api.env`, directly after `JWT_REFRESH_TTL_SECS=2592000`, add:

```
# How long a revoked session can still be used, in seconds — this is the real
# kill latency. Clamped to 1..60.
AUTH_REVOCATION_POLL_SECS=5
```

`packaging/rpm/systemd/sauron-api.service` needs no change; it already loads `/etc/sauron/api.env`. `packaging/rpm/binaries.txt` is unchanged — this slice adds no binary, because the poller is a supervised task inside `sauron-api`.

- [ ] **Step 4: Add the README row.** In `/home/splimter/projects/freelance/sauron/README.md`, directly after the `JWT_REFRESH_TTL_SECS` row, add:

```
| `AUTH_REVOCATION_POLL_SECS` | How often each API replica refreshes its revoked-session snapshot. **This is the real kill latency**: a session ended by a logout, a "sign out other devices", an admin force-logout, a deactivation or a password change stops working on a replica at its next poll. Clamped to `1`-`60`. | `5` | api |
```

- [ ] **Step 5: Add the upgrade gate.** In `/home/splimter/projects/freelance/sauron/packaging/rpm/SETUP.md`, check whether a `## 11. Upgrading` section already exists (`grep -n "^## 11" packaging/rpm/SETUP.md`). **If it does not**, append this section after `## 10. Troubleshooting`; **if it does**, append only the table row and the note to it.

````markdown
## 11. Upgrading

RPM upgrades install new binaries but **do not run migrations**:
`sauron-migrate.service` has no `[Install]` section, and `%postun server`
restarts the API. So every upgrade is:

```
systemctl stop sauron-api sauron-ingest
systemctl start sauron-migrate     # 000035 locks refresh_tokens; schedule it
systemctl start sauron-api sauron-ingest
```

| Migration | What breaks if it is skipped |
| --- | --- |
| `000035_auth_sessions` | **Total authentication outage.** Without `auth_sessions` and `refresh_tokens.session_id`, `start_or_continue_session` fails on *every* login, register, refresh and password change — not a degraded feature, and on the exact path an operator would use to diagnose it. |

**000035 needs a maintenance window.** It holds `AccessExclusiveLock` on
`refresh_tokens` across an `ADD COLUMN`, a full-table backfill and a non-partial
index build, in one transaction, on the table that authenticates every request.
`CONCURRENTLY` is unavailable inside a migration transaction, so it cannot be
softened. Measured runtime on the reference dataset: **REPLACE WITH THE NUMBER
FROM TASK 1 STEP 4**.
````

Replace the bold placeholder with the real timing from Task 1 Step 4 before finishing this step — a runbook with an unfilled placeholder is worse than one without the line. Then prove it: run `grep -n 'REPLACE WITH' /home/splimter/projects/freelance/sauron/packaging/rpm/SETUP.md`. Expected: **no hits** (grep exits 1). This is a shipped operator runbook; nothing else in the plan's green gate reads it.

- [ ] **Step 6: Add the spec changelog entry.** In `/home/splimter/projects/freelance/sauron/packaging/rpm/sauron.spec`, insert a new entry at the **top** of `%changelog`, following the in-repo convention:

```
* Sat Aug 01 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.2.0-1
- Session management: a login now has an identity that survives refresh-token
  rotation. Users can see and end their own sessions from the new Account page;
  admins with the new member:credential permission can sign a member out of every
  device from Members.
- Revoking anything (logout, sign-out, deactivation, password change, replay
  detection) now takes effect within AUTH_REVOCATION_POLL_SECS (default 5) instead
  of up to JWT_ACCESS_TTL_SECS.
- New auth_sessions table and refresh_tokens.session_id. Migration 000035 takes an
  AccessExclusiveLock on refresh_tokens for the duration — schedule a window, and
  run sauron-migrate after upgrading or authentication will fail outright.
```

- [ ] **Step 7: Fix the now-wrong sentence in the docs page and document the account surface.** In `dashboard/src/pages/Docs.svelte`, in the `id="rbac"` section, the deactivation paragraph currently ends: *"Their refresh tokens are revoked immediately, though an access token already issued keeps working until it expires (up to 15 minutes by default)."* That statement is now false. Replace that sentence with:

```
              Their sessions are revoked immediately, and any access token already
              issued stops working within a few seconds — every API replica
              refreshes its revoked-session list on the
              <code class="ic">AUTH_REVOCATION_POLL_SECS</code> interval (5 seconds
              by default).
```

Then, at the end of the same `<Card>` (after the custom-roles paragraph), add:

```
            <p class="muted concept-lead">
              Every signed-in user has an <b>Account</b> page listing the devices their
              account is signed in on — device, address, when they signed in and when the
              session was last used. The session they are currently using is badged
              <b>This device</b> and cannot be signed out from there; <b>Log out</b> in the
              top bar is that verb. <b>Sign out other devices</b> ends every session but
              the current one. "Show recent sign-outs" reveals the last 30 days of ended
              sessions with the reason each one ended, which is how a user learns that
              something other than themselves closed a session.
            </p>
            <p class="faint fine">
              An admin holding <code class="ic">member:credential</code> can sign a member
              out of every device from the Members page. That permission is carved out of
              <code class="ic">member:manage</code> rather than added beside it, so a role
              can administer membership without also holding the verbs that act on someone's
              credentials. The admin surface deliberately shows no per-device detail —
              only the one coarse verb. Signing someone out does not deactivate them and
              does not force a password change.
            </p>
```

Also update the `<p class="muted concept-lead">` lead sentence in the same section that reads "**23 atomic permissions**" — count the real number after Task 3 with `grep -c "pub const [A-Z_]*: &str" backend/crates/sauron-auth/src/rbac.rs` scoped to the `perm` module, and use `perm::ALL`'s declared length (28) instead. It has been wrong since before this slice; fix it while the file is open.

- [ ] **Step 8: Verify every new key is documented in all four places.** Run:

```
grep -rn "AUTH_REVOCATION_POLL_SECS" \
  /home/splimter/projects/freelance/sauron/.env.example \
  /home/splimter/projects/freelance/sauron/docker-compose.yml \
  /home/splimter/projects/freelance/sauron/packaging/rpm/config/api.env \
  /home/splimter/projects/freelance/sauron/README.md \
  /home/splimter/projects/freelance/sauron/backend/crates/sauron-core/src/config.rs
```

Expected: at least one hit in each of the five files. Then re-run the runbook placeholder check from Step 5: `grep -n 'REPLACE WITH' /home/splimter/projects/freelance/sauron/packaging/rpm/SETUP.md` — expected no hits.

- [ ] **Step 9: Full green gate.** Run, in order:
  1. `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` — expected: no output.
  2. `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings` — expected: clean.
  3. `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test --workspace -- --test-threads=2` — expected: green.
  4. `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check` — expected: no new errors.
  5. `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test` — expected: green.
  6. `cd /home/splimter/projects/freelance/sauron/dashboard && npm run build` — expected: succeeds.

---

## Hand-off notes for the password-reset slice (S1)

Not work for this plan — recorded here because S1 lands next and depends on decisions made above.

- S1 adds two reason constants, `REVOKE_PASSWORD_RESET = "password_reset"` and `REVOKE_RESET_FORCED = "reset_forced"`. **Both values are already in `auth_sessions_revoked_reason_check` (Task 1), so S1 ships no widening migration of its own** — that is deliberate, because S1's unauthenticated reset path would otherwise 500 on a user who has just proved they cannot get into their account.
- S1 owns two coupled edits: the constants, and their membership in `DELIBERATE_REVOKE_REASONS`, which grows to `[&str; 5]`. Missing the second edit sends the target's still-live refresh token into the theft branch about fifteen minutes later and fires a family-wide kill. `every_revocation_reason_is_classified` (Task 5) fails loudly if a new constant is added without choosing a bucket — extend `ALL_REASONS` there too.
- S1's admin reset **must** call `repo::revoke_sessions_for_user`, never the session-blind mass-revoke helpers; the pin test in Task 14 enforces it.
- S1 reuses `guard_member_admin_action` (Task 13) with `allow_self: false`, `perm::MEMBER_CREDENTIAL` (Task 3), `pub(crate) rate_limit` / `client_addr` (Task 8), `routes/account.rs` (Task 12) and the `#/account` page (Task 18).
- S1 builds `dashboard/src/lib/components/ui/RowActionsMenu.svelte` and folds this slice's inline "Sign out" button into it, in the order Edit / Reset password / Sign out all devices / Deactivate.

