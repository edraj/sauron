# PII Inspector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give an org admin a way to find developer-supplied PII in Sauron's telemetry jsonb columns, prove what was found without storing a second copy of it, irreversibly mask it in hot Postgres, and enforce the mask on all future ingest — while saying out loud, everywhere, that the mask does not reach cold Parquet, Redis or already-delivered alerts.

**Architecture:** A new pure Rust crate `sauron-inspector` owns every decision (the column allowlist, the JSON walker, key matching, value detectors, redaction, the SQL prefilter, path grammar, mask targets and the pure mask applier) and has no database or HTTP dependency, so all of it is unit-tested without Postgres. A new worker binary `sauron-inspector` runs four independent claim-based loops (scheduler, scan executor, mask executor, preview executor) plus an hourly reaper against its own 4-connection pool; all its SQL lives in `sauron-db`'s `repo.rs`. Forward enforcement lives in `sauron-pipeline` behind a 30-second per-app cache, the API exposes 17 routes gated on two new permissions, and the dashboard renders a four-tab Inspector page.

**Tech Stack:** Rust 1.82 (MSRV), diesel 2.3 + diesel-async + Postgres, `serde_json`, `tokio`, `axum` 0.8, `chrono`, `uuid`, Svelte 5 runes + vitest (node-only, no DOM). **No new workspace dependency** — `regex`, `chrono-tz`, `cron` and `csv` are all deliberately avoided; the walker, detectors, redaction, scheduling arithmetic and CSV escaping are hand-rolled.

## Global Constraints

- **NEVER add a git commit, `git add`, or branch step.** The repository owner commits manually.
- **Prerequisite:** this slice lands after S0–S4 of the notifications/security/analytics programme. It consumes, and must not rebuild: `backend/bins/sauron-api/src/csv.rs` (S4's RFC 4180 writer + formula-injection guard), `dashboard/src/lib/api/download.ts` (S4), the CORS `.expose_headers([CONTENT_DISPOSITION])` line in `backend/bins/sauron-api/src/main.rs` (S4), `pub(crate) rate_limit` / `pub(crate) client_addr` in `backend/bins/sauron-api/src/routes/auth.rs` (S2), and `repo::live_enrollments_for_apps` (S3). If any is absent, stop and report it rather than writing a second copy.
- **Never use `conn.transaction(...)`.** The MSRV blocks it. Multi-statement atomicity is one data-modifying CTE via `diesel::sql_query` with `.bind()`.
- **`backend/crates/sauron-db/src/schema.rs` is HAND-MAINTAINED. The diesel CLI must NEVER run.** A new table means hand-editing three places: a `diesel::table!` block, a `diesel::joinable!` line per non-nullable FK, and the name in `allow_tables_to_appear_in_same_query!`. This slice's delta is **exactly +6** `diesel::table!` blocks; assert the delta, never an absolute count.
- Migrations are `backend/migrations/YYYY-MM-DD-0000NN_slug/{up,down}.sql`, **BOTH files required**, `up.sql` opening with a prose comment explaining WHY. A migration runs in ONE transaction; `CONCURRENTLY` is unavailable; an index build on a partitioned parent locks every child.
- This slice owns migrations **000041, 000042, 000043**, in that order, all dated `2026-08-01`. Diesel orders by the full `YYYY-MM-DD-0000NN` string, date first — the date prefix is the landing date and must never decrease as NN increases.
- **Enum-like columns are TEXT + CHECK, never custom SQL types.**
- **Never write `NULLS NOT DISTINCT`.** It raises the Postgres floor to 15; `run_pending_migrations` stops at the first failure, so on a PG13 host every later migration in the product is permanently blocked. Use a `COALESCE(col, '00000000-0000-0000-0000-000000000000'::uuid)` expression index instead (PG11+).
- **All SQL lives in `backend/crates/sauron-db/src/repo.rs`** as free `pub async fn name(conn: &mut AsyncPgConnection, ...) -> QueryResult<T>`. Handlers never build queries inline.
- **Insertable-only structs must NOT gain a `Queryable` derive** — `Queryable` decodes positionally and would silently bind fields to the wrong columns.
- **Never hold a pooled `PgConn` across network I/O.** The API pool is 16 connections for the whole process; `drop(conn)` first. The inspector gets its own `build_pool(url, 4)` and never touches the API's.
- **Claim-based concurrency only.** There are zero advisory locks in the repository and this slice introduces none. Every claim is `FOR UPDATE SKIP LOCKED`, optionally fenced on a `worker_id` and a lease.
- **Config never `bail!`s.** `Config::from_env` is shared by every binary; every new field defaults.
- Dashboard: house UI components only (there is **NO** Select, Toggle, Tabs or Menu primitive). A new page needs three edits: the page file, `src/routes.ts`, and the `Sidebar.svelte` `groups` array. Pure decision logic goes in `src/lib/models/*.ts` with a colocated `*.test.ts`, because there is **NO DOM test environment**.
- Svelte 5 runes. `$state` deep-proxies values so `===` never matches a raw value; use `$state.raw` when identity matters. Sets and Records in `$state` are replaced, never mutated in place.
- Comments explain the failure mode that motivated the code, not what the code does.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` are hard gates.

### Verbatim commands

- **Rust check:** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`
- **Rust unit test:** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p <crate> <testname>`
- **Rust fmt:** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check`
- **Rust clippy:** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
- **Postgres-backed tests** (harness returns `None` and SKIPS when unset): prefix with `TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379`
- **Apply migrations:** `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`
- **Dashboard tests:** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
- **Dashboard typecheck:** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`

## File Structure

### Created

| Path | Responsibility |
|---|---|
| `backend/migrations/2026-08-01-000041_pii_perms/{up,down}.sql` | Grant `pii:read`/`pii:manage` to custom roles that hold `org:manage` **and** sit only at org scope |
| `backend/migrations/2026-08-01-000042_inspector_scan/{up,down}.sql` | `inspector_policies`, `inspector_scans`, `inspector_findings` |
| `backend/migrations/2026-08-01-000043_inspector_mask_audit/{up,down}.sql` | `inspector_mask_actions`, `inspector_masked_keys`, `inspector_reveal_audit` |
| `backend/crates/sauron-inspector/Cargo.toml` | Pure crate manifest — no diesel, no axum, no tokio |
| `backend/crates/sauron-inspector/src/lib.rs` | Module wiring + re-exports |
| `backend/crates/sauron-inspector/src/columns.rs` | The `&'static` scan-column inventory and the closed table allowlist |
| `backend/crates/sauron-inspector/src/walk.rs` | Depth-capped `serde_json` walker producing `(path, key, value)` leaves |
| `backend/crates/sauron-inspector/src/match.rs` | Tracked-key normalization and case-insensitive exact leaf-key matching |
| `backend/crates/sauron-inspector/src/detect.rs` | The eight hand-rolled value-shape detectors (incl. Luhn) |
| `backend/crates/sauron-inspector/src/redact.rs` | Shape-only value preview **and** `key_path` redaction |
| `backend/crates/sauron-inspector/src/prefilter.rs` | ILIKE escaping and phase-1 pattern construction |
| `backend/crates/sauron-inspector/src/path.rs` | Mask-path grammar: parse, validate, finding-path → mask-path |
| `backend/crates/sauron-inspector/src/targets.rs` | `TargetTable`/`TargetColumn` enums, `MaskTarget`, `expand_targets`, `resolve_targets` |
| `backend/crates/sauron-inspector/src/mask.rs` | The pure mask applier over `serde_json::Value` |
| `backend/crates/sauron-inspector/src/units.rs` | `Unit`, `units_for`, `tables_for` — the decomposition the scheduler AND the API both freeze from |
| `backend/bins/sauron-inspector/Cargo.toml` | Package `sauron-inspector-bin`, `[[bin]] name = "sauron-inspector"` |
| `backend/bins/sauron-inspector/src/main.rs` | Four loops, one 4-connection pool, statement-timeout wrapper |
| `backend/bins/sauron-inspector/src/scan.rs` | Scan executor: phase-1/phase-2, flush (decomposition lives in `sauron-inspector::units`) |
| `backend/bins/sauron-inspector/src/mask.rs` | Retro-mask executor: day loop, `_default` phase, companions, tail sweep |
| `backend/bins/sauron-inspector/src/preview.rs` | Preview executor: the identical day loop with `count(*)` |
| `backend/bins/sauron-inspector/src/reap.rs` | Retention + pseudonymization |
| `backend/crates/sauron-pipeline/src/mask.rs` | `PolicyCache`, `apply_wire`, `apply_context` |
| `backend/bins/sauron-api/src/routes/inspector.rs` | All 17 routes |
| `packaging/rpm/systemd/sauron-inspector.service` | systemd unit |
| `packaging/rpm/config/inspector.env` | Inspector-only env keys |
| `dashboard/src/pages/Inspector.svelte` | Four-tab page: Findings / Policy / Scans / Audit |
| `dashboard/src/lib/components/inspector/MaskDialog.svelte` | Preview → unreachable-copy panel → typed-slug confirm |
| `dashboard/src/lib/api/inspector.ts` | One exported async fn per endpoint |
| `dashboard/src/lib/models/inspector.ts` + `.test.ts` | `describeTarget`, `expandCompanionTargets`, `maskConfirmReady`, `UNREACHABLE_COPY`, `csvFilename` |
| `dashboard/src/lib/models/inspector-schedule.ts` + `.test.ts` | Weekday bitmask ↔ checkboxes, human description, next-3-runs preview |
| `dashboard/src/lib/models/inspector-findings.ts` + `.test.ts` | Grouping/sorting, "at least N", badge logic |
| `dashboard/src/lib/constants/inspectorSchedules.ts` | Weekday labels + timezone presets, mirroring the backend |
| `wiki/Privacy-Inspector.md` | The §1 list verbatim, the sticky-title regression, the audit-CSV trade |

### Modified

| Path | Change |
|---|---|
| `backend/crates/sauron-auth/src/rbac.rs` | Two `pub const`s, `perm::ALL` 28→30, Admin preset bag, five test assertions |
| `backend/crates/sauron-db/src/schema.rs` | +6 `table!` blocks, 9 `joinable!` lines, 6 `allow_tables_to_appear_in_same_query!` entries |
| `backend/crates/sauron-db/src/models.rs` | Row struct + `New…<'a>` insert struct per new table |
| `backend/crates/sauron-db/src/repo.rs` | Policy CRUD, scheduling, `enqueue_scan_for_policy` (the one scan-freeze both callers use), scan claim/flush, the three-shape phase-1 read, findings, reveal, mask claim/batch, masked keys, reapers, `upsert_issue` sticky guard |
| `backend/crates/sauron-db/Cargo.toml` | `sauron-inspector` dependency, so the batch signatures take enums and the enqueue can call `units_for` |
| `backend/crates/sauron-core/src/config.rs` | The `// --- pii inspector ---` section |
| `backend/crates/sauron-pipeline/src/{lib.rs,worker.rs,process.rs}` | `mod mask;`, the two application sites, the masked dead-letter payload |
| `backend/bins/sauron-ingest/src/main.rs` | Build the `PolicyCache` and hand it to `spawn_workers` |
| `backend/bins/sauron-api/src/main.rs` | 17 routes |
| `backend/bins/sauron-api/src/routes/{mod.rs,orgs.rs}` | `pub mod inspector;`; cancel a deactivated member's pending mask actions |
| `backend/bins/sauron-api/tests/http_env_scoping.rs` | Three new app-scoped GETs stay green |
| `backend/Cargo.toml` | `sauron-inspector` workspace dependency entry |
| `dashboard/src/lib/api/scope.ts` | Three regexes into `BACKEND_REJECTS_ENVIRONMENT_ID` (the three app-scoped GETs only — see Task 29) |
| `dashboard/src/lib/models/{index.ts,permissions.ts}` | `Permission` union + inspector response types; `ALL_PERMISSIONS`, `PERMISSION_GROUPS`, `PERMISSION_LABELS` |
| `dashboard/src/{routes.ts,lib/components/layout/Sidebar.svelte,lib/components/ui/Icon.svelte}` | Route entry, nav item, two icons |
| `dashboard/src/pages/Docs.svelte` | Document the flow |
| `packaging/rpm/{binaries.txt,sauron.spec,build-rpm.sh,SETUP.md}` | Ship the new binary, unit and config |
| `packaging/rpm/config/{sauron.env,tier.env}` | Move `TIER_HOT_DAYS` into `sauron.env`; add the two shared inspector keys |
| `docker-compose.yml`, `.env.example`, `README.md` | The `inspector` service, 25 documented keys, `max_connections=200` |

---

## Task 1: Permissions `pii:read` / `pii:manage`, migration 000041, and the four mirrors

**Files:**
- Modify `backend/crates/sauron-auth/src/rbac.rs` (perm module ~lines 28–96; `ADMIN` bag ~lines 111–142; tests ~lines 806–910)
- Create `backend/migrations/2026-08-01-000041_pii_perms/up.sql`
- Create `backend/migrations/2026-08-01-000041_pii_perms/down.sql`
- Modify `dashboard/src/lib/models/permissions.ts`
- Modify `dashboard/src/lib/models/index.ts` (`Permission` union, ~lines 182–210)

**Interfaces:**
- Consumes: nothing.
- Produces: `sauron_auth::perm::PII_READ: &str = "pii:read"`, `sauron_auth::perm::PII_MANAGE: &str = "pii:manage"`, `perm::ALL: [&str; 30]`.

> All five edits land in **one** task because `dashboard/src/lib/models/permissions.test.ts` parses `rbac.rs` and fails on drift — a mirror that lands a task later fails CI for the task that did nothing wrong. And `RoleEditorDialog` submits the full checkbox state, so a permission missing from `ALL_PERMISSIONS` is silently stripped from any role that has it on first save.

- [ ] **Step 1: Read the real starting numbers.** Run `cd /home/splimter/projects/freelance/sauron/backend && grep -n 'pub const ALL: \[&str;' crates/sauron-auth/src/rbac.rs`. Expected after S2: `pub const ALL: [&str; 28]`. Record the number; every literal below is that number + 2. If it reads 27, S2 has not landed — stop and report.

- [ ] **Step 2: Write the failing assertions first.** In `backend/crates/sauron-auth/src/rbac.rs`, change the four existing count literals inside `mod tests`: in `owner_has_every_permission` `assert_eq!(OWNER.permissions.len(), 28);` → `30`; in `admin_is_all_except_org_manage` `assert_eq!(ADMIN.permissions.len(), 27);` → `29`; in `all_permissions_are_unique` `assert_eq!(perm::ALL.len(), 28);` → `30`. Leave `developer_can_write_issues_not_manage_members`'s `18` and `viewer_is_read_only`'s `7` **untouched** — re-read both bags by eye to confirm neither gains the pair.

- [ ] **Step 3: Add the new test that pins the presets.** Append to `mod tests` in `rbac.rs`:
  ```rust
  /// A count assertion that still passes is no evidence the pair landed in the
  /// right presets: the temptation on a red count is to edit the number until
  /// the suite is green, which is exactly how `pii:manage` ends up in Developer.
  #[test]
  fn pii_permissions_are_owner_and_admin_only() {
      for p in [perm::PII_READ, perm::PII_MANAGE] {
          assert!(OWNER.permissions.contains(&p), "Owner missing {p}");
          assert!(ADMIN.permissions.contains(&p), "Admin missing {p}");
          assert!(!DEVELOPER.permissions.contains(&p), "Developer must not hold {p}");
          assert!(!VIEWER.permissions.contains(&p), "Viewer must not hold {p}");
          assert!(perm::ALL.contains(&p), "{p} missing from perm::ALL");
      }
  }
  ```

- [ ] **Step 4: Run it and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-auth pii_permissions_are_owner_and_admin_only`. Expected: compile error `error[E0599]: no function or associated item named 'PII_READ' found` (or `cannot find value PII_READ in module perm`).

- [ ] **Step 5: Add the constants.** In `rbac.rs`'s `pub mod perm`, after `ALERT_WRITE`:
  ```rust
  /// Read PII scan findings, scans, masked-key lists and the mask audit trail,
  /// and reveal a single raw value. Bulk PII disclosure — Owner and Admin only.
  pub const PII_READ: &str = "pii:read";
  /// Create/edit inspector policies, run scans, and execute an irreversible
  /// mask. Bulk destruction — Owner and Admin only, never inherited by the role
  /// every engineer gets by default.
  pub const PII_MANAGE: &str = "pii:manage";
  ```
  Then change `pub const ALL: [&str; 28]` to `pub const ALL: [&str; 30]` and append `PII_READ,` and `PII_MANAGE,` after `ALERT_WRITE,` in the array body.

- [ ] **Step 6: Add both to the Admin bag.** In `pub const ADMIN`, after `perm::ALERT_WRITE,` add `perm::PII_READ,` and `perm::PII_MANAGE,`. Owner picks them up through `&perm::ALL` with no edit. Do **not** touch `DEVELOPER` or `VIEWER`.

- [ ] **Step 7: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-auth`. All of `owner_has_every_permission`, `admin_is_all_except_org_manage`, `all_permissions_are_unique`, `roles_form_a_strict_ladder`, `developer_can_write_issues_not_manage_members`, `viewer_is_read_only` and the new test pass.

- [ ] **Step 8: Write migration 000041 `up.sql`.** Create `backend/migrations/2026-08-01-000041_pii_perms/up.sql`:
  ```sql
  -- Grant the new pii:read / pii:manage pair to CUSTOM roles that already hold
  -- org:manage. Preset roles need no UPDATE — `ensure_preset_roles` re-syncs
  -- them from rbac.rs at every API boot.
  --
  -- The NOT EXISTS clause is the whole point. `org:manage` is INERT outside org
  -- scope (`authorize_org` only ever accepts an org grant), so a custom role
  -- holding it that happens to be granted at app scope is harmless today.
  -- `pii:manage` is enforced by `authorize_app`, so it is fully live at app
  -- scope. Granting the pair on the permission predicate alone would silently
  -- promote those holders to irreversible bulk destruction of one app's data.
  --
  -- The condition is evaluated once. A role with zero grants qualifies and could
  -- later be granted at app scope — but only by someone who already holds
  -- pii:manage, because `create_grant`'s escalation check requires it.
  UPDATE roles SET permissions = permissions || '["pii:read","pii:manage"]'::jsonb
  WHERE org_id IS NOT NULL
    AND jsonb_typeof(permissions) = 'array'
    AND permissions @> '["org:manage"]'::jsonb
    AND NOT permissions @> '["pii:read"]'::jsonb
    AND NOT EXISTS (
      SELECT 1 FROM role_grants g WHERE g.role_id = roles.id AND g.scope_type <> 'org'
    );
  ```

- [ ] **Step 9: Write migration 000041 `down.sql`.** Create `backend/migrations/2026-08-01-000041_pii_perms/down.sql`:
  ```sql
  -- Leaving pii:* on a custom role after the code is reverted makes that role
  -- permanently ungrantable: the grant path requires the caller to hold every
  -- permission in the role, and nobody can hold one that is no longer in
  -- perm::ALL. Presets re-sync from code at boot; custom roles do not.
  UPDATE roles
  SET permissions = permissions - 'pii:read' - 'pii:manage'
  WHERE jsonb_typeof(permissions) = 'array'
    AND permissions ?| array['pii:read','pii:manage'];
  ```

- [ ] **Step 10: Apply and verify the migration.** Run `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`. Expected: it logs the new migration applied with no error. Then `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "SELECT name, permissions @> '[\"pii:read\"]'::jsonb AS has_pii FROM roles ORDER BY name"` and confirm no role granted outside org scope gained the pair.

- [ ] **Step 11: Mirror into `ALL_PERMISSIONS`.** In `dashboard/src/lib/models/permissions.ts`, append to `ALL_PERMISSIONS` after `'alert:write',`:
  ```ts
  'pii:read',
  'pii:manage',
  ```

- [ ] **Step 12: Mirror into groups and labels.** In the same file, append a new group after the `Alerting` entry of `PERMISSION_GROUPS`:
  ```ts
  { label: 'Privacy', permissions: ['pii:read', 'pii:manage'] },
  ```
  and append to `PERMISSION_LABELS`:
  ```ts
  'pii:read': 'View PII scan findings, the mask audit trail, and reveal single values',
  'pii:manage': 'Configure scans and permanently mask values (irreversible)',
  ```

- [ ] **Step 13: Extend the `Permission` union.** In `dashboard/src/lib/models/index.ts`, in `export type Permission =`, insert `| 'pii:read'` and `| 'pii:manage'` immediately before the trailing `| (string & {});`.

- [ ] **Step 14: Run the dashboard suite.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`. Expected: `permissions.test.ts` passes — it parses `rbac.rs` and asserts the two lists agree in content and order. Then `npm run check` for the type union.

- [ ] **Step 15: Format and lint the backend.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Both clean.

---

## Task 2: Migration 000042 — policies, scans, findings — plus schema.rs and models.rs

**Files:**
- Create `backend/migrations/2026-08-01-000042_inspector_scan/{up,down}.sql`
- Modify `backend/crates/sauron-db/src/schema.rs` (three `table!` blocks; `joinable!` block near line 469; `allow_tables_to_appear_in_same_query!` near line 503)
- Modify `backend/crates/sauron-db/src/models.rs` (append at end of file)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `sauron_db::schema::{inspector_policies, inspector_scans, inspector_findings}`; `sauron_db::models::{InspectorPolicy, NewInspectorPolicy, InspectorPolicyPatch, InspectorScan, NewInspectorScan, InspectorFinding}`.

- [ ] **Step 1: Write `up.sql`.** Create `backend/migrations/2026-08-01-000042_inspector_scan/up.sql`:
  ```sql
  -- The PII inspector's read side: where inspection is switched on, when it
  -- runs, one row per run, and the aggregated result.
  --
  -- `inspector_findings` deliberately has NO raw-value column and NO hash
  -- column. A findings table that keeps sample values is a second, longer-lived,
  -- more concentrated copy of the PII in a table nobody tiers — strictly worse
  -- than the original. And a SHA-256 of an email is a stable pseudonymous
  -- identifier of a person, trivially brute-forced for low-entropy values, so
  -- "just hash it" is not a mitigation. A locator plus a shape-only preview is
  -- everything an admin needs to decide.
  --
  -- `target_type` is NOT named `scope_type`: dashboard/src/lib/models/
  -- scope-type.test.ts parses the newest `CHECK (scope_type IN (...))` out of
  -- this directory and asserts it equals ['app','env','org','project']. A new
  -- column with that name fails that test.
  --
  -- `scan_columns` is NOT named `columns`: diesel_derives emits `pub mod
  -- columns` inside every generated table module and re-exports it, so a column
  -- named `columns` produces `error[E0573]: expected type, found module` on the
  -- table! block AND on every #[diesel(table_name = ...)] derive.

  CREATE TABLE inspector_policies (
      id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
      -- Denormalized tenant key, same as alert_rules: list queries and the
      -- reaper must never join upward to find the org.
      org_id            UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
      target_type       TEXT NOT NULL CHECK (target_type IN ('project','app','app_env')),
      -- Polymorphic, no FK (matches role_grants). For 'app_env' this holds an
      -- app_environments.id — the ENROLLMENT id, never a catalogue
      -- environments.id. Event rows store the enrollment id, so the other one
      -- would silently match nothing.
      target_id         UUID NOT NULL,
      enabled           BOOL NOT NULL DEFAULT TRUE,
      -- [{key, scope:'any'|'top'}], key lowercased at write.
      tracked_keys      JSONB NOT NULL DEFAULT '[]'::jsonb,
      -- Preset detector ids from a &'static list in sauron-inspector.
      detectors         JSONB NOT NULL DEFAULT '[]'::jsonb,
      -- NULL = the default column set from the inventory.
      scan_columns      JSONB,
      rollups           JSONB NOT NULL DEFAULT '["issues","event_users"]'::jsonb,
      window_days       INT NOT NULL DEFAULT 30 CHECK (window_days BETWEEN 1 AND 400),
      schedule_enabled  BOOL NOT NULL DEFAULT FALSE,
      -- Bit N = EXTRACT(DOW) = N, so Sunday is bit 0.
      schedule_days     SMALLINT NOT NULL DEFAULT 0 CHECK (schedule_days BETWEEN 0 AND 127),
      schedule_time     TIME NOT NULL DEFAULT '03:00',
      -- IANA name, validated at write with `SELECT now() AT TIME ZONE $1`.
      schedule_tz       TEXT NOT NULL DEFAULT 'UTC',
      -- Materialized due time; the monitors.next_check_at pattern.
      next_run_at       TIMESTAMPTZ,
      last_run_at       TIMESTAMPTZ,
      last_scan_id      UUID,
      last_skip_reason  TEXT NOT NULL DEFAULT '',
      created_by        UUID REFERENCES users(id) ON DELETE SET NULL,
      created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
      updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
  );
  -- One policy per node is what makes precedence a database fact rather than an
  -- ordering problem.
  CREATE UNIQUE INDEX inspector_policies_target_key ON inspector_policies (target_type, target_id);
  CREATE INDEX inspector_policies_org_idx ON inspector_policies (org_id);
  CREATE INDEX inspector_policies_due_idx ON inspector_policies (next_run_at)
      WHERE enabled AND schedule_enabled;

  CREATE TABLE inspector_scans (
      id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
      policy_id           UUID NOT NULL REFERENCES inspector_policies(id) ON DELETE CASCADE,
      org_id              UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
      trigger_type        TEXT NOT NULL CHECK (trigger_type IN ('scheduled','manual')),
      requested_by        UUID REFERENCES users(id) ON DELETE SET NULL,
      status              TEXT NOT NULL DEFAULT 'queued'
                            CHECK (status IN ('queued','running','succeeded','failed','cancelled')),
      -- Kept separate from `status` so a completed-but-incomplete scan is not
      -- mistaken for a failure.
      coverage            TEXT NOT NULL DEFAULT 'full' CHECK (coverage IN ('full','partial')),
      coverage_note       TEXT NOT NULL DEFAULT '',
      window_from         TIMESTAMPTZ NOT NULL,
      window_to           TIMESTAMPTZ NOT NULL,
      -- Frozen copies of tracked_keys/detectors/scan_columns/rollups. The unit
      -- list is recomputed from these on resume, so an admin editing the policy
      -- mid-scan must not be able to change what unit #37 means.
      params              JSONB NOT NULL,
      -- Resolved ordered [(app_id, app_env_id|null)] pairs, capped at 2000.
      targets             JSONB NOT NULL,
      units_total         INT NOT NULL DEFAULT 0,
      units_done          INT NOT NULL DEFAULT 0,
      cursor              JSONB NOT NULL DEFAULT '{}'::jsonb,
      rows_scanned        BIGINT NOT NULL DEFAULT 0,
      findings_count      INT NOT NULL DEFAULT 0,
      findings_reaped_at  TIMESTAMPTZ,
      worker_id           TEXT,
      heartbeat_at        TIMESTAMPTZ,
      attempts            INT NOT NULL DEFAULT 0,
      cancel_requested_at TIMESTAMPTZ,
      error               TEXT NOT NULL DEFAULT '',
      started_at          TIMESTAMPTZ,
      finished_at         TIMESTAMPTZ,
      created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
  );
  CREATE INDEX inspector_scans_policy_idx ON inspector_scans (policy_id, created_at DESC);
  CREATE INDEX inspector_scans_org_idx ON inspector_scans (org_id, created_at DESC);
  CREATE INDEX inspector_scans_claim_idx ON inspector_scans (status, heartbeat_at);
  -- "One active scan per policy" as a database invariant instead of a race
  -- between the API and the scheduler.
  CREATE UNIQUE INDEX inspector_scans_active_key ON inspector_scans (policy_id)
      WHERE status IN ('queued','running');

  CREATE TABLE inspector_findings (
      id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
      scan_id            UUID NOT NULL REFERENCES inspector_scans(id) ON DELETE CASCADE,
      org_id             UUID NOT NULL,
      app_id             UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
      environment_id     UUID,
      -- The third state a two-state (env_id IS NULL) model cannot express.
      -- `issues`, `event_users`, `devices` and `identities` have no environment
      -- column at all, so every rollup finding would otherwise land in the
      -- "unattributed" bucket and conflate "the platform could not attribute
      -- this row" with "this table has no environment concept".
      env_scope          TEXT NOT NULL
                           CHECK (env_scope IN ('enrollment','unattributed','no_env_column')),
      CONSTRAINT inspector_findings_env_consistency
          CHECK ((env_scope = 'enrollment') = (environment_id IS NOT NULL)),
      -- Both from the &'static inventory in sauron-inspector, never caller bytes.
      source_table       TEXT NOT NULL,
      source_column      TEXT NOT NULL,
      -- Dev-controlled bytes: object keys are arbitrary UTF-8, so this is
      -- redacted in Rust before it is written. See sauron_inspector::redact.
      key_path           TEXT NOT NULL,
      matched_key        TEXT NOT NULL,
      detector           TEXT NOT NULL DEFAULT '',
      value_type         TEXT NOT NULL,
      match_count        BIGINT NOT NULL DEFAULT 0,
      match_count_exact  BOOL NOT NULL DEFAULT TRUE,
      -- Shape-only, capped at 64 chars, never more than the first and last
      -- codepoint. NOT the value.
      sample_preview     TEXT NOT NULL DEFAULT '',
      sample_row_id      UUID,
      -- Mandatory for partitioned sources so the reveal query prunes to one child.
      sample_occurred_at TIMESTAMPTZ,
      partition_kind     TEXT NOT NULL DEFAULT 'ranged'
                           CHECK (partition_kind IN ('ranged','default','rollup')),
      first_seen_at      TIMESTAMPTZ,
      last_seen_at       TIMESTAMPTZ,
      created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
  );
  -- An EXPRESSION index, not `NULLS NOT DISTINCT`: that syntax silently raises
  -- the deployment's Postgres floor to 15, and because run_pending_migrations
  -- stops at the first failure, a PG13 host would apply 000041, fail here, and
  -- block every later migration in the product permanently. COALESCE is PG11+.
  CREATE UNIQUE INDEX inspector_findings_key ON inspector_findings
      (scan_id, app_id, env_scope,
       COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid),
       source_table, source_column, key_path, detector);
  CREATE INDEX inspector_findings_scan_rank_idx ON inspector_findings (scan_id, match_count DESC);
  CREATE INDEX inspector_findings_reaper_idx ON inspector_findings (org_id, created_at);
  ```

- [ ] **Step 2: Write `down.sql`.** Create `backend/migrations/2026-08-01-000042_inspector_scan/down.sql`:
  ```sql
  -- Findings first: they reference scans, which reference policies. Dropping a
  -- table drops its indexes and constraints with it.
  DROP TABLE IF EXISTS inspector_findings;
  DROP TABLE IF EXISTS inspector_scans;
  DROP TABLE IF EXISTS inspector_policies;
  ```

- [ ] **Step 3: Apply and see it succeed.** `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`. Then confirm with `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c '\d inspector_findings'` that `inspector_findings_key` exists and no column named `value` or `hash` is present.

- [ ] **Step 4: Add the three `table!` blocks.** Append to `backend/crates/sauron-db/src/schema.rs`, immediately before the first `diesel::joinable!` line:
  ```rust
  diesel::table! {
      inspector_policies (id) {
          id -> Uuid,
          org_id -> Uuid,
          target_type -> Text,
          target_id -> Uuid,
          enabled -> Bool,
          tracked_keys -> Jsonb,
          detectors -> Jsonb,
          scan_columns -> Nullable<Jsonb>,
          rollups -> Jsonb,
          window_days -> Int4,
          schedule_enabled -> Bool,
          schedule_days -> Int2,
          schedule_time -> Time,
          schedule_tz -> Text,
          next_run_at -> Nullable<Timestamptz>,
          last_run_at -> Nullable<Timestamptz>,
          last_scan_id -> Nullable<Uuid>,
          last_skip_reason -> Text,
          created_by -> Nullable<Uuid>,
          created_at -> Timestamptz,
          updated_at -> Timestamptz,
      }
  }

  diesel::table! {
      inspector_scans (id) {
          id -> Uuid,
          policy_id -> Uuid,
          org_id -> Uuid,
          trigger_type -> Text,
          requested_by -> Nullable<Uuid>,
          status -> Text,
          coverage -> Text,
          coverage_note -> Text,
          window_from -> Timestamptz,
          window_to -> Timestamptz,
          params -> Jsonb,
          targets -> Jsonb,
          units_total -> Int4,
          units_done -> Int4,
          cursor -> Jsonb,
          rows_scanned -> Int8,
          findings_count -> Int4,
          findings_reaped_at -> Nullable<Timestamptz>,
          worker_id -> Nullable<Text>,
          heartbeat_at -> Nullable<Timestamptz>,
          attempts -> Int4,
          cancel_requested_at -> Nullable<Timestamptz>,
          error -> Text,
          started_at -> Nullable<Timestamptz>,
          finished_at -> Nullable<Timestamptz>,
          created_at -> Timestamptz,
      }
  }

  diesel::table! {
      inspector_findings (id) {
          id -> Uuid,
          scan_id -> Uuid,
          org_id -> Uuid,
          app_id -> Uuid,
          environment_id -> Nullable<Uuid>,
          env_scope -> Text,
          source_table -> Text,
          source_column -> Text,
          key_path -> Text,
          matched_key -> Text,
          detector -> Text,
          value_type -> Text,
          match_count -> Int8,
          match_count_exact -> Bool,
          sample_preview -> Text,
          sample_row_id -> Nullable<Uuid>,
          sample_occurred_at -> Nullable<Timestamptz>,
          partition_kind -> Text,
          first_seen_at -> Nullable<Timestamptz>,
          last_seen_at -> Nullable<Timestamptz>,
          created_at -> Timestamptz,
      }
  }
  ```

- [ ] **Step 5: Add the joinables and the query-set entries.** In `schema.rs`, after `diesel::joinable!(workflows -> app_environments (environment_id));` add:
  ```rust
  diesel::joinable!(inspector_policies -> organizations (org_id));
  diesel::joinable!(inspector_scans -> inspector_policies (policy_id));
  diesel::joinable!(inspector_scans -> organizations (org_id));
  diesel::joinable!(inspector_findings -> inspector_scans (scan_id));
  diesel::joinable!(inspector_findings -> apps (app_id));
  ```
  and add `inspector_policies,`, `inspector_scans,`, `inspector_findings,` to `allow_tables_to_appear_in_same_query!`. **No `joinable!` for the nullable `created_by`/`requested_by` FKs** — that matches `alert_rules.created_by`, which has none.

- [ ] **Step 6: Add the model structs.** Append to `backend/crates/sauron-db/src/models.rs`:
  ```rust
  // --- PII inspector ----------------------------------------------------------

  #[derive(Debug, Clone, Queryable, Selectable, Serialize)]
  #[diesel(table_name = inspector_policies)]
  #[diesel(check_for_backend(diesel::pg::Pg))]
  pub struct InspectorPolicy {
      pub id: Uuid,
      pub org_id: Uuid,
      pub target_type: String,
      pub target_id: Uuid,
      pub enabled: bool,
      pub tracked_keys: Value,
      pub detectors: Value,
      pub scan_columns: Option<Value>,
      pub rollups: Value,
      pub window_days: i32,
      pub schedule_enabled: bool,
      pub schedule_days: i16,
      #[serde(serialize_with = "ser_time")]
      pub schedule_time: chrono::NaiveTime,
      pub schedule_tz: String,
      pub next_run_at: Option<DateTime<Utc>>,
      pub last_run_at: Option<DateTime<Utc>>,
      pub last_scan_id: Option<Uuid>,
      pub last_skip_reason: String,
      pub created_by: Option<Uuid>,
      pub created_at: DateTime<Utc>,
      pub updated_at: DateTime<Utc>,
  }

  /// `chrono::NaiveTime`'s default Serialize emits `03:00:00`; the dashboard's
  /// `<input type="time">` round-trips `HH:MM`. Pinning the format here rather
  /// than reformatting in three call sites keeps the wire shape single-sourced.
  fn ser_time<S: serde::Serializer>(t: &chrono::NaiveTime, s: S) -> Result<S::Ok, S::Error> {
      s.serialize_str(&t.format("%H:%M").to_string())
  }

  #[derive(Debug, Insertable)]
  #[diesel(table_name = inspector_policies)]
  pub struct NewInspectorPolicy<'a> {
      pub org_id: Uuid,
      pub target_type: &'a str,
      pub target_id: Uuid,
      pub enabled: bool,
      pub tracked_keys: &'a Value,
      pub detectors: &'a Value,
      pub scan_columns: Option<&'a Value>,
      pub rollups: &'a Value,
      pub window_days: i32,
      pub schedule_enabled: bool,
      pub schedule_days: i16,
      pub schedule_time: chrono::NaiveTime,
      pub schedule_tz: &'a str,
      pub created_by: Option<Uuid>,
  }

  /// PATCH body lowered to a diesel changeset. Deliberately NOT `Queryable`:
  /// `Insertable`/`AsChangeset` map by name, `Queryable` decodes positionally,
  /// so adding it would silently bind each field to whatever column occupies
  /// its index.
  #[derive(Debug, Default, AsChangeset)]
  #[diesel(table_name = inspector_policies)]
  pub struct InspectorPolicyPatch<'a> {
      pub enabled: Option<bool>,
      pub tracked_keys: Option<&'a Value>,
      pub detectors: Option<&'a Value>,
      pub scan_columns: Option<Option<&'a Value>>,
      pub rollups: Option<&'a Value>,
      pub window_days: Option<i32>,
      pub schedule_enabled: Option<bool>,
      pub schedule_days: Option<i16>,
      pub schedule_time: Option<chrono::NaiveTime>,
      pub schedule_tz: Option<&'a str>,
      pub updated_at: Option<DateTime<Utc>>,
  }

  #[derive(Debug, Clone, Queryable, Selectable, Serialize, QueryableByName)]
  #[diesel(table_name = inspector_scans)]
  #[diesel(check_for_backend(diesel::pg::Pg))]
  pub struct InspectorScan {
      pub id: Uuid,
      pub policy_id: Uuid,
      pub org_id: Uuid,
      pub trigger_type: String,
      pub requested_by: Option<Uuid>,
      pub status: String,
      pub coverage: String,
      pub coverage_note: String,
      pub window_from: DateTime<Utc>,
      pub window_to: DateTime<Utc>,
      pub params: Value,
      pub targets: Value,
      pub units_total: i32,
      pub units_done: i32,
      pub cursor: Value,
      pub rows_scanned: i64,
      pub findings_count: i32,
      pub findings_reaped_at: Option<DateTime<Utc>>,
      pub worker_id: Option<String>,
      pub heartbeat_at: Option<DateTime<Utc>>,
      pub attempts: i32,
      pub cancel_requested_at: Option<DateTime<Utc>>,
      pub error: String,
      pub started_at: Option<DateTime<Utc>>,
      pub finished_at: Option<DateTime<Utc>>,
      pub created_at: DateTime<Utc>,
  }

  #[derive(Debug, Insertable)]
  #[diesel(table_name = inspector_scans)]
  pub struct NewInspectorScan<'a> {
      pub policy_id: Uuid,
      pub org_id: Uuid,
      pub trigger_type: &'a str,
      pub requested_by: Option<Uuid>,
      pub window_from: DateTime<Utc>,
      pub window_to: DateTime<Utc>,
      pub params: &'a Value,
      pub targets: &'a Value,
      pub units_total: i32,
  }

  #[derive(Debug, Clone, Queryable, Selectable, Serialize)]
  #[diesel(table_name = inspector_findings)]
  #[diesel(check_for_backend(diesel::pg::Pg))]
  pub struct InspectorFinding {
      pub id: Uuid,
      pub scan_id: Uuid,
      pub org_id: Uuid,
      pub app_id: Uuid,
      pub environment_id: Option<Uuid>,
      pub env_scope: String,
      pub source_table: String,
      pub source_column: String,
      pub key_path: String,
      pub matched_key: String,
      pub detector: String,
      pub value_type: String,
      pub match_count: i64,
      pub match_count_exact: bool,
      pub sample_preview: String,
      pub sample_row_id: Option<Uuid>,
      pub sample_occurred_at: Option<DateTime<Utc>>,
      pub partition_kind: String,
      pub first_seen_at: Option<DateTime<Utc>>,
      pub last_seen_at: Option<DateTime<Utc>>,
      pub created_at: DateTime<Utc>,
  }
  ```
  `QueryableByName` on `InspectorScan` is required because the claim uses `diesel::sql_query(...).get_result()`, exactly like `claim_due_monitors` returning `Monitor`.

- [ ] **Step 7: Build.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Expected: clean. If it reports `error[E0573]: expected type, found module`, a column was named `columns` — rename it.

- [ ] **Step 8: Assert the schema delta so far.** Run `cd /home/splimter/projects/freelance/sauron/backend && grep -c '^diesel::table!' crates/sauron-db/src/schema.rs`. Record the number; Task 3 must raise it by exactly 3 more, for a slice total of +6.

- [ ] **Step 9: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 3: Migration 000043 — mask actions, masked keys, reveal audit

**Files:**
- Create `backend/migrations/2026-08-01-000043_inspector_mask_audit/{up,down}.sql`
- Modify `backend/crates/sauron-db/src/schema.rs`
- Modify `backend/crates/sauron-db/src/models.rs`

**Interfaces:**
- Consumes: `inspector_findings`, `inspector_scans` (Task 2) — the FKs require 000042 to have run first.
- Produces: `sauron_db::schema::{inspector_mask_actions, inspector_masked_keys, inspector_reveal_audit}`; `sauron_db::models::{InspectorMaskAction, NewInspectorMaskAction, InspectorMaskedKey, NewInspectorMaskedKey, NewInspectorRevealAudit, InspectorRevealAudit}`.

- [ ] **Step 1: Write `up.sql`.** Create `backend/migrations/2026-08-01-000043_inspector_mask_audit/up.sql`:
  ```sql
  -- The PII inspector's write side. `inspector_mask_actions` is simultaneously
  -- the job queue, the resume cursor, the progress meter and the record of who
  -- did it — this repository's first audit table.
  --
  -- `kind` is load-bearing and is NOT the same axis as `status`. Routing
  -- previews through the status machine (status='preview' as a queue state)
  -- means the mask claim predicate matches neither arm, no preview ever runs,
  -- the dialog polls forever, and confirm — which requires 'previewed' — can
  -- never fire. Counting vs. updating branches on `kind`, never on `phase`.
  --
  -- Upgrade hazard: sauron-migrate.service has no [Install] section and is not
  -- in %postun's restart list, so `dnf upgrade` leaves new binaries running
  -- against the old schema. Until `systemctl start sauron-migrate` is run by
  -- hand, the pipeline's masked_keys_for_app query fails on every cache miss
  -- and forward masking is off deployment-wide with only a log line.

  CREATE TABLE inspector_mask_actions (
      id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
      org_id              UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
      app_id              UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
      kind                TEXT NOT NULL CHECK (kind IN ('preview','mask')),
      -- Nullable so the audit row outlives finding pruning. Both are validated
      -- against app_id at preview time.
      finding_id          UUID REFERENCES inspector_findings(id) ON DELETE SET NULL,
      scan_id             UUID REFERENCES inspector_scans(id) ON DELETE SET NULL,
      -- The fully resolved [{table, column, path, wildcard}] list, frozen at
      -- preview so confirm can never widen what was counted and shown.
      -- Contains paths, never values.
      targets             JSONB NOT NULL DEFAULT '[]'::jsonb,
      status              TEXT NOT NULL DEFAULT 'preview' CHECK (status IN (
                              'preview','previewed','pending','running',
                              'cancelling','done','failed','cancelled')),
      -- SET NULL, not CASCADE: deleting a user must not erase the trail.
      requested_by        UUID REFERENCES users(id) ON DELETE SET NULL,
      -- Denormalized snapshot, because SET NULL loses the identity.
      requested_by_email  TEXT NOT NULL DEFAULT '',
      cancelled_by        UUID REFERENCES users(id) ON DELETE SET NULL,
      cancelled_by_email  TEXT NOT NULL DEFAULT '',
      cancelled_at        TIMESTAMPTZ,
      requested_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
      -- The preview TTL runs from HERE, not from requested_at: a preview queued
      -- behind a multi-hour mask would otherwise expire before it was readable.
      previewed_at        TIMESTAMPTZ,
      confirmed_at        TIMESTAMPTZ,
      started_at          TIMESTAMPTZ,
      finished_at         TIMESTAMPTZ,
      -- Behind the shipped nginx with API_TRUST_FORWARDED_HEADERS=false this is
      -- the proxy's address for every actor; the value records its own trust
      -- decision so a reader can tell.
      confirm_source      TEXT NOT NULL DEFAULT '',
      estimated_rows      BIGINT NOT NULL DEFAULT 0,
      rows_scanned        BIGINT NOT NULL DEFAULT 0,
      rows_masked         BIGINT NOT NULL DEFAULT 0,
      cold_rows_skipped   BIGINT NOT NULL DEFAULT 0,
      -- Re-recorded at finish, not only at preview, so the audit shows what
      -- execution actually skipped rather than what the preview predicted.
      cold_boundary_at    TIMESTAMPTZ,
      day_cursor          DATE,
      cursor_occurred_at  TIMESTAMPTZ,
      cursor_id           UUID,
      phase               TEXT NOT NULL DEFAULT 'idle' CHECK (phase IN (
                              'idle','counting','hot','default_partition',
                              'companions','tail_sweep','finished')),
      worker_id           TEXT,
      claimed_at          TIMESTAMPTZ,
      vacuum_advised      BOOL NOT NULL DEFAULT FALSE,
      error               TEXT NOT NULL DEFAULT ''
  );
  CREATE INDEX inspector_mask_actions_app_idx ON inspector_mask_actions (app_id, requested_at DESC);
  CREATE INDEX inspector_mask_actions_org_idx ON inspector_mask_actions (org_id, requested_at DESC);
  -- Two independent claim slots. A single FIFO would let a multi-hour mask
  -- starve every preview past its 15-minute TTL, making confirm permanently
  -- impossible on a busy app.
  CREATE INDEX inspector_mask_actions_mask_claim_idx ON inspector_mask_actions (requested_at)
      WHERE kind = 'mask' AND status IN ('pending','running','cancelling');
  CREATE INDEX inspector_mask_actions_preview_claim_idx ON inspector_mask_actions (requested_at)
      WHERE kind = 'preview' AND status = 'preview';

  CREATE TABLE inspector_masked_keys (
      id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
      app_id           UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
      -- An ALLOWLIST, not a denylist: a denylist silently fails to protect the
      -- next account table someone adds. The scan-only tables (devices,
      -- identities, workflows) are deliberately absent — a masked-key row for
      -- one of them would be read by the pipeline enforcer and the retro-mask,
      -- both of which would report success on a write the next event overwrites.
      target_table     TEXT NOT NULL CHECK (target_table IN (
                           'error_events','analytics_events','transactions',
                           'issues','event_users','sessions')),
      target_column    TEXT NOT NULL,
      -- '' = the whole column (TEXT columns).
      json_path        TEXT NOT NULL DEFAULT '',
      created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
      created_by       UUID REFERENCES users(id) ON DELETE SET NULL,
      source_action_id UUID REFERENCES inspector_mask_actions(id) ON DELETE SET NULL
  );
  -- Makes re-masking the same finding idempotent.
  CREATE UNIQUE INDEX inspector_masked_keys_key
      ON inspector_masked_keys (app_id, target_table, target_column, json_path);
  CREATE INDEX inspector_masked_keys_app_idx ON inspector_masked_keys (app_id);

  -- POST /findings/{id}/reveal is an endpoint whose entire purpose is emitting
  -- raw customer PII. Shipping it with no record of who revealed what is not
  -- defensible. The row is written BEFORE the value is returned, so a failure
  -- to audit is a failure to reveal.
  CREATE TABLE inspector_reveal_audit (
      id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
      app_id         UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
      org_id         UUID NOT NULL,
      finding_id     UUID REFERENCES inspector_findings(id) ON DELETE SET NULL,
      user_id        UUID REFERENCES users(id) ON DELETE SET NULL,
      user_email     TEXT NOT NULL DEFAULT '',
      source_table   TEXT NOT NULL,
      source_column  TEXT NOT NULL,
      key_path       TEXT NOT NULL,
      request_source TEXT NOT NULL DEFAULT '',
      created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
  );
  CREATE INDEX inspector_reveal_audit_app_idx ON inspector_reveal_audit (app_id, created_at DESC);
  ```

- [ ] **Step 2: Write `down.sql`.** Create `backend/migrations/2026-08-01-000043_inspector_mask_audit/down.sql`:
  ```sql
  -- masked_keys and reveal_audit reference mask_actions / findings, so they go
  -- first. Dropping inspector_masked_keys re-enables ingest of every masked key
  -- immediately: a revert restores raw values on the write path, it does not
  -- restore the ones already overwritten.
  DROP TABLE IF EXISTS inspector_reveal_audit;
  DROP TABLE IF EXISTS inspector_masked_keys;
  DROP TABLE IF EXISTS inspector_mask_actions;
  ```

- [ ] **Step 3: Apply and verify.** `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`, then `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "INSERT INTO inspector_masked_keys (app_id, target_table, target_column) VALUES (gen_random_uuid(), 'auth_sessions', 'x')"`. Expected failure: `new row for relation "inspector_masked_keys" violates check constraint "inspector_masked_keys_target_table_check"` — that is the allowlist doing its job (the FK on `app_id` may fire first; if so, re-run with a real app id).

- [ ] **Step 4: Add the three `table!` blocks.** Append to `schema.rs` before the `joinable!` block:
  ```rust
  diesel::table! {
      inspector_mask_actions (id) {
          id -> Uuid,
          org_id -> Uuid,
          app_id -> Uuid,
          kind -> Text,
          finding_id -> Nullable<Uuid>,
          scan_id -> Nullable<Uuid>,
          targets -> Jsonb,
          status -> Text,
          requested_by -> Nullable<Uuid>,
          requested_by_email -> Text,
          cancelled_by -> Nullable<Uuid>,
          cancelled_by_email -> Text,
          cancelled_at -> Nullable<Timestamptz>,
          requested_at -> Timestamptz,
          previewed_at -> Nullable<Timestamptz>,
          confirmed_at -> Nullable<Timestamptz>,
          started_at -> Nullable<Timestamptz>,
          finished_at -> Nullable<Timestamptz>,
          confirm_source -> Text,
          estimated_rows -> Int8,
          rows_scanned -> Int8,
          rows_masked -> Int8,
          cold_rows_skipped -> Int8,
          cold_boundary_at -> Nullable<Timestamptz>,
          day_cursor -> Nullable<Date>,
          cursor_occurred_at -> Nullable<Timestamptz>,
          cursor_id -> Nullable<Uuid>,
          phase -> Text,
          worker_id -> Nullable<Text>,
          claimed_at -> Nullable<Timestamptz>,
          vacuum_advised -> Bool,
          error -> Text,
      }
  }

  diesel::table! {
      inspector_masked_keys (id) {
          id -> Uuid,
          app_id -> Uuid,
          target_table -> Text,
          target_column -> Text,
          json_path -> Text,
          created_at -> Timestamptz,
          created_by -> Nullable<Uuid>,
          source_action_id -> Nullable<Uuid>,
      }
  }

  diesel::table! {
      inspector_reveal_audit (id) {
          id -> Uuid,
          app_id -> Uuid,
          org_id -> Uuid,
          finding_id -> Nullable<Uuid>,
          user_id -> Nullable<Uuid>,
          user_email -> Text,
          source_table -> Text,
          source_column -> Text,
          key_path -> Text,
          request_source -> Text,
          created_at -> Timestamptz,
      }
  }
  ```

- [ ] **Step 5: Add the joinables and query-set entries.** After the five lines added in Task 2 Step 5:
  ```rust
  diesel::joinable!(inspector_mask_actions -> organizations (org_id));
  diesel::joinable!(inspector_mask_actions -> apps (app_id));
  diesel::joinable!(inspector_masked_keys -> apps (app_id));
  diesel::joinable!(inspector_reveal_audit -> apps (app_id));
  ```
  and add `inspector_mask_actions,`, `inspector_masked_keys,`, `inspector_reveal_audit,` to `allow_tables_to_appear_in_same_query!`.

- [ ] **Step 6: Add the model structs.** Append to `models.rs`:
  ```rust
  #[derive(Debug, Clone, Queryable, Selectable, Serialize, QueryableByName)]
  #[diesel(table_name = inspector_mask_actions)]
  #[diesel(check_for_backend(diesel::pg::Pg))]
  pub struct InspectorMaskAction {
      pub id: Uuid,
      pub org_id: Uuid,
      pub app_id: Uuid,
      pub kind: String,
      pub finding_id: Option<Uuid>,
      pub scan_id: Option<Uuid>,
      pub targets: Value,
      pub status: String,
      pub requested_by: Option<Uuid>,
      pub requested_by_email: String,
      pub cancelled_by: Option<Uuid>,
      pub cancelled_by_email: String,
      pub cancelled_at: Option<DateTime<Utc>>,
      pub requested_at: DateTime<Utc>,
      pub previewed_at: Option<DateTime<Utc>>,
      pub confirmed_at: Option<DateTime<Utc>>,
      pub started_at: Option<DateTime<Utc>>,
      pub finished_at: Option<DateTime<Utc>>,
      pub confirm_source: String,
      pub estimated_rows: i64,
      pub rows_scanned: i64,
      pub rows_masked: i64,
      pub cold_rows_skipped: i64,
      pub cold_boundary_at: Option<DateTime<Utc>>,
      pub day_cursor: Option<chrono::NaiveDate>,
      pub cursor_occurred_at: Option<DateTime<Utc>>,
      pub cursor_id: Option<Uuid>,
      pub phase: String,
      pub worker_id: Option<String>,
      pub claimed_at: Option<DateTime<Utc>>,
      pub vacuum_advised: bool,
      pub error: String,
  }

  #[derive(Debug, Insertable)]
  #[diesel(table_name = inspector_mask_actions)]
  pub struct NewInspectorMaskAction<'a> {
      pub org_id: Uuid,
      pub app_id: Uuid,
      pub kind: &'a str,
      pub finding_id: Option<Uuid>,
      pub scan_id: Option<Uuid>,
      pub targets: &'a Value,
      pub requested_by: Option<Uuid>,
      pub requested_by_email: &'a str,
  }

  #[derive(Debug, Clone, Queryable, Selectable, Serialize)]
  #[diesel(table_name = inspector_masked_keys)]
  #[diesel(check_for_backend(diesel::pg::Pg))]
  pub struct InspectorMaskedKey {
      pub id: Uuid,
      pub app_id: Uuid,
      pub target_table: String,
      pub target_column: String,
      pub json_path: String,
      pub created_at: DateTime<Utc>,
      pub created_by: Option<Uuid>,
      pub source_action_id: Option<Uuid>,
  }

  #[derive(Debug, Insertable)]
  #[diesel(table_name = inspector_masked_keys)]
  pub struct NewInspectorMaskedKey<'a> {
      pub app_id: Uuid,
      pub target_table: &'a str,
      pub target_column: &'a str,
      pub json_path: &'a str,
      pub created_by: Option<Uuid>,
      pub source_action_id: Option<Uuid>,
  }

  #[derive(Debug, Clone, Queryable, Selectable, Serialize)]
  #[diesel(table_name = inspector_reveal_audit)]
  #[diesel(check_for_backend(diesel::pg::Pg))]
  pub struct InspectorRevealAudit {
      pub id: Uuid,
      pub app_id: Uuid,
      pub org_id: Uuid,
      pub finding_id: Option<Uuid>,
      pub user_id: Option<Uuid>,
      pub user_email: String,
      pub source_table: String,
      pub source_column: String,
      pub key_path: String,
      pub request_source: String,
      pub created_at: DateTime<Utc>,
  }

  #[derive(Debug, Insertable)]
  #[diesel(table_name = inspector_reveal_audit)]
  pub struct NewInspectorRevealAudit<'a> {
      pub app_id: Uuid,
      pub org_id: Uuid,
      pub finding_id: Option<Uuid>,
      pub user_id: Option<Uuid>,
      pub user_email: &'a str,
      pub source_table: &'a str,
      pub source_column: &'a str,
      pub key_path: &'a str,
      pub request_source: &'a str,
  }
  ```

- [ ] **Step 7: Build.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Clean.

- [ ] **Step 8: Assert the +6 delta.** `cd /home/splimter/projects/freelance/sauron/backend && grep -c '^diesel::table!' crates/sauron-db/src/schema.rs`. Expected: exactly 6 more than the number recorded before Task 2 Step 4.

- [ ] **Step 9: Confirm the diesel CLI never ran.** `cd /home/splimter/projects/freelance/sauron/backend && grep -n 'file' diesel.toml`. Expected: **no `file =` key**. If one appeared, delete it — with it present `diesel migration run` rewrites `schema.rs` to include every tier-created partition child and redeclares `error_events`' primary key, and all of it compiles.

- [ ] **Step 10: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 4: Config — the `// --- pii inspector ---` section

**Files:**
- Modify `backend/crates/sauron-core/src/config.rs` (struct fields after `alert_event_retention_days`, ~line 95; `from_env` body after `alerts_allow_private`, ~line 218)

**Interfaces:**
- Consumes: the existing private `var()` / `parse()` helpers and the already-read `tier_hot_days` local.
- Produces, on `sauron_core::Config`: `inspector_enabled: bool`, `inspector_tick_secs: u64`, `inspector_batch_rows: i64`, `inspector_batch_pause_ms: u64`, `inspector_lease_secs: i64`, `inspector_max_attempts: i32`, `inspector_statement_timeout_ms: u64`, `inspector_window_days: i64`, `inspector_detector_window_days: i64`, `inspector_max_phase2_rows_per_unit: i64`, `inspector_default_sweep_rows: i64`, `inspector_catchup_grace_hours: i64`, `inspector_scan_keep: i64`, `inspector_finding_retention_days: i64`, `inspector_mask_batch: i64`, `inspector_mask_pause_ms: u64`, `inspector_mask_max_rows: i64`, `inspector_claim_stale_secs: i64`, `inspector_preview_ttl_secs: i64`, `inspector_preview_gc_days: i64`, `inspector_audit_retention_days: i64`, `inspector_audit_pii_days: i64`, `inspector_export_max_rows: i64`, `inspector_policy_cache_secs: u64`, `inspector_tail_sweep_secs: u64`.

- [ ] **Step 1: Write the failing test.** Append to `backend/crates/sauron-core/src/config.rs` (create `#[cfg(test)] mod tests` if the file has none):
  ```rust
  #[cfg(test)]
  mod inspector_config_tests {
      use super::*;

      /// The inspector is OFF by default. It is a heavy scanner against the
      /// same Postgres the ingest path writes to, so a deployment must opt in.
      #[test]
      fn inspector_defaults_are_conservative() {
          // Nothing in the environment: every key falls back.
          let enabled = var("INSPECTOR_ENABLED")
              .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
              .unwrap_or(false);
          assert!(!enabled, "INSPECTOR_ENABLED must default to false");
          assert_eq!(parse("INSPECTOR_TICK_SECS", 30u64), 30);
          assert_eq!(parse("INSPECTOR_MASK_MAX_ROWS", 20_000_000i64), 20_000_000);
          assert_eq!(parse("INSPECTOR_AUDIT_RETENTION_DAYS", 0i64), 0);
      }

      /// The tail sweep must outlast the pipeline's policy cache or it closes
      /// nothing: rows written between "mask applied" and the last replica's
      /// cache refresh stay raw forever, because the retro-mask is a one-shot
      /// job that ends at `done`.
      #[test]
      fn tail_sweep_is_clamped_above_the_cache_ttl() {
          assert_eq!(clamp_tail_sweep(10, 30), 120);
          assert_eq!(clamp_tail_sweep(600, 30), 600);
      }
  }
  ```

- [ ] **Step 2: Run it and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-core inspector_config`. Expected: `error[E0425]: cannot find function 'clamp_tail_sweep' in this scope`.

- [ ] **Step 3: Add the clamp helper.** In `config.rs`, next to `parse`:
  ```rust
  /// The tail sweep re-runs the enforcement seam once after a retro-mask. Its
  /// window must exceed the pipeline's policy-cache TTL by a real margin or it
  /// closes nothing at all. Clamped rather than `bail!`ed because
  /// `Config::from_env` is shared by every binary — a bail here would take down
  /// `sauron-ingest` over a setting it never reads.
  pub fn clamp_tail_sweep(tail_sweep_secs: u64, policy_cache_secs: u64) -> u64 {
      tail_sweep_secs.max(policy_cache_secs.saturating_mul(4))
  }
  ```

- [ ] **Step 4: Add the struct fields.** In `config.rs`, after `pub alert_event_retention_days: i64,`:
  ```rust
  // --- pii inspector ---
  /// Master switch. OFF by default: the scanner reads the same partitions the
  /// ingest path writes, so a deployment opts in deliberately.
  pub inspector_enabled: bool,
  /// Scheduler-loop cadence. Clamped 5..3600.
  pub inspector_tick_secs: u64,
  /// Rows read per phase-1 batch. The LIMIT sits on an index-bounded inner
  /// window, so this bounds SCANNED rows, not matches.
  pub inspector_batch_rows: i64,
  /// Sleep between batches. This plus the batch size is the duty cycle that
  /// keeps the ingest working set resident.
  pub inspector_batch_pause_ms: u64,
  /// A scan whose heartbeat is older than this is re-claimable.
  pub inspector_lease_secs: i64,
  /// After this many claims a scan finalizes as `failed`, so one poison unit
  /// cannot loop forever.
  pub inspector_max_attempts: i32,
  /// Per-connection `SET statement_timeout`, applied at checkout and RESET
  /// before `drop(conn)` — deadpool's recycle does not reset session state.
  pub inspector_statement_timeout_ms: u64,
  /// Scan window ceiling. Defaults to `search_scan_clamp_days`, which itself
  /// defaults to `tier_hot_days`: nothing older is in Postgres anyway.
  pub inspector_window_days: i64,
  /// Detector mode reads every row in the window and walks every string leaf —
  /// roughly 20x the CPU and 20x the bytes shipped — so it gets its own,
  /// much shorter window.
  pub inspector_detector_window_days: i64,
  /// Phase-2 rows per unit before `match_count_exact = false` and
  /// `coverage = 'partial'`.
  pub inspector_max_phase2_rows_per_unit: i64,
  /// Truncation point for the `_default`-partition sweep.
  pub inspector_default_sweep_rows: i64,
  /// A missed scheduled run older than this is skipped, not replayed.
  pub inspector_catchup_grace_hours: i64,
  /// Scans retained per policy.
  pub inspector_scan_keep: i64,
  pub inspector_finding_retention_days: i64,
  /// Rows rewritten per mask batch. Halved automatically when any target
  /// carries a wildcard, because the array rebuild re-serializes the whole
  /// array per row.
  pub inspector_mask_batch: i64,
  pub inspector_mask_pause_ms: u64,
  /// Confirm refuses above this unless the ceiling is raised explicitly.
  pub inspector_mask_max_rows: i64,
  /// A mask action claimed longer ago than this is re-claimable (crash resume).
  pub inspector_claim_stale_secs: i64,
  /// Measured from `previewed_at` — the preview COMPLETING — not from the
  /// request, or a queued preview expires before it is readable.
  pub inspector_preview_ttl_secs: i64,
  pub inspector_preview_gc_days: i64,
  /// 0 = never prune. This table grows per human action, not per rule
  /// evaluation, and it is the record a compliance question is answered from.
  pub inspector_audit_retention_days: i64,
  /// Age at which staff emails and `confirm_source` are nulled, keeping counts
  /// and targets. Without this the privacy feature is the only un-erasable
  /// store of staff PII in the schema.
  pub inspector_audit_pii_days: i64,
  pub inspector_export_max_rows: i64,
  /// Read by BOTH `sauron-ingest` (the enforcer's cache TTL) and `sauron-api`
  /// (the number the UI states literally). Declared in `sauron.env`, never in
  /// `inspector.env`, or the two diverge silently.
  pub inspector_policy_cache_secs: u64,
  /// Read by BOTH `sauron-inspector` and `sauron-api`. Clamped against
  /// `inspector_policy_cache_secs` at load.
  pub inspector_tail_sweep_secs: u64,
  ```

- [ ] **Step 5: Populate them in `from_env`.** In the `Ok(Self { ... })` literal, after `alerts_allow_private: ...,`:
  ```rust
  inspector_enabled: var("INSPECTOR_ENABLED")
      .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
      .unwrap_or(false),
  inspector_tick_secs: parse::<u64>("INSPECTOR_TICK_SECS", 30).clamp(5, 3600),
  inspector_batch_rows: parse("INSPECTOR_BATCH_ROWS", 5_000),
  inspector_batch_pause_ms: parse("INSPECTOR_BATCH_PAUSE_MS", 200),
  inspector_lease_secs: parse("INSPECTOR_LEASE_SECS", 120),
  inspector_max_attempts: parse("INSPECTOR_MAX_ATTEMPTS", 3),
  inspector_statement_timeout_ms: parse("INSPECTOR_STATEMENT_TIMEOUT_MS", 30_000),
  inspector_window_days: parse("INSPECTOR_WINDOW_DAYS", search_scan_clamp_days),
  inspector_detector_window_days: parse("INSPECTOR_DETECTOR_WINDOW_DAYS", 7),
  inspector_max_phase2_rows_per_unit: parse("INSPECTOR_MAX_PHASE2_ROWS_PER_UNIT", 200_000),
  inspector_default_sweep_rows: parse("INSPECTOR_DEFAULT_SWEEP_ROWS", 50_000),
  inspector_catchup_grace_hours: parse("INSPECTOR_CATCHUP_GRACE_HOURS", 6),
  inspector_scan_keep: parse("INSPECTOR_SCAN_KEEP", 20),
  inspector_finding_retention_days: parse("INSPECTOR_FINDING_RETENTION_DAYS", 90),
  inspector_mask_batch: parse("INSPECTOR_MASK_BATCH", 2_000),
  inspector_mask_pause_ms: parse("INSPECTOR_MASK_PAUSE_MS", 200),
  inspector_mask_max_rows: parse("INSPECTOR_MASK_MAX_ROWS", 20_000_000),
  inspector_claim_stale_secs: parse("INSPECTOR_CLAIM_STALE_SECS", 300),
  inspector_preview_ttl_secs: parse("INSPECTOR_PREVIEW_TTL_SECS", 900),
  inspector_preview_gc_days: parse("INSPECTOR_PREVIEW_GC_DAYS", 7),
  inspector_audit_retention_days: parse("INSPECTOR_AUDIT_RETENTION_DAYS", 0),
  inspector_audit_pii_days: parse("INSPECTOR_AUDIT_PII_DAYS", 730),
  inspector_export_max_rows: parse("INSPECTOR_EXPORT_MAX_ROWS", 50_000),
  inspector_policy_cache_secs: policy_cache_secs,
  inspector_tail_sweep_secs: clamp_tail_sweep(
      parse("INSPECTOR_TAIL_SWEEP_SECS", 120),
      policy_cache_secs,
  ),
  ```
  and immediately above `Ok(Self {`, next to the existing `let tier_hot_days = parse("TIER_HOT_DAYS", 30);` line, add:
  ```rust
  // Read once so the tail-sweep clamp can be computed against it below. Both
  // keys live in `sauron.env`, not `inspector.env`: `sauron-ingest` and
  // `sauron-api` never read `inspector.env`, so the "about 30 seconds" the API
  // reports to the UI would otherwise diverge from what the enforcer uses.
  let policy_cache_secs: u64 = parse("INSPECTOR_POLICY_CACHE_SECS", 30);
  ```
  Also move the existing `search_scan_clamp_days` computation above the struct literal into a `let search_scan_clamp_days = parse("SEARCH_SCAN_CLAMP_DAYS", tier_hot_days);` binding, and use that binding both for the existing field and for `inspector_window_days`.

- [ ] **Step 6: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-core inspector_config`. Both tests green.

- [ ] **Step 7: Build the workspace.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Clean — no other binary is affected, because every field defaults.

- [ ] **Step 8: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

> Documentation of these 25 keys in `.env.example`, `docker-compose.yml`, `packaging/rpm/config/*.env` and `README.md` is **Task 34** — it lands with the rest of packaging so the CI assertion S0 added (every `var("KEY")`/`parse("KEY"` literal in `config.rs` appears in `.env.example`) is satisfied in one place. If that CI gate runs on every commit rather than on the branch tip, do Task 34 Steps 1–4 now.

---

## Task 5: The `sauron-inspector` crate scaffold and the column allowlist

**Files:**
- Create `backend/crates/sauron-inspector/Cargo.toml`
- Create `backend/crates/sauron-inspector/src/lib.rs`
- Create `backend/crates/sauron-inspector/src/columns.rs`
- Modify `backend/Cargo.toml` (`[workspace.dependencies]`, after `sauron-query`)

**Interfaces:**
- Consumes: nothing.
- Produces: `sauron_inspector::columns::{ColumnKind, TableClass, ScanColumn, INVENTORY, MASKABLE_TABLES, SCAN_TABLES, find, default_columns, table_class, is_maskable_table}`.

- [ ] **Step 1: Create the manifest.** `backend/crates/sauron-inspector/Cargo.toml`:
  ```toml
  [package]
  name = "sauron-inspector"
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  rust-version.workspace = true

  # Deliberately pure: no diesel, no axum, no tokio, no DuckDB. Every decision
  # in this slice lives here so it is unit-tested without a database — CI runs
  # `cargo test --workspace` with no Postgres, and the DB harness SKIPS.
  # `chrono` is here for the unit decomposition in `units.rs`, which is a pure
  # function of (pairs, tables, window) and must be callable from BOTH the
  # worker and `sauron-db` — see Task 22.
  [dependencies]
  chrono = { workspace = true }
  serde = { workspace = true }
  serde_json = { workspace = true }
  uuid = { workspace = true }
  ```

- [ ] **Step 2: Register it in the workspace.** In `backend/Cargo.toml`, under `[workspace.dependencies]` after the `sauron-query` line, add:
  ```toml
  sauron-inspector = { path = "crates/sauron-inspector" }
  ```

- [ ] **Step 3: Write the failing allowlist test.** Create `backend/crates/sauron-inspector/src/columns.rs` containing **only** this test module for now:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      /// An ALLOWLIST, asserted. A denylist silently fails to protect the next
      /// account table someone adds, and S2's session work asserts this
      /// constraint on this slice.
      #[test]
      fn inventory_contains_no_account_table() {
          const FORBIDDEN: [&str; 8] = [
              "users", "session", "token", "role", "grant", "secret", "mail", "channel",
          ];
          for c in INVENTORY {
              // `sessions` is a telemetry rollup and is allowed by exact name;
              // anything else matching a forbidden substring is not.
              if c.table == "sessions" {
                  continue;
              }
              for bad in FORBIDDEN {
                  assert!(
                      !c.table.contains(bad),
                      "{} looks like an account table and must not be scannable",
                      c.table
                  );
              }
          }
      }

      /// The maskable subset of the inventory must equal the six tables in the
      /// `inspector_masked_keys.target_table` CHECK, exactly. A masked-key row
      /// for a scan-only table would be read by the pipeline enforcer and the
      /// retro-mask, both of which would report success on a write the next
      /// event overwrites.
      #[test]
      fn maskable_subset_matches_the_check_constraint() {
          let mut from_inventory: Vec<&str> = INVENTORY
              .iter()
              .filter(|c| c.maskable)
              .map(|c| c.table)
              .collect();
          from_inventory.sort_unstable();
          from_inventory.dedup();
          let mut expected = MASKABLE_TABLES.to_vec();
          expected.sort_unstable();
          assert_eq!(from_inventory, expected);
      }

      /// devices has no maskable column at all: `upsert_device`'s DO UPDATE is
      /// `family = COALESCE(EXCLUDED.family, devices.family)`, the values are
      /// derived server-side by `enrich`, and there is no wire field for the
      /// enforcer to touch. Offering it would retro-succeed and be overwritten
      /// by the next event from that device, permanently, with a green badge.
      #[test]
      fn scan_only_tables_are_never_maskable() {
          for table in ["devices", "identities", "workflows"] {
              assert!(
                  INVENTORY.iter().any(|c| c.table == table) || table == "devices",
                  "{table} must still be reachable by a scan"
              );
              assert!(
                  !INVENTORY.iter().any(|c| c.table == table && c.maskable),
                  "{table} must never be a mask target"
              );
          }
      }

      /// error_events.title / culprit are absent from `ErrorEvent::as_select()`
      /// but ARE what the Issues list renders. A model-walking scanner misses
      /// them; this inventory is hand-verified against `\d+` for that reason.
      #[test]
      fn title_and_culprit_are_scannable_on_error_events() {
          for col in ["title", "culprit"] {
              let c = find("error_events", col).expect("missing from inventory");
              assert_eq!(c.kind, ColumnKind::Text);
              assert!(c.default_on);
              assert!(c.maskable);
          }
      }

      /// Source lines are verbatim customer source. A `pii:read` holder without
      /// `source:read` could otherwise track the key `pre_context`, reveal, and
      /// receive de-obfuscated proprietary source.
      #[test]
      fn source_bearing_columns_are_not_reveal_eligible() {
          for col in ["stacktrace", "stacktrace_symbolicated", "debug_meta"] {
              let c = find("error_events", col).expect("missing from inventory");
              assert!(!c.reveal_ok, "{col} must not be reveal-eligible");
              assert!(!c.default_on, "{col} must be opt-in");
          }
      }

      /// `transactions` is PARTITIONED (migration 000013 declares
      /// `PARTITION BY RANGE (occurred_at)`) and `sauron-tier` lists it in
      /// TIERED_TABLES. Treating it as a rollup would mean no `occurred_at`
      /// predicate, an `id`-keyset over a column that is not unique across
      /// partitions, and a `_default` sweep that double-scans the same rows.
      #[test]
      fn transactions_is_partitioned_not_a_rollup() {
          assert_eq!(table_class("transactions"), Some(TableClass::Partitioned));
          assert_eq!(table_class("issues"), Some(TableClass::Rollup));
          assert_eq!(table_class("auth_sessions"), None);
      }

      #[test]
      fn default_columns_are_the_bold_set() {
          let mut d: Vec<&str> = default_columns("error_events").iter().map(|c| c.column).collect();
          d.sort_unstable();
          assert_eq!(
              d,
              [
                  "context", "contexts", "culprit", "event_user", "exception_type",
                  "exception_value", "extra", "message", "tags", "title"
              ]
          );
      }
  }
  ```

- [ ] **Step 4: Add the module wiring so it compiles at all.** Create `backend/crates/sauron-inspector/src/lib.rs`:
  ```rust
  //! `sauron-inspector` — every decision the PII inspector makes, as pure
  //! functions over owned data.
  //!
  //! No diesel, no axum, no tokio. CI runs `cargo test --workspace` against a
  //! machine with no Postgres and the DB harness SKIPS, so a decision that
  //! lives in a repo function or a handler is a decision with no test. That is
  //! why the walker, the matcher, the detectors, the redactor, the prefilter
  //! builder, the path grammar, target expansion, target resolution and the
  //! mask applier all live here rather than in the worker binary.

  pub mod columns;
  ```

- [ ] **Step 5: Run it and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector`. Expected: `error[E0425]: cannot find value 'INVENTORY' in this scope` and six sibling errors.

- [ ] **Step 6: Write the inventory.** Prepend to `backend/crates/sauron-inspector/src/columns.rs`, above the test module:
  ```rust
  //! The closed inventory of columns a scan may read and a mask may write.
  //!
  //! Hand-verified against `\d+`, and deliberately NOT derived from the diesel
  //! models: `error_events.title` / `culprit` are absent from
  //! `ErrorEvent::as_select()` but are exactly what the Issues list renders, so
  //! a model-walking scanner would miss the two columns most likely to carry a
  //! customer's name.
  //!
  //! `source_table` / `source_column` on a finding always come from here, never
  //! from caller bytes, because SQL identifiers cannot be bound and the batch
  //! statements interpolate them.

  /// How a column is read and written.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum ColumnKind {
      Jsonb,
      /// Masking a TEXT column replaces the WHOLE value with `'****'`. There is
      /// no partial redaction: the workspace has no direct regex dependency and
      /// partial masking leaves recoverable residue.
      Text,
  }

  /// How a scan decomposes the table into units.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum TableClass {
      /// `PARTITION BY RANGE (occurred_at)`; scanned one `(app, env, day)` at a
      /// time plus one `_default` sweep.
      Partitioned,
      /// One unit per `(app, table)`, PK keyset paginated. An at-rest mask here
      /// may be undone by the next event — see `maskable` per column.
      Rollup,
  }

  #[derive(Debug, Clone, Copy)]
  pub struct ScanColumn {
      pub table: &'static str,
      pub column: &'static str,
      pub kind: ColumnKind,
      /// In the default set when a policy leaves `scan_columns` NULL.
      pub default_on: bool,
      /// May appear in `inspector_masked_keys` and in a mask action's targets.
      pub maskable: bool,
      /// May be returned raw by `POST /findings/{id}/reveal`.
      pub reveal_ok: bool,
      /// Rough bytes-per-row weight, used only to order units cheapest-first
      /// inside a table so a killed scan has covered the most ground.
      pub cost_class: u16,
  }

  /// Exactly the six tables in the `inspector_masked_keys.target_table` CHECK.
  pub const MASKABLE_TABLES: [&str; 6] = [
      "error_events",
      "analytics_events",
      "transactions",
      "issues",
      "event_users",
      "sessions",
  ];

  /// Every table a scan may read, with its unit decomposition.
  pub const SCAN_TABLES: [(&str, TableClass); 8] = [
      ("error_events", TableClass::Partitioned),
      ("analytics_events", TableClass::Partitioned),
      ("transactions", TableClass::Partitioned),
      ("issues", TableClass::Rollup),
      ("event_users", TableClass::Rollup),
      ("sessions", TableClass::Rollup),
      ("identities", TableClass::Rollup),
      ("workflows", TableClass::Rollup),
  ];

  const fn c(
      table: &'static str,
      column: &'static str,
      kind: ColumnKind,
      default_on: bool,
      maskable: bool,
      reveal_ok: bool,
      cost_class: u16,
  ) -> ScanColumn {
      ScanColumn { table, column, kind, default_on, maskable, reveal_ok, cost_class }
  }

  pub const INVENTORY: &[ScanColumn] = &[
      // --- error_events (partitioned) ---
      c("error_events", "tags", ColumnKind::Jsonb, true, true, true, 52),
      c("error_events", "contexts", ColumnKind::Jsonb, true, true, true, 336),
      c("error_events", "extra", ColumnKind::Jsonb, true, true, true, 317),
      c("error_events", "context", ColumnKind::Jsonb, true, true, true, 447),
      c("error_events", "event_user", ColumnKind::Jsonb, true, true, true, 174),
      c("error_events", "breadcrumbs", ColumnKind::Jsonb, false, true, true, 368),
      c("error_events", "sdk", ColumnKind::Jsonb, false, true, true, 64),
      // Not reveal-eligible: debug images can carry absolute build paths that
      // identify a developer's machine and, with `stacktrace_symbolicated`,
      // de-obfuscate proprietary source.
      c("error_events", "debug_meta", ColumnKind::Jsonb, false, false, false, 96),
      c("error_events", "stacktrace", ColumnKind::Jsonb, false, false, false, 623),
      // `strip_source_context` removes context_line/pre_context/post_context
      // from RESPONSES only when the caller lacks `source:read`. A pii:read
      // holder without it must not get them back through reveal.
      c("error_events", "stacktrace_symbolicated", ColumnKind::Jsonb, false, false, false, 700),
      c("error_events", "message", ColumnKind::Text, true, true, true, 80),
      c("error_events", "exception_value", ColumnKind::Text, true, true, true, 80),
      c("error_events", "exception_type", ColumnKind::Text, true, true, true, 32),
      c("error_events", "title", ColumnKind::Text, true, true, true, 96),
      c("error_events", "culprit", ColumnKind::Text, true, true, true, 96),
      // --- analytics_events (partitioned) ---
      c("analytics_events", "properties", ColumnKind::Jsonb, true, true, true, 260),
      c("analytics_events", "tags", ColumnKind::Jsonb, true, true, true, 52),
      c("analytics_events", "contexts", ColumnKind::Jsonb, true, true, true, 200),
      c("analytics_events", "extra", ColumnKind::Jsonb, true, true, true, 200),
      c("analytics_events", "context", ColumnKind::Jsonb, true, true, true, 447),
      // --- transactions (partitioned) ---
      c("transactions", "url", ColumnKind::Text, true, true, true, 120),
      // --- issues (rollup) ---
      c("issues", "title", ColumnKind::Text, true, true, true, 96),
      c("issues", "culprit", ColumnKind::Text, true, true, true, 96),
      // --- event_users (rollup) ---
      // Maskable, but `upsert_event_user` merges with `||`, which never removes
      // keys — an at-rest mask is undone by the next identify(). Reachable
      // through FORWARD ENFORCEMENT only, and the UI says so.
      c("event_users", "properties", ColumnKind::Jsonb, true, true, true, 200),
      // --- sessions (rollup) ---
      // `bump_session` writes the post-enrichment snapshot whole, so masking the
      // enriched `context` sticks on every subsequent event. `distinct_id` and
      // `ip_address` are excluded: both are `COALESCE(EXCLUDED.x, sessions.x)`,
      // so a non-null incoming value always wins.
      c("sessions", "context", ColumnKind::Jsonb, true, true, true, 447),
      // --- identities (rollup, SCAN ONLY) ---
      // `alias_id` and `distinct_id` ARE the identity graph. Collapsing them to
      // '****' does not redact a person — it merges every masked person into
      // one, silently and irreversibly corrupting downstream identity
      // resolution. The remedy is on the SDK side, not here.
      c("identities", "alias_id", ColumnKind::Text, false, false, true, 48),
      c("identities", "distinct_id", ColumnKind::Text, false, false, true, 48),
      // --- workflows (rollup, SCAN ONLY) ---
      // `cancel_reason` is derived server-side in process.rs from
      // properties["reason"], so there is no wire field to enforce on, and
      // `apply_workflow_lifecycle`'s CASE lets a later cancellation write the
      // raw string back over the sentinel. Mask analytics_events.properties
      // instead — that is where the bytes actually arrive.
      c("workflows", "cancel_reason", ColumnKind::Text, false, false, true, 64),
  ];

  /// Look up one inventory entry. Returns `None` for anything outside the
  /// allowlist — which is what makes caller-supplied table/column strings safe
  /// to reject before they ever reach an interpolated identifier.
  pub fn find(table: &str, column: &str) -> Option<&'static ScanColumn> {
      INVENTORY.iter().find(|c| c.table == table && c.column == column)
  }

  /// The set a policy scans when `scan_columns` is NULL.
  pub fn default_columns(table: &str) -> Vec<&'static ScanColumn> {
      INVENTORY.iter().filter(|c| c.table == table && c.default_on).collect()
  }

  pub fn table_class(table: &str) -> Option<TableClass> {
      SCAN_TABLES.iter().find(|(t, _)| *t == table).map(|(_, k)| *k)
  }

  pub fn is_maskable_table(table: &str) -> bool {
      MASKABLE_TABLES.contains(&table)
  }
  ```

- [ ] **Step 7: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector`. All seven tests green.

- [ ] **Step 8: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 6: `walk.rs` — the depth-capped JSON walker

**Files:**
- Create `backend/crates/sauron-inspector/src/walk.rs`
- Modify `backend/crates/sauron-inspector/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `sauron_inspector::walk::{MAX_DEPTH, Leaf, walk}` where `pub struct Leaf<'a> { pub path: String, pub key: String, pub value: &'a serde_json::Value }` and `pub fn walk(root: &serde_json::Value) -> Vec<Leaf<'_>>`.

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-inspector/src/walk.rs` with only:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use serde_json::json;

      /// Sorted DISTINCT paths. The walker deliberately emits one leaf per
      /// array ELEMENT — that is what makes `match_count` count occurrences
      /// rather than shapes — so an array of two objects yields the same path
      /// twice and only the distinct set is interesting to these assertions.
      fn paths(v: &serde_json::Value) -> Vec<String> {
          let mut p: Vec<String> = walk(v).into_iter().map(|l| l.path).collect();
          p.sort();
          p.dedup();
          p
      }

      #[test]
      fn one_level_tags() {
          assert_eq!(paths(&json!({"env": "prod", "email": "a@b.c"})), ["email", "env"]);
      }

      #[test]
      fn two_level_contexts() {
          // A key whose value is an OBJECT still yields a leaf: the matcher
          // matches leaf key names at any depth, and `contexts.order` is a real
          // finding if `order` is tracked.
          assert_eq!(
              paths(&json!({"order": {"id": 7, "email": "a@b.c"}})),
              ["order", "order.email", "order.id"]
          );
      }

      #[test]
      fn arbitrary_depth_extra() {
          assert_eq!(
              paths(&json!({"a": {"b": {"c": {"d": 1}}}})),
              ["a", "a.b", "a.b.c", "a.b.c.d"]
          );
      }

      /// Array elements collapse to a SINGLE `[]` segment. Per-index paths would
      /// make every row produce a different key_path, so the aggregate would be
      /// one finding per array position instead of one per shape.
      ///
      /// An OBJECT element is not itself a named key, so it yields no leaf of
      /// its own — only its children do. That is why `breadcrumbs[]` is absent
      /// here while `breadcrumbs[].data` appears once per element before dedup.
      #[test]
      fn breadcrumb_array_collapses_to_one_segment() {
          let v = json!({"breadcrumbs": [{"data": {"email": "a@b.c"}}, {"data": {"email": "d@e.f"}}]});
          assert_eq!(
              paths(&v),
              ["breadcrumbs", "breadcrumbs[].data", "breadcrumbs[].data.email"]
          );
          // Two elements, two matches: the raw leaf list is NOT deduplicated,
          // because `match_count` counts occurrences.
          assert_eq!(
              walk(&v).iter().filter(|l| l.path == "breadcrumbs[].data.email").count(),
              2
          );
      }

      #[test]
      fn depth_is_capped_at_six() {
          let v = json!({"a": {"b": {"c": {"d": {"e": {"f": {"g": 1}}}}}}});
          let deepest = paths(&v).into_iter().max_by_key(|p| p.len()).unwrap();
          assert_eq!(deepest, "a.b.c.d.e.f");
          assert_eq!(MAX_DEPTH, 6);
      }

      /// Real live data: a circular `contexts` block serializes as this scalar.
      /// A walker that assumes an object root panics or silently drops the row.
      #[test]
      fn tolerates_a_scalar_root() {
          assert!(walk(&json!("[Circular]")).is_empty());
          assert!(walk(&json!(null)).is_empty());
          assert!(walk(&json!(42)).is_empty());
          assert!(walk(&json!([1, 2, 3])).len() == 3);
      }

      #[test]
      fn empty_object_yields_nothing() {
          assert!(walk(&json!({})).is_empty());
      }

      /// Tag keys are unvalidated free-form UTF-8 by design (`tag:<key>=<value>`
      /// is the documented escape hatch), so the walker must not choke on
      /// separators it also uses in paths.
      #[test]
      fn keys_may_contain_dots_spaces_and_equals() {
          let v = json!({"a.b": {"c d": {"e=f": 1}}});
          let ls = walk(&v);
          assert!(ls.iter().any(|l| l.key == "e=f"));
          assert!(ls.iter().any(|l| l.path == "a.b.c d.e=f"));
      }

      #[test]
      fn key_is_lowercased_for_matching_but_path_is_not() {
          let ls = walk(&json!({"Email": 1}));
          assert_eq!(ls[0].key, "email");
          assert_eq!(ls[0].path, "Email");
      }
  }
  ```

- [ ] **Step 2: Wire the module.** Add `pub mod walk;` to `backend/crates/sauron-inspector/src/lib.rs`.

- [ ] **Step 3: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector walk`. Expected: `error[E0425]: cannot find function 'walk' in this scope`.

- [ ] **Step 4: Implement.** Prepend to `walk.rs`:
  ```rust
  //! Depth-capped walk over one jsonb column's parsed value.
  //!
  //! Bounded on purpose: the accumulator downstream is keyed on
  //! `(column, path, matched_key, detector)` and must stay small enough that a
  //! worker's RSS is flat regardless of scan size. A depth cap plus array
  //! collapse is what bounds path cardinality to roughly keys x columns.

  use serde_json::Value;

  /// Deeper than this and a payload is a data structure, not a field an admin
  /// is going to reason about. Also the bound that keeps path cardinality flat.
  pub const MAX_DEPTH: usize = 6;

  /// One key encountered anywhere in the document.
  #[derive(Debug, Clone, PartialEq)]
  pub struct Leaf<'a> {
      /// Dot-joined path from the column root. Array elements collapse to a
      /// single `[]` segment appended to their parent key.
      pub path: String,
      /// The key's own name, LOWERCASED — matching is case-insensitive because
      /// SDK payloads mix `Email`, `EMAIL` and `email` freely.
      pub key: String,
      pub value: &'a Value,
  }

  /// Every key in `root`, at every depth up to [`MAX_DEPTH`].
  ///
  /// A non-object root yields nothing rather than panicking: `contexts` is
  /// sometimes the scalar string `"[Circular]"` in real live data.
  pub fn walk(root: &Value) -> Vec<Leaf<'_>> {
      let mut out = Vec::new();
      descend(root, String::new(), 0, &mut out);
      out
  }

  fn descend<'a>(v: &'a Value, prefix: String, depth: usize, out: &mut Vec<Leaf<'a>>) {
      if depth >= MAX_DEPTH {
          return;
      }
      match v {
          Value::Object(map) => {
              for (k, child) in map {
                  let path = if prefix.is_empty() {
                      k.clone()
                  } else {
                      format!("{prefix}.{k}")
                  };
                  out.push(Leaf { path: path.clone(), key: k.to_lowercase(), value: child });
                  descend(child, path, depth + 1, out);
              }
          }
          Value::Array(items) => {
              // Every element shares one `[]` segment, and the element itself is
              // not a named key, so it produces no Leaf of its own — only its
              // children do.
              let path = format!("{prefix}[]");
              for child in items {
                  match child {
                      Value::Object(_) | Value::Array(_) => {
                          descend(child, path.clone(), depth + 1, out)
                      }
                      // A scalar array element is still worth reporting under
                      // the collapsed path so `tags[]` full of emails is not
                      // invisible; its key is the parent's last segment.
                      _ => out.push(Leaf {
                          path: path.clone(),
                          key: last_segment(prefix.as_str()).to_lowercase(),
                          value: child,
                      }),
                  }
              }
          }
          _ => {}
      }
  }

  fn last_segment(path: &str) -> &str {
      path.rsplit('.').next().unwrap_or(path)
  }
  ```

- [ ] **Step 5: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector walk`. All nine tests green. If `tolerates_a_scalar_root`'s `walk(&json!([1,2,3])).len() == 3` fails, the scalar-array-element arm is missing.

- [ ] **Step 6: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 7: `match.rs` — tracked keys and exact case-insensitive matching

**Files:**
- Create `backend/crates/sauron-inspector/src/match.rs`
- Modify `backend/crates/sauron-inspector/src/lib.rs`

**Interfaces:**
- Consumes: `sauron_inspector::walk::Leaf` (Task 6).
- Produces: `sauron_inspector::matching::{KeyScope, TrackedKey, normalize_key, is_top_level, matched, parse_tracked_keys}`.

> The module **file** is `match.rs` (as the design's file list names it) but it is declared `#[path = "match.rs"] pub mod matching;` because `match` is a Rust keyword and `pub mod r#match;` forces every call site to write `r#match::`.

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-inspector/src/match.rs` with only:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::walk::walk;
      use serde_json::json;

      fn keys(spec: &[(&str, KeyScope)]) -> Vec<TrackedKey> {
          spec.iter()
              .map(|(k, s)| TrackedKey { key: normalize_key(k), scope: *s })
              .collect()
      }

      #[test]
      fn matching_is_case_insensitive() {
          let ks = keys(&[("email", KeyScope::Any)]);
          for doc in [json!({"Email": 1}), json!({"EMAIL": 1}), json!({"email": 1})] {
              let ls = walk(&doc);
              assert!(matched(&ks, &ls[0]).is_some(), "{doc} should match");
          }
      }

      /// Exact, not substring. Substring matching over ~15 keys per row across
      /// millions of rows is a cross product that produces findings nobody
      /// asked for, and it would force a per-key OR instead of one bound text[].
      #[test]
      fn matching_is_exact_not_substring() {
          let ks = keys(&[("email", KeyScope::Any)]);
          for doc in [json!({"user_email": 1}), json!({"emails": 1}), json!({"e-mail": 1})] {
              let ls = walk(&doc);
              assert!(matched(&ks, &ls[0]).is_none(), "{doc} must not match");
          }
      }

      #[test]
      fn any_scope_matches_at_depth() {
          let ks = keys(&[("email", KeyScope::Any)]);
          let doc = json!({"order": {"customer": {"email": "a@b.c"}}});
          let ls = walk(&doc);
          assert!(ls.iter().any(|l| matched(&ks, l).is_some()));
      }

      #[test]
      fn top_scope_matches_only_the_first_level() {
          let ks = keys(&[("email", KeyScope::Top)]);
          let nested = walk(&json!({"order": {"email": "a@b.c"}}));
          assert!(nested.iter().all(|l| matched(&ks, l).is_none()));
          let top = walk(&json!({"email": "a@b.c"}));
          assert!(matched(&ks, &top[0]).is_some());
      }

      /// An array segment is not the top level: `tags[]` is inside a container.
      #[test]
      fn an_array_segment_is_not_top_level() {
          assert!(is_top_level("email"));
          assert!(!is_top_level("order.email"));
          assert!(!is_top_level("tags[]"));
      }

      #[test]
      fn keys_containing_dots_spaces_and_equals_are_accepted() {
          let ks = keys(&[("a.b", KeyScope::Any), ("c d", KeyScope::Any), ("e=f", KeyScope::Any)]);
          for (doc, _) in [(json!({"A.B": 1}), 0), (json!({"C D": 1}), 0), (json!({"E=F": 1}), 0)] {
              let ls = walk(&doc);
              assert!(matched(&ks, &ls[0]).is_some(), "{doc} should match");
          }
      }

      #[test]
      fn normalize_trims_and_lowercases() {
          assert_eq!(normalize_key("  Email \n"), "email");
      }

      #[test]
      fn parse_tolerates_a_bare_string_entry() {
          // Older policy rows and hand-written JSON use `["email"]`; the object
          // form is `[{"key":"email","scope":"top"}]`. Both must load, because a
          // policy that silently parses to zero keys scans nothing and reports
          // a confident false negative.
          let v = json!(["Email", {"key": "SSN", "scope": "top"}]);
          let ks = parse_tracked_keys(&v);
          assert_eq!(ks.len(), 2);
          assert_eq!(ks[0], TrackedKey { key: "email".into(), scope: KeyScope::Any });
          assert_eq!(ks[1], TrackedKey { key: "ssn".into(), scope: KeyScope::Top });
      }

      #[test]
      fn parse_drops_blank_and_non_string_entries() {
          let v = json!(["", "  ", 7, {"key": ""}, {"nope": 1}, "email"]);
          assert_eq!(parse_tracked_keys(&v), vec![TrackedKey { key: "email".into(), scope: KeyScope::Any }]);
      }
  }
  ```

- [ ] **Step 2: Wire the module.** Add to `backend/crates/sauron-inspector/src/lib.rs`:
  ```rust
  // `match` is a keyword, so the module is named `matching` while the file keeps
  // the name the design's file list uses.
  #[path = "match.rs"]
  pub mod matching;
  ```

- [ ] **Step 3: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector matching`. Expected: `error[E0412]: cannot find type 'TrackedKey' in this scope`.

- [ ] **Step 4: Implement.** Prepend to `match.rs`:
  ```rust
  //! Tracked-key matching.
  //!
  //! A tracked key is a literal NAME, lowercased at write, matched
  //! case-insensitively and EXACTLY against a leaf's own key at any depth.
  //! Admin-authored regex was rejected outright: it means accepting ReDoS
  //! authored by an org admin against a shared worker, and `regex` is only a
  //! transitive dependency today so declaring it is a workspace edit.
  //!
  //! Dotted paths are wrong as INPUT — the admin does not know the SDK nested
  //! the field under `contexts.order` — and right as OUTPUT, which is what a
  //! finding's `key_path` is.

  use serde::{Deserialize, Serialize};
  use serde_json::Value;

  use crate::walk::Leaf;

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "lowercase")]
  pub enum KeyScope {
      /// Any depth.
      Any,
      /// Only the top level of the column.
      Top,
  }

  impl Default for KeyScope {
      fn default() -> Self {
          KeyScope::Any
      }
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct TrackedKey {
      pub key: String,
      #[serde(default)]
      pub scope: KeyScope,
  }

  /// Lowercase + trim. Applied at policy write AND at match time, so a row that
  /// predates the normalization still matches.
  pub fn normalize_key(raw: &str) -> String {
      raw.trim().to_lowercase()
  }

  /// True when the path names a key directly under the column root. An array
  /// segment is never top level — `tags[]` is inside a container.
  pub fn is_top_level(path: &str) -> bool {
      !path.contains('.') && !path.contains('[')
  }

  /// The tracked key this leaf satisfies, if any.
  pub fn matched<'k>(keys: &'k [TrackedKey], leaf: &Leaf<'_>) -> Option<&'k TrackedKey> {
      keys.iter().find(|k| {
          k.key == leaf.key && (k.scope == KeyScope::Any || is_top_level(&leaf.path))
      })
  }

  /// Load a policy's `tracked_keys` jsonb.
  ///
  /// Tolerant by design: a policy whose keys silently parse to an empty list
  /// produces a scan that reads zero rows and finishes `succeeded`,
  /// `coverage='full'`, zero findings. A confident false negative on a privacy
  /// scan is the worst thing this feature can emit, so malformed ENTRIES are
  /// dropped individually rather than failing the whole list.
  pub fn parse_tracked_keys(v: &Value) -> Vec<TrackedKey> {
      let Some(arr) = v.as_array() else { return Vec::new() };
      let mut out = Vec::with_capacity(arr.len());
      for item in arr {
          let (raw, scope) = match item {
              Value::String(s) => (s.as_str(), KeyScope::Any),
              Value::Object(o) => {
                  let Some(Value::String(s)) = o.get("key") else { continue };
                  let scope = match o.get("scope").and_then(|s| s.as_str()) {
                      Some("top") => KeyScope::Top,
                      _ => KeyScope::Any,
                  };
                  (s.as_str(), scope)
              }
              _ => continue,
          };
          let key = normalize_key(raw);
          if key.is_empty() {
              continue;
          }
          out.push(TrackedKey { key, scope });
      }
      out
  }
  ```

- [ ] **Step 5: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector matching`. All nine tests green.

- [ ] **Step 6: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Clippy will flag the manual `impl Default for KeyScope`; replace it with `#[derive(Default)]` on the enum plus `#[default]` on the `Any` variant if it does.

---

## Task 8: `detect.rs` — the eight hand-rolled value detectors

**Files:**
- Create `backend/crates/sauron-inspector/src/detect.rs`
- Modify `backend/crates/sauron-inspector/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `sauron_inspector::detect::{Detector, ALL_DETECTORS, parse_detectors, detect_first}`.

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-inspector/src/detect.rs` with only:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn email_accepts_a_plus_tag() {
          assert!(Detector::Email.matches("jane+receipts@acme.co.uk"));
          assert!(Detector::Email.matches("a@b.co"));
          assert!(!Detector::Email.matches("jane@acme"));
          assert!(!Detector::Email.matches("@acme.com"));
          assert!(!Detector::Email.matches("jane@@acme.com"));
          assert!(!Detector::Email.matches("not an email"));
      }

      #[test]
      fn e164_with_and_without_plus() {
          assert!(Detector::PhoneE164.matches("+213770123456"));
          assert!(Detector::PhoneE164.matches("447700900123"));
          assert!(!Detector::PhoneE164.matches("12345"));
          assert!(!Detector::PhoneE164.matches("+0123456789"));
          assert!(!Detector::PhoneE164.matches("+44 7700 900123"));
      }

      #[test]
      fn ip_detectors() {
          assert!(Detector::Ipv4.matches("192.168.1.10"));
          assert!(!Detector::Ipv4.matches("999.1.1.1"));
          assert!(!Detector::Ipv4.matches("1.2.3"));
          assert!(Detector::Ipv6.matches("2001:db8::1"));
          assert!(!Detector::Ipv6.matches("2001:db8"));
      }

      #[test]
      fn jwt_needs_three_base64url_segments() {
          assert!(Detector::Jwt.matches("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcDEF-_123"));
          assert!(!Detector::Jwt.matches("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0"));
          assert!(!Detector::Jwt.matches("a.b.c"));
      }

      #[test]
      fn iban_and_ssn() {
          assert!(Detector::Iban.matches("DE89370400440532013000"));
          assert!(!Detector::Iban.matches("DE8937"));
          assert!(Detector::SsnUs.matches("123-45-6789"));
          assert!(!Detector::SsnUs.matches("000-45-6789"));
          assert!(!Detector::SsnUs.matches("12345678"));
      }

      /// Luhn, not "16 digits". A non-Luhn 16-digit number is an order id, a
      /// device serial or a padded counter, and flagging those is what makes a
      /// detector-mode report unreadable.
      #[test]
      fn credit_card_requires_luhn() {
          assert!(Detector::CreditCard.matches("4111111111111111"));
          assert!(Detector::CreditCard.matches("4111 1111 1111 1111"));
          assert!(!Detector::CreditCard.matches("4111111111111112"));
          assert!(!Detector::CreditCard.matches("1234567890123456"));
      }

      /// The negative corpus that keeps detector mode usable. Every one of these
      /// is something a real payload is full of.
      #[test]
      fn negative_corpus_is_clean() {
          let corpus = [
              "550e8400-e29b-41d4-a716-446655440000",
              "2026-08-01T03:00:00Z",
              "ORD-2026-0001",
              "checkout_started",
              "1.2.3",
              "v1.14.0",
              "sha256:9f86d081884c7d659a2feaa0c55ad015",
              "",
              "0",
          ];
          for s in corpus {
              let hit = detect_first(&ALL_DETECTORS, s);
              assert!(hit.is_none(), "{s} was flagged as {hit:?}");
          }
      }

      #[test]
      fn ids_round_trip() {
          for d in ALL_DETECTORS {
              assert_eq!(Detector::from_id(d.id()), Some(d));
          }
          assert_eq!(Detector::from_id("nope"), None);
      }

      #[test]
      fn parse_drops_unknown_ids() {
          let v = serde_json::json!(["email", "nope", 7, "credit_card"]);
          assert_eq!(parse_detectors(&v), vec![Detector::Email, Detector::CreditCard]);
      }
  }
  ```

- [ ] **Step 2: Wire the module.** Add `pub mod detect;` to `lib.rs`.

- [ ] **Step 3: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector detect`. Expected: `error[E0433]: failed to resolve: use of undeclared type 'Detector'`.

- [ ] **Step 4: Implement.** Prepend to `detect.rs`:
  ```rust
  //! A fixed, CLOSED library of value-shape detectors.
  //!
  //! Hand-rolled byte scanners, not regex. `regex` is only a transitive
  //! dependency today (via `validator`, `woothee`, `arrow-string`), so declaring
  //! it is a workspace edit — and admin-authored patterns would mean accepting
  //! ReDoS authored by an org admin against a shared worker.
  //!
  //! Detectors are opt-in per policy and get their own much shorter window,
  //! because enabling them removes the SQL prefilter entirely: every row in the
  //! window is shipped out of Postgres and every string leaf is scanned. That is
  //! roughly 20x the CPU and 20x the bytes of key mode.

  use serde_json::Value;

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Detector {
      Email,
      PhoneE164,
      Ipv4,
      Ipv6,
      Jwt,
      Iban,
      SsnUs,
      CreditCard,
  }

  pub const ALL_DETECTORS: [Detector; 8] = [
      Detector::Email,
      Detector::PhoneE164,
      Detector::Ipv4,
      Detector::Ipv6,
      Detector::Jwt,
      Detector::Iban,
      Detector::SsnUs,
      Detector::CreditCard,
  ];

  impl Detector {
      pub fn id(self) -> &'static str {
          match self {
              Detector::Email => "email",
              Detector::PhoneE164 => "phone_e164",
              Detector::Ipv4 => "ipv4",
              Detector::Ipv6 => "ipv6",
              Detector::Jwt => "jwt",
              Detector::Iban => "iban",
              Detector::SsnUs => "ssn_us",
              Detector::CreditCard => "credit_card",
          }
      }

      pub fn from_id(s: &str) -> Option<Detector> {
          ALL_DETECTORS.into_iter().find(|d| d.id() == s)
      }

      pub fn matches(self, s: &str) -> bool {
          match self {
              Detector::Email => is_email(s),
              Detector::PhoneE164 => is_e164(s),
              Detector::Ipv4 => is_ipv4(s),
              Detector::Ipv6 => is_ipv6(s),
              Detector::Jwt => is_jwt(s),
              Detector::Iban => is_iban(s),
              Detector::SsnUs => is_ssn_us(s),
              Detector::CreditCard => is_credit_card(s),
          }
      }
  }

  /// The first enabled detector this value trips, in `ALL_DETECTORS` order.
  /// First-wins rather than all-matches: a finding carries one detector, and
  /// reporting the same path once per detector would multiply the findings
  /// table by eight for no extra information.
  pub fn detect_first(enabled: &[Detector], s: &str) -> Option<Detector> {
      ALL_DETECTORS.into_iter().find(|d| enabled.contains(d) && d.matches(s))
  }

  /// Load a policy's `detectors` jsonb, dropping ids this build does not know.
  /// An unknown id is a downgrade artifact, not a reason to fail the scan.
  pub fn parse_detectors(v: &Value) -> Vec<Detector> {
      let Some(arr) = v.as_array() else { return Vec::new() };
      arr.iter()
          .filter_map(|i| i.as_str())
          .filter_map(Detector::from_id)
          .collect()
  }

  fn is_email(s: &str) -> bool {
      let s = s.trim();
      if s.len() < 6 || s.len() > 254 || s.contains(char::is_whitespace) {
          return false;
      }
      let mut parts = s.split('@');
      let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
          return false;
      };
      if local.is_empty() || domain.len() < 4 {
          return false;
      }
      // A bare hostname is not an address; require a dot with labels either side.
      match domain.rsplit_once('.') {
          Some((host, tld)) => {
              !host.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
          }
          None => false,
      }
  }

  fn is_e164(s: &str) -> bool {
      let s = s.trim();
      let digits = s.strip_prefix('+').unwrap_or(s);
      // E.164 is 8..15 digits and never starts with 0. No separators: a value
      // with spaces or dashes is a formatted local number, and treating it as
      // E.164 flags every order reference that happens to be numeric.
      (8..=15).contains(&digits.len())
          && digits.bytes().all(|b| b.is_ascii_digit())
          && !digits.starts_with('0')
  }

  fn is_ipv4(s: &str) -> bool {
      let mut n = 0;
      for part in s.trim().split('.') {
          n += 1;
          if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
              return false;
          }
          if part.parse::<u16>().unwrap_or(999) > 255 {
              return false;
          }
      }
      n == 4
  }

  fn is_ipv6(s: &str) -> bool {
      let s = s.trim();
      // At least two colons keeps `2001:db8` and `08:30` out; hex-or-empty
      // groups accept the `::` compressed form without a full parser.
      s.matches(':').count() >= 2
          && s.len() >= 3
          && s.split(':').all(|g| g.len() <= 4 && g.bytes().all(|b| b.is_ascii_hexdigit()))
  }

  fn is_jwt(s: &str) -> bool {
      let parts: Vec<&str> = s.trim().split('.').collect();
      parts.len() == 3
          // A real header/payload is base64url of at least a small JSON object.
          && parts[0].len() >= 8
          && parts[1].len() >= 8
          && parts.iter().all(|p| {
              !p.is_empty()
                  && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=')
          })
  }

  fn is_iban(s: &str) -> bool {
      let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
      let b = compact.as_bytes();
      (15..=34).contains(&b.len())
          && b[0].is_ascii_uppercase()
          && b[1].is_ascii_uppercase()
          && b[2].is_ascii_digit()
          && b[3].is_ascii_digit()
          && b[4..].iter().all(|c| c.is_ascii_alphanumeric())
  }

  fn is_ssn_us(s: &str) -> bool {
      let b = s.trim().as_bytes();
      if b.len() != 11 || b[3] != b'-' || b[6] != b'-' {
          return false;
      }
      if !b.iter().enumerate().all(|(i, c)| i == 3 || i == 6 || c.is_ascii_digit()) {
          return false;
      }
      // Area 000/666/9xx and group/serial 00/0000 are never issued; excluding
      // them is what keeps `000-00-0000` placeholders out of the report.
      let area = &s[0..3];
      area != "000" && area != "666" && !area.starts_with('9') && &s[4..6] != "00" && &s[7..11] != "0000"
  }

  fn is_credit_card(s: &str) -> bool {
      let digits: Vec<u32> = s
          .chars()
          .filter(|c| !matches!(c, ' ' | '-'))
          .map(|c| c.to_digit(10).unwrap_or(u32::MAX))
          .collect();
      if !(13..=19).contains(&digits.len()) || digits.iter().any(|d| *d == u32::MAX) {
          return false;
      }
      // Luhn. Without it every 16-digit order id is a "credit card" and the
      // report is unreadable, which is how a privacy scan gets ignored.
      let mut sum = 0;
      for (i, d) in digits.iter().rev().enumerate() {
          let mut v = *d;
          if i % 2 == 1 {
              v *= 2;
              if v > 9 {
                  v -= 9;
              }
          }
          sum += v;
      }
      sum % 10 == 0
  }
  ```

- [ ] **Step 5: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector detect`. All nine tests green. If `negative_corpus_is_clean` fails on `550e8400-e29b-41d4-a716-446655440000`, tighten `is_iban` (a UUID contains `-`, so the compact form must still be rejected by the `b[0..2]` uppercase check) — assert which detector fired from the failure message before changing anything.

- [ ] **Step 6: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 9: `redact.rs` — shape-only previews and redacted key paths

**Files:**
- Create `backend/crates/sauron-inspector/src/redact.rs`
- Modify `backend/crates/sauron-inspector/src/lib.rs`

**Interfaces:**
- Consumes: `sauron_inspector::detect::{ALL_DETECTORS, detect_first}` (Task 8).
- Produces: `sauron_inspector::redact::{PREVIEW_MAX, PATH_SEGMENT_MAX, PATH_MAX, REDACTED_SEGMENT, truncate_chars, preview, value_type, redact_path}`.

> **This is the task that keeps the findings table from becoming a PII store.** Both halves of the original design missed that `key_path` is untrusted input: `ErrorItem.tags/contexts/extra` are `serde_json::Value`, so object *keys* are arbitrary dev-controlled UTF-8. A payload shaped `extra.customers["jane@acme.com"].email` writes raw PII straight into `key_path`, unredacted, rendered in the UI, emitted into the CSV, and reachable by every `pii:read` holder with no reveal call.

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-inspector/src/redact.rs` with only:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use serde_json::json;

      /// The corpus every property assertion in this module runs against.
      const RAW: [&str; 8] = [
          "jane+receipts@acme.co.uk",
          "+213770123456",
          "123-45-6789",
          "4111111111111111",
          "192.168.1.10",
          "DE89370400440532013000",
          "Jane Q. Doe",
          "شارع محمد الخامس 12",
      ];

      #[test]
      fn preview_never_contains_the_raw_value() {
          for raw in RAW {
              let p = preview(&json!(raw));
              assert!(!p.contains(raw), "preview {p:?} leaked {raw:?}");
              assert!(p.chars().count() <= PREVIEW_MAX);
          }
      }

      #[test]
      fn preview_echoes_at_most_first_and_last_codepoint() {
          assert_eq!(preview(&json!("jane@acme.com")), "j…m");
          assert_eq!(preview(&json!("شارع محمد")), "ش…د");
      }

      #[test]
      fn short_strings_are_not_echoed_at_all() {
          for s in ["", "a", "ab", "abc"] {
              assert_eq!(preview(&json!(s)), "<short string>");
          }
      }

      /// Numbers and booleans must not leak magnitude: `cart_value_cents: 4200`
      /// is a customer's order total.
      #[test]
      fn scalars_render_without_magnitude() {
          assert_eq!(preview(&json!(4200)), "<number>");
          assert_eq!(preview(&json!(-0.5)), "<number>");
          assert_eq!(preview(&json!(true)), "<boolean>");
          assert_eq!(preview(&json!(null)), "<null>");
          assert_eq!(preview(&json!({"a": 1})), "<object>");
          assert_eq!(preview(&json!([1, 2])), "<array>");
      }

      #[test]
      fn value_types_are_stable_strings() {
          assert_eq!(value_type(&json!("x")), "string");
          assert_eq!(value_type(&json!(1)), "number");
          assert_eq!(value_type(&json!(true)), "boolean");
          assert_eq!(value_type(&json!(null)), "null");
          assert_eq!(value_type(&json!({})), "object");
          assert_eq!(value_type(&json!([])), "array");
      }

      #[test]
      fn truncate_is_char_boundary_safe() {
          let s = "شارعشارعشارع";
          let t = truncate_chars(s, 4);
          assert_eq!(t.chars().count(), 4);
          assert!(s.starts_with(&t));
      }

      /// The whole point of this module's second half.
      #[test]
      fn key_path_never_contains_the_raw_value() {
          for raw in RAW {
              let path = format!("extra.customers.{raw}.email");
              let r = redact_path(&path);
              assert!(!r.contains(raw), "key_path {r:?} leaked {raw:?}");
          }
      }

      /// The path is split on `.` FIRST, so an interpolated email is already
      /// three segments by the time redaction runs: `jane@acme`, `com`, and
      /// the real key `email`. Only the segment that trips a rule is replaced —
      /// `com` is indistinguishable from a field name and survives. What
      /// matters is the property asserted above: the raw value is no longer
      /// reconstructible from the path. Collapsing neighbours would be
      /// prettier and would also erase real field names next to a redaction.
      #[test]
      fn a_detector_tripping_segment_is_replaced_wholesale() {
          assert_eq!(
              redact_path("customers.jane@acme.com.email"),
              format!("customers.{REDACTED_SEGMENT}.com.email")
          );
      }

      #[test]
      fn an_over_long_segment_is_replaced_not_truncated() {
          let long = "x".repeat(PATH_SEGMENT_MAX + 1);
          assert_eq!(redact_path(&format!("a.{long}.b")), format!("a.{REDACTED_SEGMENT}.b"));
      }

      /// A segment that is mostly digits or punctuation is an id or an
      /// interpolated value, not a field name. `ssn_123-45-6789` shows why a
      /// detector alone is not enough: the segment is not a bare SSN.
      #[test]
      fn a_segment_that_looks_like_data_is_replaced() {
          assert_eq!(redact_path("extra.ssn_123-45-6789.value"), format!("extra.{REDACTED_SEGMENT}.value"));
          assert_eq!(redact_path("extra.order.id"), "extra.order.id");
          assert_eq!(redact_path("breadcrumbs[].data.email"), "breadcrumbs[].data.email");
      }

      #[test]
      fn the_whole_path_is_capped() {
          let deep = (0..40).map(|i| format!("seg{i}")).collect::<Vec<_>>().join(".");
          let r = redact_path(&deep);
          assert!(r.chars().count() <= PATH_MAX);
      }
  }
  ```

- [ ] **Step 2: Wire the module.** Add `pub mod redact;` to `lib.rs`.

- [ ] **Step 3: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector redact`. Expected: `error[E0425]: cannot find function 'preview' in this scope`.

- [ ] **Step 4: Implement.** Prepend to `redact.rs`:
  ```rust
  //! Everything written into `inspector_findings` passes through here first.
  //!
  //! A tool that reports PII is a tool that stores PII. The findings table has
  //! no raw-value column and no hash column — a SHA-256 of an email is a stable
  //! pseudonymous identifier of a person and is trivially brute-forced for
  //! low-entropy values — so the ONLY things that reach it are a shape-only
  //! preview and a path. Both are produced here, and both are property-tested
  //! against a corpus for non-containment of the raw value.

  use serde_json::Value;

  use crate::detect::{detect_first, ALL_DETECTORS};

  pub const PREVIEW_MAX: usize = 64;
  pub const PATH_SEGMENT_MAX: usize = 64;
  pub const PATH_MAX: usize = 512;
  /// What a segment that carries data rather than a field name becomes.
  pub const REDACTED_SEGMENT: &str = "<key>";

  /// Truncate to `n` CODEPOINTS. `&s[..n]` panics mid-codepoint on the Arabic
  /// and CJK keys real payloads contain.
  pub fn truncate_chars(s: &str, n: usize) -> String {
      s.chars().take(n).collect()
  }

  /// A stable type name for the UI's "is this really an email or an enum?".
  pub fn value_type(v: &Value) -> &'static str {
      match v {
          Value::Null => "null",
          Value::Bool(_) => "boolean",
          Value::Number(_) => "number",
          Value::String(_) => "string",
          Value::Array(_) => "array",
          Value::Object(_) => "object",
      }
  }

  /// A shape-only rendering: at most the first and last codepoint of a string,
  /// and no magnitude at all for anything else.
  pub fn preview(v: &Value) -> String {
      let Value::String(s) = v else {
          return format!("<{}>", value_type(v));
      };
      let chars: Vec<char> = s.chars().collect();
      // Below four codepoints, first-and-last IS the value.
      if chars.len() < 4 {
          return "<short string>".to_string();
      }
      truncate_chars(
          &format!("{}…{}", chars[0], chars[chars.len() - 1]),
          PREVIEW_MAX,
      )
  }

  /// Redact a walked path so it is safe to store, render, and export.
  ///
  /// Object keys are arbitrary dev-controlled UTF-8, so a payload shaped
  /// `extra.customers["jane@acme.com"].email` would otherwise write raw PII
  /// straight into a column every `pii:read` holder can read with no reveal
  /// call and no audit row.
  pub fn redact_path(path: &str) -> String {
      let redacted: Vec<String> = path
          .split('.')
          .map(|seg| {
              if segment_is_data(seg) {
                  REDACTED_SEGMENT.to_string()
              } else {
                  seg.to_string()
              }
          })
          .collect();
      truncate_chars(&redacted.join("."), PATH_MAX)
  }

  /// Whether a path segment carries data rather than naming a field.
  ///
  /// Three independent tests, because a detector alone is not enough:
  /// `ssn_123-45-6789` is not a bare SSN and would pass one.
  fn segment_is_data(seg: &str) -> bool {
      // An array marker is structural, never data.
      let bare = seg.strip_suffix("[]").unwrap_or(seg);
      if bare.is_empty() {
          return false;
      }
      if bare.chars().count() > PATH_SEGMENT_MAX {
          return true;
      }
      if detect_first(&ALL_DETECTORS, bare).is_some() {
          return true;
      }
      // A field name is overwhelmingly letters plus `_`/`-`/digits. A segment
      // that is mostly digits, or carries `@`/`+`/`:`/`/`/whitespace, is an
      // interpolated identifier or a value.
      if bare.chars().any(|c| matches!(c, '@' | '+' | ':' | '/' | '\\') || c.is_whitespace()) {
          return true;
      }
      let digits = bare.chars().filter(|c| c.is_ascii_digit()).count();
      digits * 2 > bare.chars().count()
  }
  ```

- [ ] **Step 5: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector redact`. All eleven tests green. `a_segment_that_looks_like_data_is_replaced` is the one to watch: `ssn_123-45-6789` has 9 digits out of 15 characters, so the digit-majority rule fires.

- [ ] **Step 6: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 10: `prefilter.rs` — ILIKE escaping and phase-1 pattern construction

**Files:**
- Create `backend/crates/sauron-inspector/src/prefilter.rs`
- Modify `backend/crates/sauron-inspector/src/lib.rs`

**Interfaces:**
- Consumes: `sauron_inspector::matching::TrackedKey` (Task 7), `sauron_inspector::detect::Detector` (Task 8).
- Produces: `sauron_inspector::prefilter::{escape_like, like_contains, key_patterns, text_key_patterns, use_prefilter}`.

> `escape_like` is **private** in `repo.rs` and this crate has no DB dependency, so the three-character escape is re-implemented here with its own tests rather than citing a function it cannot call. Keep both in sync by behaviour, asserted below.

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-inspector/src/prefilter.rs` with only:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::detect::Detector;
      use crate::matching::{KeyScope, TrackedKey};

      fn k(name: &str) -> TrackedKey {
          TrackedKey { key: name.into(), scope: KeyScope::Any }
      }

      #[test]
      fn escapes_the_three_like_metacharacters() {
          assert_eq!(escape_like("50%"), "50\\%");
          assert_eq!(escape_like("a_b"), "a\\_b");
          assert_eq!(escape_like("c\\d"), "c\\\\d");
      }

      /// A double quote is NOT a LIKE metacharacter and must survive verbatim —
      /// the whole pattern is `%"key"%`, so escaping it would match nothing.
      #[test]
      fn a_double_quote_is_untouched() {
          assert_eq!(escape_like("say\"hi"), "say\"hi");
      }

      #[test]
      fn like_contains_wraps_in_percent() {
          assert_eq!(like_contains("50%"), "%50\\%%");
      }

      /// The pattern greps the JSON TEXT for the QUOTED key name. Without the
      /// quotes, tracking `id` matches every row that contains the letters "id"
      /// anywhere — including inside a UUID — and the prefilter eliminates
      /// nothing, which is the entire cost model.
      #[test]
      fn patterns_quote_the_key() {
          assert_eq!(key_patterns(&[k("email")]), vec!["%\"email\"%".to_string()]);
      }

      #[test]
      fn patterns_escape_metacharacters_inside_the_key() {
          assert_eq!(key_patterns(&[k("a%b_c")]), vec!["%\"a\\%b\\_c\"%".to_string()]);
      }

      /// A TEXT column is not JSON, so there are no quotes to grep for. Applying
      /// the quoted pattern to `error_events.title` matches NOTHING — which is
      /// how ten `default_on` TEXT columns, the ones the Issues list renders,
      /// come to report zero findings with `coverage='full'`.
      #[test]
      fn a_text_column_pattern_is_unquoted() {
          assert_eq!(text_key_patterns(&[k("email")]), vec!["%email%".to_string()]);
          assert_eq!(text_key_patterns(&[k("a%b")]), vec!["%a\\%b%".to_string()]);
      }

      /// When detectors are on, the prefilter is omitted ENTIRELY and every row
      /// in the (shorter) detector window is walked. That is what makes a
      /// detector-only policy work at all — otherwise a policy with no tracked
      /// keys builds an empty pattern list, matches zero rows, and finishes
      /// `succeeded` / `coverage='full'` / zero findings. A confident false
      /// negative on a privacy scan is the worst thing this feature can emit.
      #[test]
      fn detectors_disable_the_prefilter() {
          assert!(!use_prefilter(&[], &[Detector::Email]));
          assert!(!use_prefilter(&[k("email")], &[Detector::Email]));
          assert!(use_prefilter(&[k("email")], &[]));
      }

      /// No keys and no detectors is rejected at the API with a 400; if one ever
      /// reaches the worker it must scan nothing rather than everything.
      #[test]
      fn an_empty_policy_still_uses_the_prefilter() {
          assert!(use_prefilter(&[], &[]));
          assert!(key_patterns(&[]).is_empty());
      }
  }
  ```

- [ ] **Step 2: Wire the module.** Add `pub mod prefilter;` to `lib.rs`.

- [ ] **Step 3: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector prefilter`. Expected: `error[E0425]: cannot find function 'escape_like' in this scope`.

- [ ] **Step 4: Implement.** Prepend to `prefilter.rs`:
  ```rust
  //! Phase-1 SQL prefilter construction.
  //!
  //! The scan is two phases: a cheap `column::text ILIKE ANY($patterns)` over an
  //! index-bounded row window, then a `serde_json` walk in Rust over only the
  //! rows that survive. Measured on this codebase, `extra::text ILIKE` over
  //! 210,146 rows / 678 MB is 184 ms — about 0.9 us/row — and eliminates 95-99%
  //! of rows before anything is parsed.
  //!
  //! DETECTION IS BEST-EFFORT, NOT A COMPLIANCE GUARANTEE. This greps the JSON
  //! *text* for the quoted key name, so a key serialized with a unicode escape
  //! (`"email"`) evades it, as does anything inside a base64 or URL-encoded
  //! blob. That is the right tool for accidental PII, which is what it is for,
  //! and useless against an adversary. The Findings tab says so non-dismissibly.

  use crate::detect::Detector;
  use crate::matching::TrackedKey;

  /// Escape Postgres LIKE/ILIKE wildcards so a key matches literally.
  ///
  /// Re-implemented rather than imported: `repo::escape_like` is private and
  /// this crate has no diesel dependency on purpose. Postgres' default
  /// LIKE/ILIKE escape character is `\`, and exactly three characters need it.
  pub fn escape_like(v: &str) -> String {
      let mut out = String::with_capacity(v.len());
      for c in v.chars() {
          if matches!(c, '\\' | '%' | '_') {
              out.push('\\');
          }
          out.push(c);
      }
      out
  }

  pub fn like_contains(v: &str) -> String {
      format!("%{}%", escape_like(v))
  }

  /// One `%"key"%` pattern per tracked key, for a single bound `text[]`.
  ///
  /// The quotes are load-bearing: the value is matched against the column's
  /// serialized JSON, where an object key always appears as `"name":`. Dropping
  /// them turns tracking `id` into a substring match against every UUID in the
  /// row and the prefilter stops eliminating anything.
  pub fn key_patterns(keys: &[TrackedKey]) -> Vec<String> {
      keys.iter().map(|k| like_contains(&format!("\"{}\"", k.key))).collect()
  }

  /// The same list WITHOUT the quotes, for `ColumnKind::Text` columns.
  ///
  /// A TEXT column is not JSON: `error_events.title` is `Error: jane@acme.com`,
  /// with no `"email":` anywhere in it. Applying the quoted pattern to it
  /// matches nothing, so a policy tracking `email` would report zero findings
  /// with `coverage='full'` for exactly the ten `default_on` TEXT columns the
  /// Issues list renders. The trade is honest and stated in the UI: an unquoted
  /// substring over free text is noisier than a key-name grep, which is why
  /// phase 2 still has to agree before a finding is written.
  pub fn text_key_patterns(keys: &[TrackedKey]) -> Vec<String> {
      keys.iter().map(|k| like_contains(&k.key)).collect()
  }

  /// Whether phase 1 applies an ILIKE predicate at all.
  ///
  /// False when any detector is enabled: a detector looks at VALUES, and no key
  /// name predicate can pre-select rows for it. Building a pattern list from the
  /// key list alone and applying it anyway is how a detector-only policy scans
  /// zero rows and reports zero findings with `coverage='full'`.
  pub fn use_prefilter(keys: &[TrackedKey], detectors: &[Detector]) -> bool {
      let _ = keys;
      detectors.is_empty()
  }
  ```

- [ ] **Step 5: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector prefilter`. All eight tests green.

- [ ] **Step 6: Cross-check against the private original.** Run `cd /home/splimter/projects/freelance/sauron/backend && sed -n '/^fn escape_like/,/^}/p' crates/sauron-db/src/repo.rs` and confirm character-for-character that the escaped set is `'\\' | '%' | '_'` and the escape character is `\`. If they differ, the DB one wins and this copy is wrong.

- [ ] **Step 7: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. Clippy will flag `let _ = keys;` as needless; replace the parameter with `_keys: &[TrackedKey]` and delete the line if it does — the parameter stays in the signature because every call site reads better passing both, and a future prefilter that combines them will need it.

---

## Task 11: `path.rs` — the mask-path grammar

**Files:**
- Create `backend/crates/sauron-inspector/src/path.rs`
- Modify `backend/crates/sauron-inspector/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `sauron_inspector::path::{MaskPath, PathError, parse_mask_path, finding_path_to_mask_path}`, with `MaskPath { pub head: String, pub wildcard: bool, pub rest: Vec<String> }` and methods `text_array()`, `sub_array()`, `to_wire()`.

> **A mask path is RELATIVE TO THE COLUMN and never repeats the column name.** `error_events.extra` + `customer.email` masks `extra->'customer'->'email'`; `error_events.stacktrace` + `[*].abs_path` masks `abs_path` in every element of the `stacktrace` array. This is not cosmetic: the wildcard lowering the design fixes is `jsonb_agg` over `jsonb_array_elements(col)`, i.e. **the column itself is the array**, so a wildcard hanging off a named segment (`breadcrumbs[*].data.email` on the `breadcrumbs` column) has no lowering at all — the SQL would find `jsonb_typeof(col) = 'array'` true, look for `col->'breadcrumbs'` inside each frame, match nothing, and report a successful mask that changed nothing. It is rejected by the grammar rather than silently no-op'd.

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-inspector/src/path.rs` with only:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn a_plain_path_parses() {
          let p = parse_mask_path("customer.email").unwrap();
          assert_eq!(p.head, "customer");
          assert!(!p.wildcard);
          assert_eq!(p.rest, ["email"]);
          assert_eq!(p.text_array(), ["customer", "email"]);
          assert_eq!(p.to_wire(), "customer.email");
      }

      #[test]
      fn a_single_segment_parses() {
          let p = parse_mask_path("email").unwrap();
          assert_eq!(p.text_array(), ["email"]);
          assert!(p.sub_array().is_empty());
      }

      /// A bare `[*]` is the ONLY wildcard form: the path is relative to the
      /// column, and the column value itself is the array.
      #[test]
      fn a_bare_leading_wildcard_parses() {
          let p = parse_mask_path("[*].data.email").unwrap();
          assert_eq!(p.head, "");
          assert!(p.wildcard);
          assert_eq!(p.sub_array(), ["data", "email"]);
          assert_eq!(p.to_wire(), "[*].data.email");
      }

      /// The wildcard lowering is `jsonb_agg` over `jsonb_array_elements(col)`,
      /// so an array one level INSIDE the column has no lowering. Rejecting it
      /// is the difference between "not maskable" and a mask that reports
      /// success having changed nothing.
      #[test]
      fn a_wildcard_on_a_named_segment_is_rejected() {
          assert_eq!(
              parse_mask_path("breadcrumbs[*].data.email"),
              Err(PathError::WildcardNotAtRoot)
          );
      }

      /// An index is not stable across rows, so a finding must never carry one
      /// and a mask must never accept one.
      #[test]
      fn a_numeric_index_is_rejected() {
          assert_eq!(parse_mask_path("breadcrumbs.3.data.email"), Err(PathError::NumericIndex));
          assert_eq!(parse_mask_path("0"), Err(PathError::NumericIndex));
      }

      #[test]
      fn a_non_leading_wildcard_is_rejected() {
          assert_eq!(parse_mask_path("a.b[*].c"), Err(PathError::WildcardNotFirst));
      }

      #[test]
      fn a_second_wildcard_is_rejected() {
          assert_eq!(parse_mask_path("[*].b[*]"), Err(PathError::WildcardNotFirst));
      }

      #[test]
      fn empty_and_blank_segments_are_rejected() {
          assert_eq!(parse_mask_path(""), Err(PathError::Empty));
          assert_eq!(parse_mask_path("a..b"), Err(PathError::EmptySegment));
          assert_eq!(parse_mask_path("a. .b"), Err(PathError::EmptySegment));
      }

      #[test]
      fn a_path_deeper_than_the_walker_is_rejected() {
          assert_eq!(parse_mask_path("a.b.c.d.e.f.g"), Err(PathError::TooDeep));
      }

      /// The walker emits `[]`; the mask grammar spells the wildcard `[*]`.
      /// Converting rather than making them identical keeps `key_path` a
      /// faithful record of where the value was found. Both are relative to
      /// the column, so a finding on the `stacktrace` column reads `[].abs_path`
      /// and masks as `[*].abs_path`.
      #[test]
      fn a_finding_path_converts_to_a_mask_path() {
          assert_eq!(finding_path_to_mask_path("[].abs_path").unwrap(), "[*].abs_path");
          assert_eq!(finding_path_to_mask_path("customer.email").unwrap(), "customer.email");
      }

      /// An array that is not the column root cannot be expressed by a grammar
      /// whose only lowering is `jsonb_array_elements(col)`, so the finding is
      /// reported and simply is not maskable.
      #[test]
      fn an_array_below_the_column_root_has_no_mask_path() {
          assert_eq!(
              finding_path_to_mask_path("breadcrumbs[].data.email"),
              Err(PathError::WildcardNotAtRoot)
          );
          assert_eq!(
              finding_path_to_mask_path("a.items[].email"),
              Err(PathError::WildcardNotFirst)
          );
      }

      /// A redacted segment names no real key, so a mask built from it would
      /// target a path that exists in no row.
      #[test]
      fn a_redacted_segment_has_no_mask_path() {
          assert_eq!(finding_path_to_mask_path("extra.<key>.email"), Err(PathError::RedactedSegment));
      }
  }
  ```

- [ ] **Step 2: Wire the module.** Add `pub mod path;` to `lib.rs`.

- [ ] **Step 3: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector path`. Expected: `error[E0433]: failed to resolve: use of undeclared type 'PathError'`.

- [ ] **Step 4: Implement.** Prepend to `path.rs`:
  ```rust
  //! The mask-path grammar: dot-separated segments RELATIVE TO THE COLUMN,
  //! plus AT MOST ONE wildcard, legal only as a bare leading `[*]`.
  //!
  //! The one-wildcard rule is not arbitrary. A non-wildcard path lowers to a
  //! single `jsonb_set(coalesce(col,'{}'), $path::text[], '"****"', false)`. A
  //! wildcard path lowers to a full array rebuild — `jsonb_agg` over
  //! `jsonb_array_elements(col) WITH ORDINALITY` — which re-serializes the whole
  //! array per row and is measurably more expensive, which is why the batch size
  //! halves when any target carries one. Two wildcards would mean a nested
  //! rebuild with no bound on the work per row.
  //!
  //! The bare-`[*]` rule is not arbitrary either: `jsonb_array_elements(col)`
  //! means THE COLUMN IS THE ARRAY. A wildcard hanging off a named segment
  //! (`breadcrumbs[*].data.email`) has no lowering, and if it were accepted the
  //! statement would match nothing and the audit row would report a successful
  //! mask that changed no bytes — the worst possible outcome for a privacy
  //! control. `error_events.stacktrace` and `error_events.breadcrumbs` are both
  //! arrays at their root (`process.rs` writes `json!([])` for each), so the
  //! forms this grammar accepts are the forms the product actually needs.

  use crate::redact::REDACTED_SEGMENT;
  use crate::walk::MAX_DEPTH;

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum PathError {
      Empty,
      EmptySegment,
      /// An index is not stable across rows.
      NumericIndex,
      WildcardNotFirst,
      /// A wildcard on a NAMED segment. The only lowering is
      /// `jsonb_array_elements(col)`, so the array has to be the column itself.
      WildcardNotAtRoot,
      TooDeep,
      /// The finding's path segment was replaced by the redactor, so it names no
      /// real key.
      RedactedSegment,
  }

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct MaskPath {
      /// The first segment. EMPTY for a wildcard path, because a wildcard
      /// addresses the column itself.
      pub head: String,
      /// Whether the COLUMN is an array whose every element is addressed.
      pub wildcard: bool,
      /// Everything after `head`.
      pub rest: Vec<String>,
  }

  impl MaskPath {
      /// The full path as a bound `text[]` for the non-wildcard lowering.
      ///
      /// A wildcard path has an empty head, and pushing `""` would build the
      /// `text[]` `{"", "abs_path"}` — a path that exists in no document and
      /// would silently mask nothing. Wildcard callers use `sub_array()`.
      pub fn text_array(&self) -> Vec<String> {
          if self.head.is_empty() {
              return self.rest.clone();
          }
          let mut v = Vec::with_capacity(self.rest.len() + 1);
          v.push(self.head.clone());
          v.extend(self.rest.iter().cloned());
          v
      }

      /// The path WITHIN one array element, for the wildcard lowering.
      pub fn sub_array(&self) -> Vec<String> {
          self.rest.clone()
      }

      /// Round-trips `parse_mask_path`. This is what is stored in
      /// `inspector_masked_keys.json_path` and in a mask action's `targets`.
      pub fn to_wire(&self) -> String {
          let head = if self.wildcard {
              format!("{}[*]", self.head)
          } else {
              self.head.clone()
          };
          if self.rest.is_empty() {
              head
          } else {
              format!("{head}.{}", self.rest.join("."))
          }
      }
  }

  pub fn parse_mask_path(raw: &str) -> Result<MaskPath, PathError> {
      if raw.trim().is_empty() {
          return Err(PathError::Empty);
      }
      let parts: Vec<&str> = raw.split('.').collect();
      if parts.len() > MAX_DEPTH {
          return Err(PathError::TooDeep);
      }
      let mut head = String::new();
      let mut wildcard = false;
      let mut rest = Vec::with_capacity(parts.len().saturating_sub(1));
      for (i, part) in parts.iter().enumerate() {
          let seg = *part;
          if seg.trim().is_empty() {
              return Err(PathError::EmptySegment);
          }
          let (bare, has_star) = match seg.strip_suffix("[*]") {
              Some(b) => (b, true),
              None => (seg, false),
          };
          if has_star && i != 0 {
              return Err(PathError::WildcardNotFirst);
          }
          // `[*]` is legal ONLY bare: the lowering is
          // `jsonb_array_elements(col)`, so the array is the column itself.
          if has_star && !bare.is_empty() {
              return Err(PathError::WildcardNotAtRoot);
          }
          if bare.is_empty() && !has_star {
              return Err(PathError::EmptySegment);
          }
          if bare.contains("[*]") || bare.contains('[') || bare.contains(']') {
              return Err(PathError::WildcardNotFirst);
          }
          // Guarded on non-empty: `"".bytes().all(..)` is vacuously true, and a
          // bare `[*]` head would otherwise be rejected as a numeric index.
          if !bare.is_empty() && bare.bytes().all(|b| b.is_ascii_digit()) {
              return Err(PathError::NumericIndex);
          }
          if i == 0 {
              head = bare.to_string();
              wildcard = has_star;
          } else {
              rest.push(bare.to_string());
          }
      }
      Ok(MaskPath { head, wildcard, rest })
  }

  /// Convert a finding's `key_path` (walker form, `[]`) into a mask path
  /// (`[*]`), or explain why the finding is not maskable.
  pub fn finding_path_to_mask_path(key_path: &str) -> Result<String, PathError> {
      if key_path.split('.').any(|s| s == REDACTED_SEGMENT) {
          return Err(PathError::RedactedSegment);
      }
      let candidate = key_path.replace("[]", "[*]");
      Ok(parse_mask_path(&candidate)?.to_wire())
  }
  ```

- [ ] **Step 5: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector path`. All twelve tests green.

- [ ] **Step 6: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 12: `targets.rs` part 1 — identifier enums, `MaskTarget`, `expand_targets`

**Files:**
- Create `backend/crates/sauron-inspector/src/targets.rs`
- Modify `backend/crates/sauron-inspector/src/lib.rs`

**Interfaces:**
- Consumes: `sauron_inspector::columns::{find, ColumnKind}` (Task 5), `sauron_inspector::path::{parse_mask_path, PathError}` (Task 11).
- Produces: `sauron_inspector::targets::{TargetTable, TargetColumn, MaskTarget, TargetError, expand_targets, validate_target}`.

> **SQL identifiers cannot be bound, so the batch functions must interpolate `target_table` and `target_column`.** The worker reads `targets` back out of Postgres in a *different process* from the one that validated it, so "validated in Rust at write time" is not a control. These are therefore enums whose `as_sql()` returns `&'static str`, and the batch functions take the enums, never `String`. Anything that can write that JSONB column — this API, a future repo fn, a migration — would otherwise be injection into an unattended `UPDATE` running with full DB rights.

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-inspector/src/targets.rs` with only:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      fn t(table: TargetTable, column: TargetColumn, path: &str) -> MaskTarget {
          MaskTarget { table, column, path: path.to_string() }
      }

      #[test]
      fn as_sql_is_static_and_round_trips() {
          for tt in TargetTable::ALL {
              assert_eq!(TargetTable::from_sql(tt.as_sql()), Some(tt));
          }
          for tc in TargetColumn::ALL {
              assert_eq!(TargetColumn::from_sql(tc.as_sql()), Some(tc));
          }
          assert_eq!(TargetTable::from_sql("auth_sessions"), None);
          assert_eq!(TargetTable::from_sql("error_events; DROP TABLE users"), None);
      }

      #[test]
      fn a_target_outside_the_inventory_is_rejected() {
          // `identities` is scan-only, so it is not a TargetTable at all, and
          // `stacktrace` is in the inventory but not maskable.
          assert_eq!(
              validate_target(&t(TargetTable::ErrorEvents, TargetColumn::Stacktrace, "abs_path")),
              Err(TargetError::NotMaskable)
          );
          assert_eq!(
              validate_target(&t(TargetTable::Issues, TargetColumn::Extra, "")),
              Err(TargetError::NoSuchColumn)
          );
      }

      #[test]
      fn a_text_column_takes_the_whole_value_and_rejects_a_path() {
          assert_eq!(validate_target(&t(TargetTable::Issues, TargetColumn::Title, "")), Ok(()));
          assert_eq!(
              validate_target(&t(TargetTable::Issues, TargetColumn::Title, "a.b")),
              Err(TargetError::PathOnTextColumn)
          );
      }

      #[test]
      fn a_jsonb_column_requires_a_path() {
          assert_eq!(
              validate_target(&t(TargetTable::ErrorEvents, TargetColumn::Extra, "")),
              Err(TargetError::MissingPath)
          );
          assert_eq!(validate_target(&t(TargetTable::ErrorEvents, TargetColumn::Extra, "a.b")), Ok(()));
          assert_eq!(
              validate_target(&t(TargetTable::ErrorEvents, TargetColumn::Extra, "a.3.b")),
              Err(TargetError::Path(crate::path::PathError::NumericIndex))
          );
      }

      /// `error_events.title` is derived SERVER-SIDE by `build_title(exc,
      /// message)` and has NO wire field, so `apply_wire` has nothing to mask
      /// for that target: the first event after the mask writes a raw title and
      /// the Issues page shows the PII again while the audit row reports
      /// success. Masking the INPUTS `build_title`/`build_culprit` consume is
      /// what makes forward enforcement actually reach them.
      #[test]
      fn title_expands_to_the_wire_sources_and_issues() {
          let out = expand_targets(&t(TargetTable::ErrorEvents, TargetColumn::Title, ""));
          let pairs: Vec<(&str, &str)> =
              out.iter().map(|m| (m.table.as_sql(), m.column.as_sql())).collect();
          assert!(pairs.contains(&("error_events", "title")));
          assert!(pairs.contains(&("issues", "title")));
          assert!(pairs.contains(&("error_events", "exception_value")));
          assert!(pairs.contains(&("error_events", "exception_type")));
          assert!(pairs.contains(&("error_events", "message")));
      }

      #[test]
      fn culprit_expands_to_issues_culprit() {
          let out = expand_targets(&t(TargetTable::ErrorEvents, TargetColumn::Culprit, ""));
          let pairs: Vec<(&str, &str)> =
              out.iter().map(|m| (m.table.as_sql(), m.column.as_sql())).collect();
          assert!(pairs.contains(&("issues", "culprit")));
      }

      /// The symbolicated copy holds the same frame data.
      #[test]
      fn stacktrace_expands_to_the_symbolicated_copy() {
          let out = expand_targets(&t(
              TargetTable::ErrorEvents,
              TargetColumn::Stacktrace,
              "[*].abs_path",
          ));
          assert!(out
              .iter()
              .any(|m| m.column == TargetColumn::StacktraceSymbolicated && m.path == "[*].abs_path"));
      }

      /// `bump_session` snapshots the same enriched jsonb on every event, so a
      /// mask on `context` that ignores `sessions.context` leaves a live copy.
      #[test]
      fn context_expands_to_sessions_context() {
          for table in [TargetTable::ErrorEvents, TargetTable::AnalyticsEvents] {
              let out = expand_targets(&t(table, TargetColumn::Context, "user.email"));
              assert!(out.iter().any(|m| m.table == TargetTable::Sessions
                  && m.column == TargetColumn::Context
                  && m.path == "user.email"));
          }
      }

      #[test]
      fn everything_else_expands_to_itself() {
          let one = t(TargetTable::ErrorEvents, TargetColumn::Extra, "customer.email");
          assert_eq!(expand_targets(&one), vec![one.clone()]);
      }

      #[test]
      fn expansion_is_deduplicated_and_contains_the_original_first() {
          let out = expand_targets(&t(TargetTable::ErrorEvents, TargetColumn::Title, ""));
          assert_eq!(out[0].column, TargetColumn::Title);
          let mut seen = out.clone();
          seen.dedup();
          assert_eq!(seen.len(), out.len());
      }
  }
  ```

- [ ] **Step 2: Wire the module.** Add `pub mod targets;` to `lib.rs`.

- [ ] **Step 3: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector targets`. Expected: `error[E0433]: failed to resolve: use of undeclared type 'TargetTable'`.

- [ ] **Step 4: Implement the enums.** Prepend to `targets.rs`:
  ```rust
  //! Mask targets, as ENUMS rather than strings.
  //!
  //! SQL identifiers cannot be bound, so the batch statements interpolate the
  //! table and column names. The worker reads `inspector_mask_actions.targets`
  //! back out of Postgres in a DIFFERENT PROCESS from the one that validated it,
  //! so "validated in Rust at write time" is not a control. Deserializing into
  //! enums whose `as_sql()` returns `&'static str` is: an unknown pair fails
  //! deserialization and the worker fails the action rather than interpolating
  //! caller bytes into an unattended UPDATE running with full DB rights.

  use serde::{Deserialize, Serialize};

  use crate::columns::{find, ColumnKind};
  use crate::path::{parse_mask_path, PathError};

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum TargetTable {
      ErrorEvents,
      AnalyticsEvents,
      Transactions,
      Issues,
      EventUsers,
      Sessions,
  }

  impl TargetTable {
      pub const ALL: [TargetTable; 6] = [
          TargetTable::ErrorEvents,
          TargetTable::AnalyticsEvents,
          TargetTable::Transactions,
          TargetTable::Issues,
          TargetTable::EventUsers,
          TargetTable::Sessions,
      ];

      pub fn as_sql(self) -> &'static str {
          match self {
              TargetTable::ErrorEvents => "error_events",
              TargetTable::AnalyticsEvents => "analytics_events",
              TargetTable::Transactions => "transactions",
              TargetTable::Issues => "issues",
              TargetTable::EventUsers => "event_users",
              TargetTable::Sessions => "sessions",
          }
      }

      pub fn from_sql(s: &str) -> Option<TargetTable> {
          TargetTable::ALL.into_iter().find(|t| t.as_sql() == s)
      }

      /// Partitioned tables get a day loop and an `occurred_at` range on every
      /// statement; rollups get one keyset pass filtered on `app_id`.
      ///
      /// Every rollup keysets on the bare `id` PK — including `sessions`, whose
      /// `(started_at, id)` ordering would buy locality and nothing else: `id`
      /// is a unique non-null PK on all six maskable tables, so `id > $cursor
      /// ORDER BY id` already visits every row exactly once. A second keyset
      /// shape here would be a second cursor encoding in `BatchOutcome` and a
      /// second resume path to get wrong. (This supersedes design §9's
      /// "`(started_at, id)` for `sessions`".)
      pub fn is_partitioned(self) -> bool {
          matches!(
              self,
              TargetTable::ErrorEvents | TargetTable::AnalyticsEvents | TargetTable::Transactions
          )
      }
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum TargetColumn {
      Tags,
      Contexts,
      Extra,
      Context,
      EventUser,
      Breadcrumbs,
      Sdk,
      DebugMeta,
      Stacktrace,
      StacktraceSymbolicated,
      Message,
      ExceptionValue,
      ExceptionType,
      Title,
      Culprit,
      Properties,
      Url,
  }

  impl TargetColumn {
      pub const ALL: [TargetColumn; 17] = [
          TargetColumn::Tags,
          TargetColumn::Contexts,
          TargetColumn::Extra,
          TargetColumn::Context,
          TargetColumn::EventUser,
          TargetColumn::Breadcrumbs,
          TargetColumn::Sdk,
          TargetColumn::DebugMeta,
          TargetColumn::Stacktrace,
          TargetColumn::StacktraceSymbolicated,
          TargetColumn::Message,
          TargetColumn::ExceptionValue,
          TargetColumn::ExceptionType,
          TargetColumn::Title,
          TargetColumn::Culprit,
          TargetColumn::Properties,
          TargetColumn::Url,
      ];

      pub fn as_sql(self) -> &'static str {
          match self {
              TargetColumn::Tags => "tags",
              TargetColumn::Contexts => "contexts",
              TargetColumn::Extra => "extra",
              TargetColumn::Context => "context",
              TargetColumn::EventUser => "event_user",
              TargetColumn::Breadcrumbs => "breadcrumbs",
              TargetColumn::Sdk => "sdk",
              TargetColumn::DebugMeta => "debug_meta",
              TargetColumn::Stacktrace => "stacktrace",
              TargetColumn::StacktraceSymbolicated => "stacktrace_symbolicated",
              TargetColumn::Message => "message",
              TargetColumn::ExceptionValue => "exception_value",
              TargetColumn::ExceptionType => "exception_type",
              TargetColumn::Title => "title",
              TargetColumn::Culprit => "culprit",
              TargetColumn::Properties => "properties",
              TargetColumn::Url => "url",
          }
      }

      pub fn from_sql(s: &str) -> Option<TargetColumn> {
          TargetColumn::ALL.into_iter().find(|c| c.as_sql() == s)
      }
  }

  /// One fully resolved mask target. `path` is `""` for a TEXT column (the whole
  /// value is replaced) and a wire-form mask path for a jsonb column.
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct MaskTarget {
      pub table: TargetTable,
      pub column: TargetColumn,
      #[serde(default)]
      pub path: String,
  }

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum TargetError {
      /// The `(table, column)` pair is not in the inventory at all.
      NoSuchColumn,
      /// In the inventory, but `maskable = false`.
      NotMaskable,
      /// A jsonb column with no path would collapse the entire column.
      MissingPath,
      /// A TEXT column takes the whole value; a path there means the caller
      /// believes something is happening that is not.
      PathOnTextColumn,
      Path(PathError),
  }

  pub fn validate_target(t: &MaskTarget) -> Result<(), TargetError> {
      let Some(entry) = find(t.table.as_sql(), t.column.as_sql()) else {
          return Err(TargetError::NoSuchColumn);
      };
      if !entry.maskable {
          return Err(TargetError::NotMaskable);
      }
      match entry.kind {
          ColumnKind::Text => {
              if t.path.is_empty() {
                  Ok(())
              } else {
                  Err(TargetError::PathOnTextColumn)
              }
          }
          ColumnKind::Jsonb => {
              if t.path.is_empty() {
                  return Err(TargetError::MissingPath);
              }
              parse_mask_path(&t.path).map(|_| ()).map_err(TargetError::Path)
          }
      }
  }
  ```

- [ ] **Step 5: Implement `expand_targets`.** Append to `targets.rs`, above the test module:
  ```rust
  /// Everything a mask on `t` must ALSO rewrite, `t` first.
  ///
  /// Applied at PREVIEW time and frozen into the action's `targets`, so confirm
  /// can never widen what was counted and shown. Nothing outside this map
  /// auto-expands.
  pub fn expand_targets(t: &MaskTarget) -> Vec<MaskTarget> {
      let mut out = vec![t.clone()];
      let mut push = |table: TargetTable, column: TargetColumn, path: &str| {
          let m = MaskTarget { table, column, path: path.to_string() };
          if !out.contains(&m) {
              out.push(m);
          }
      };
      match (t.table, t.column) {
          // `error_events.title` is derived server-side by `build_title(exc,
          // message)` and has NO wire field, so forward enforcement cannot reach
          // it directly: the next event writes a raw title and the Issues page
          // shows the PII again while the audit row says success. Mask the
          // inputs. `issues.title` additionally gets the sticky guard in
          // `upsert_issue`, because `exception_type` is concatenated into the
          // title too and the 30s cache window restores the raw string on the
          // very next occurrence.
          (TargetTable::ErrorEvents, TargetColumn::Title) => {
              push(TargetTable::Issues, TargetColumn::Title, "");
              push(TargetTable::ErrorEvents, TargetColumn::ExceptionValue, "");
              push(TargetTable::ErrorEvents, TargetColumn::ExceptionType, "");
              push(TargetTable::ErrorEvents, TargetColumn::Message, "");
          }
          (TargetTable::ErrorEvents, TargetColumn::Culprit) => {
              push(TargetTable::Issues, TargetColumn::Culprit, "");
          }
          // The symbolicated copy holds the same frame data.
          (TargetTable::ErrorEvents, TargetColumn::Stacktrace) => {
              push(
                  TargetTable::ErrorEvents,
                  TargetColumn::StacktraceSymbolicated,
                  &t.path,
              );
          }
          // `bump_session` snapshots the same enriched jsonb on every event.
          (TargetTable::ErrorEvents | TargetTable::AnalyticsEvents, TargetColumn::Context) => {
              push(TargetTable::Sessions, TargetColumn::Context, &t.path);
          }
          _ => {}
      }
      out
  }
  ```

- [ ] **Step 6: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector targets`. All ten tests green. Note that `stacktrace` expansion is deliberately reachable even though `validate_target` rejects it as a mask target — the expansion map is pure and the validation is the gate.

- [ ] **Step 7: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`. The closure capturing `out` mutably while also reading it will not borrow-check; if clippy or rustc complains, replace the closure with a plain `fn push_unique(out: &mut Vec<MaskTarget>, m: MaskTarget)` free function and call it explicitly.

---

## Task 13: `targets.rs` part 2 — `resolve_targets`, the precedence subtraction

**Files:**
- Modify `backend/crates/sauron-inspector/src/targets.rs` (append above the test module; add cases to the test module)

**Interfaces:**
- Consumes: nothing from Task 12 (independent types in the same file).
- Produces: `sauron_inspector::targets::{PolicyTargetType, PolicyNode, ScanPair, ResolvedTargets, resolve_targets, include_rollups}`.

> Precedence is **most specific wins, whole row, no merging**: `app_env` > `app` > `project`, and `UNIQUE (target_type, target_id)` makes it one policy per node. The subtraction is the part the original design documented and did not implement: `claim_due_policies` filtering `WHERE enabled AND schedule_enabled` only stops the narrow row from running its *own* scan — the parent project policy would still walk the excluded environment and persist its key paths for 90 days while the UI showed it as excluded.

- [ ] **Step 1: Write the failing tests.** Append to `targets.rs`'s `mod tests`:
  ```rust
  fn u(n: u128) -> uuid::Uuid {
      uuid::Uuid::from_u128(n)
  }

  fn pair(app: u128, env: Option<u128>) -> ScanPair {
      ScanPair { app_id: u(app), app_env_id: env.map(u) }
  }

  fn node(kind: PolicyTargetType, id: u128) -> PolicyNode {
      PolicyNode { target_type: kind, target_id: u(id) }
  }

  #[test]
  fn a_project_policy_keeps_every_pair_when_nothing_is_narrower() {
      let pairs = vec![pair(1, Some(10)), pair(1, Some(11)), pair(2, Some(20))];
      let r = resolve_targets(node(PolicyTargetType::Project, 99), &pairs, &[]);
      assert_eq!(r.pairs, pairs);
      assert_eq!(r.subtracted, 0);
  }

  /// The whole point: "most specific wins, whole row" applies to EXCLUSION as
  /// well as configuration, so the narrower row subtracts whether it is enabled
  /// or not. A disabled child policy is how an admin excludes one noisy
  /// environment, and the parent must stop walking it.
  #[test]
  fn a_narrower_app_env_row_subtracts_that_pair() {
      let pairs = vec![pair(1, Some(10)), pair(1, Some(11)), pair(2, Some(20))];
      let r = resolve_targets(
          node(PolicyTargetType::Project, 99),
          &pairs,
          &[node(PolicyTargetType::AppEnv, 11)],
      );
      assert_eq!(r.pairs, vec![pair(1, Some(10)), pair(2, Some(20))]);
      assert_eq!(r.subtracted, 1);
  }

  #[test]
  fn a_narrower_app_row_subtracts_every_pair_of_that_app() {
      let pairs = vec![pair(1, Some(10)), pair(1, Some(11)), pair(2, Some(20))];
      let r = resolve_targets(
          node(PolicyTargetType::Project, 99),
          &pairs,
          &[node(PolicyTargetType::App, 1)],
      );
      assert_eq!(r.pairs, vec![pair(2, Some(20))]);
      assert_eq!(r.subtracted, 2);
  }

  /// A policy never subtracts itself, or an app policy would resolve to nothing.
  #[test]
  fn a_policy_never_subtracts_its_own_node() {
      let pairs = vec![pair(1, Some(10)), pair(1, None)];
      let r = resolve_targets(
          node(PolicyTargetType::App, 1),
          &pairs,
          &[node(PolicyTargetType::App, 1)],
      );
      assert_eq!(r.pairs, pairs);
      assert_eq!(r.subtracted, 0);
  }

  /// `EnvFilter::Subset` uses `= ANY`, which never matches NULL, so
  /// unattributed rows are only reachable from an app- or project-scoped
  /// policy. An app_env narrower row must not silently take them away.
  #[test]
  fn an_app_env_narrower_row_leaves_the_unattributed_pair() {
      let pairs = vec![pair(1, Some(10)), pair(1, None)];
      let r = resolve_targets(
          node(PolicyTargetType::App, 1),
          &pairs,
          &[node(PolicyTargetType::AppEnv, 10)],
      );
      assert_eq!(r.pairs, vec![pair(1, None)]);
      assert_eq!(r.subtracted, 1);
  }

  /// Neither rollups nor `_default` sweeps can be environment-attributed —
  /// `event_users` and `issues` carry `app_id` only — so running them for an
  /// env-scoped policy would mean a policy an admin deliberately scoped to
  /// staging persisting key paths derived from production traffic, readable by
  /// anyone with pii:read on staging.
  #[test]
  fn rollup_and_default_classes_are_absent_for_an_app_env_policy() {
      assert!(!include_rollups(PolicyTargetType::AppEnv));
      assert!(include_rollups(PolicyTargetType::App));
      assert!(include_rollups(PolicyTargetType::Project));
  }

  #[test]
  fn the_pair_cap_truncates_and_is_reported() {
      let pairs: Vec<ScanPair> = (0..2_500).map(|i| pair(i as u128, Some(i as u128 + 10_000))).collect();
      let r = resolve_targets(node(PolicyTargetType::Project, 99), &pairs, &[]);
      assert_eq!(r.pairs.len(), MAX_SCAN_PAIRS);
      assert!(r.truncated);
  }
  ```

- [ ] **Step 2: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector targets::tests::a_narrower`. Expected: `error[E0433]: failed to resolve: use of undeclared type 'ScanPair'`.

- [ ] **Step 3: Implement.** Append to `targets.rs`, above the test module:
  ```rust
  /// Cap on the resolved `(app, enrollment)` list a single scan may carry. A
  /// project with more apps than this is a deployment-shaped problem, and a
  /// scan whose target list does not fit in one jsonb column is not resumable.
  pub const MAX_SCAN_PAIRS: usize = 2_000;

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum PolicyTargetType {
      Project,
      App,
      AppEnv,
  }

  impl PolicyTargetType {
      pub fn as_sql(self) -> &'static str {
          match self {
              PolicyTargetType::Project => "project",
              PolicyTargetType::App => "app",
              PolicyTargetType::AppEnv => "app_env",
          }
      }

      pub fn from_sql(s: &str) -> Option<PolicyTargetType> {
          match s {
              "project" => Some(PolicyTargetType::Project),
              "app" => Some(PolicyTargetType::App),
              "app_env" => Some(PolicyTargetType::AppEnv),
              _ => None,
          }
      }
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct PolicyNode {
      pub target_type: PolicyTargetType,
      /// For `AppEnv` this is an `app_environments.id` — the ENROLLMENT id,
      /// never a catalogue `environments.id`. Event rows store the enrollment
      /// id, so the other one matches nothing and the scan silently reads zero
      /// rows.
      pub target_id: uuid::Uuid,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct ScanPair {
      pub app_id: uuid::Uuid,
      /// `None` = the unattributed bucket, reachable only from an app- or
      /// project-scoped policy.
      pub app_env_id: Option<uuid::Uuid>,
  }

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct ResolvedTargets {
      pub pairs: Vec<ScanPair>,
      /// How many pairs a more-specific policy row took away. Goes into
      /// `coverage_note` so an operator can see why a scan covered less than the
      /// policy's node.
      pub subtracted: usize,
      pub truncated: bool,
  }

  /// Whether a policy at this level scans rollup tables and `_default` sweeps.
  ///
  /// False for `app_env`, because neither class can be environment-attributed:
  /// `issues` and `event_users` carry `app_id` only, and a `_default` row's
  /// `environment_id` is whatever the edge resolved, with no way to bound the
  /// sweep to one enrollment without an index that does not exist.
  pub fn include_rollups(level: PolicyTargetType) -> bool {
      !matches!(level, PolicyTargetType::AppEnv)
  }

  /// Subtract every pair covered by a MORE SPECIFIC policy row, enabled or not.
  ///
  /// A union of tracked keys across levels was rejected: it makes "turn this off
  /// for staging" inexpressible, because a narrow row could only ever add, and
  /// it would force the schedule to be merged too, which is meaningless.
  pub fn resolve_targets(
      node: PolicyNode,
      pairs: &[ScanPair],
      narrower: &[PolicyNode],
  ) -> ResolvedTargets {
      let mut kept: Vec<ScanPair> = Vec::with_capacity(pairs.len());
      let mut subtracted = 0usize;
      for p in pairs {
          let covered = narrower.iter().any(|n| {
              if *n == node {
                  return false;
              }
              match n.target_type {
                  PolicyTargetType::AppEnv => p.app_env_id == Some(n.target_id),
                  PolicyTargetType::App => p.app_id == n.target_id,
                  // A project row can only be narrower than another project row
                  // if they are the same node, which is excluded above.
                  PolicyTargetType::Project => false,
              }
          });
          if covered {
              subtracted += 1;
          } else {
              kept.push(*p);
          }
      }
      let truncated = kept.len() > MAX_SCAN_PAIRS;
      kept.truncate(MAX_SCAN_PAIRS);
      ResolvedTargets { pairs: kept, subtracted, truncated }
  }
  ```

- [ ] **Step 4: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector targets`. All seventeen tests green.

- [ ] **Step 5: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 14: `mask.rs` — the pure mask applier

**Files:**
- Create `backend/crates/sauron-inspector/src/mask.rs`
- Modify `backend/crates/sauron-inspector/src/lib.rs`

**Interfaces:**
- Consumes: `sauron_inspector::path::MaskPath` (Task 11).
- Produces: `sauron_inspector::mask::{MASK_SENTINEL, object_or_empty, apply_mask_path, apply_wire_path}`.

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-inspector/src/mask.rs` with only:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::path::parse_mask_path;
      use serde_json::json;

      fn apply(doc: &mut serde_json::Value, path: &str) -> usize {
          apply_mask_path(doc, &parse_mask_path(path).unwrap())
      }

      #[test]
      fn masks_three_levels_into_extra() {
          let mut d = json!({"a": {"b": {"email": "jane@acme.com", "keep": 1}}});
          assert_eq!(apply(&mut d, "a.b.email"), 1);
          assert_eq!(d, json!({"a": {"b": {"email": "****", "keep": 1}}}));
      }

      /// `create_missing = false` semantics: a row lacking the path is
      /// untouched. Writing the sentinel into rows that never had the field
      /// would make a `has:<key>` predicate report presence where there was
      /// none — the exact inverse of the key-removal lie the design rejects.
      #[test]
      fn a_missing_path_leaves_the_document_byte_identical() {
          let before = json!({"a": {"b": 1}});
          let mut d = before.clone();
          assert_eq!(apply(&mut d, "a.c.email"), 0);
          assert_eq!(d, before);
      }

      /// If the value at the path is an object or array, the whole subtree
      /// collapses: the subtree IS the PII.
      #[test]
      fn an_object_value_collapses_wholesale() {
          let mut d = json!({"customer": {"email": "a@b.c", "name": "Jane"}});
          assert_eq!(apply(&mut d, "customer"), 1);
          assert_eq!(d, json!({"customer": "****"}));
      }

      /// Ordinality matters: `jsonb_agg` order is not guaranteed, and the Rust
      /// applier must match the SQL lowering's guarantee, not merely happen to.
      ///
      /// `doc` here is the COLUMN VALUE, and `error_events.breadcrumbs` is an
      /// array at its root — which is why the path is a bare `[*]` and not
      /// `breadcrumbs[*]`. Same bytes the SQL `jsonb_array_elements(col)` sees.
      #[test]
      fn a_wildcard_preserves_order_and_length() {
          let mut d = json!([
              {"data": {"email": "a@b.c"}, "n": 1},
              {"data": {"other": 2}, "n": 2},
              {"data": {"email": "d@e.f"}, "n": 3}
          ]);
          assert_eq!(apply(&mut d, "[*].data.email"), 2);
          let arr = d.as_array().unwrap();
          assert_eq!(arr.len(), 3);
          assert_eq!(arr[0]["data"]["email"], json!("****"));
          assert_eq!(arr[1]["data"], json!({"other": 2}));
          assert_eq!(arr[2]["n"], json!(3));
      }

      #[test]
      fn an_empty_array_stays_an_empty_array() {
          let mut d = json!([]);
          assert_eq!(apply(&mut d, "[*].data.email"), 0);
          assert_eq!(d, json!([]));
      }

      #[test]
      fn a_wildcard_over_a_non_array_does_nothing() {
          let mut d = json!("[Circular]");
          assert_eq!(apply(&mut d, "[*].data.email"), 0);
          assert_eq!(d, json!("[Circular]"));
      }

      /// `jsonb_set` returns NULL if ANY argument is NULL, and a NULL written
      /// into a `NOT NULL DEFAULT '{}'` column is the single most likely
      /// implementation bug in this slice. The Rust side normalizes the same
      /// way the SQL `coalesce(col, '{}'::jsonb)` does.
      #[test]
      fn a_null_column_normalizes_to_an_object_not_sql_null() {
          let mut d = json!(null);
          object_or_empty(&mut d);
          assert_eq!(d, json!({}));
          let mut s = json!("[Circular]");
          object_or_empty(&mut s);
          assert_eq!(s, json!({}));
          let mut keep = json!({"a": 1});
          object_or_empty(&mut keep);
          assert_eq!(keep, json!({"a": 1}));
      }

      #[test]
      fn the_key_is_retained_never_removed() {
          let mut d = json!({"email": "a@b.c"});
          assert_eq!(apply(&mut d, "email"), 1);
          assert!(d.as_object().unwrap().contains_key("email"));
      }

      /// The wire applier takes the same wire-form string the pipeline reads out
      /// of `inspector_masked_keys.json_path`, so the enforcer and the retro-mask
      /// can never disagree about what a path means.
      #[test]
      fn the_wire_applier_accepts_the_stored_form() {
          let mut d = json!({"user": {"email": "a@b.c"}});
          assert_eq!(apply_wire_path(&mut d, "user.email"), 1);
          assert_eq!(d["user"]["email"], json!("****"));
          // An unparseable stored path is a no-op, never a panic: the pipeline
          // runs this on the ingest hot path.
          assert_eq!(apply_wire_path(&mut d, "a..b"), 0);
      }
  }
  ```

- [ ] **Step 2: Wire the module.** Add `pub mod mask;` to `lib.rs`.

- [ ] **Step 3: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector mask`. Expected: `error[E0425]: cannot find function 'apply_mask_path' in this scope`.

- [ ] **Step 4: Implement.** Prepend to `mask.rs`:
  ```rust
  //! The mask applier over an owned `serde_json::Value`.
  //!
  //! Used twice with identical semantics: by the pipeline enforcer on inbound
  //! wire payloads, and as the reference the SQL lowering's tests are written
  //! against. The value at the path becomes the JSON string `"****"` and THE KEY
  //! IS RETAINED — removing it changes row shape, breaks the `contexts`
  //! named-block structure, and makes a `has:<key>` predicate report absence
  //! where data existed, which is a second, subtler lie.
  //!
  //! Consequences that must stay in the spec, the dialog and the wiki: the TYPE
  //! changes (`extra.cart_value_cents: 4200` becomes `"****"`, so arithmetic,
  //! `@>` containment and B-tree comparison stop working for masked rows), and
  //! masking `event_user.email` breaks the shipped `user.email:` search
  //! dimension.

  use serde_json::Value;

  use crate::path::{parse_mask_path, MaskPath};

  pub const MASK_SENTINEL: &str = "****";

  /// Normalize a column value to an object, the way the SQL lowering's
  /// `coalesce(col, '{}'::jsonb)` does.
  ///
  /// `jsonb_set` returns NULL if any argument is NULL, and a NULL written into a
  /// `NOT NULL DEFAULT '{}'` column is the single most likely implementation bug
  /// in this slice. A scalar (`"[Circular]"` is real live data) normalizes the
  /// same way rather than being masked into something that looks like a value.
  pub fn object_or_empty(v: &mut Value) {
      if !v.is_object() {
          *v = Value::Object(serde_json::Map::new());
      }
  }

  /// Apply one parsed mask path. Returns how many values were replaced.
  ///
  /// `doc` is one COLUMN's value and the path is relative to it, so a wildcard
  /// iterates `doc` ITSELF — byte-for-byte the set of elements the SQL
  /// lowering's `jsonb_array_elements(col)` produces. Reaching through a named
  /// head here instead would make the ingest-time enforcer and the retro-mask
  /// disagree about what one stored `json_path` means, and the disagreement
  /// would only ever surface as data that quietly stayed raw.
  pub fn apply_mask_path(doc: &mut Value, p: &MaskPath) -> usize {
      if p.wildcard {
          let Value::Array(items) = doc else {
              return 0;
          };
          let sub = p.sub_array();
          let mut n = 0;
          for item in items.iter_mut() {
              n += set_at(item, &sub);
          }
          return n;
      }
      set_at(doc, &p.text_array())
  }

  /// Apply a stored wire-form path (`inspector_masked_keys.json_path`).
  ///
  /// An unparseable path is a NO-OP rather than an error: this runs on the
  /// ingest hot path, and a stored row written by a newer binary must never be
  /// able to drop an event.
  pub fn apply_wire_path(doc: &mut Value, wire: &str) -> usize {
      match parse_mask_path(wire) {
          Ok(p) => apply_mask_path(doc, &p),
          Err(_) => 0,
      }
  }

  /// Replace the value at `segments`, if the whole path exists. Missing =
  /// untouched, matching `jsonb_set(..., create_missing => false)`.
  fn set_at(doc: &mut Value, segments: &[String]) -> usize {
      if segments.is_empty() {
          if doc.is_null() {
              return 0;
          }
          *doc = Value::String(MASK_SENTINEL.to_string());
          return 1;
      }
      let mut cur = doc;
      for seg in &segments[..segments.len() - 1] {
          match cur.get_mut(seg) {
              Some(next) => cur = next,
              None => return 0,
          }
      }
      let last = &segments[segments.len() - 1];
      match cur.get_mut(last) {
          Some(slot) => {
              *slot = Value::String(MASK_SENTINEL.to_string());
              1
          }
          None => 0,
      }
  }
  ```

- [ ] **Step 5: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector`. Every test in the crate — columns, walk, matching, detect, redact, prefilter, path, targets, mask — green.

- [ ] **Step 6: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 15: `repo.rs` — policy CRUD, scope validation, and DST-correct scheduling

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (append a new `// === PII inspector: policies ===` section at end of file)
- Create `backend/crates/sauron-db/tests/inspector_schedule.rs`

**Interfaces:**
- Consumes: `sauron_db::models::{InspectorPolicy, NewInspectorPolicy, InspectorPolicyPatch}` (Task 2).
- Produces: `repo::{NEXT_RUN_SQL, create_inspector_policy, get_inspector_policy, list_inspector_policies_for_org, list_inspector_policies_under, patch_inspector_policy, delete_inspector_policy, reschedule_policy, claim_due_policies, validate_scope_in_org, timezone_is_valid, effective_policy_for_app, scan_pairs_for_node}`.

- [ ] **Step 1: Write the failing Postgres-backed test.** Create `backend/crates/sauron-db/tests/inspector_schedule.rs`:
  ```rust
  mod common;

  use chrono::{Datelike, NaiveTime, Utc};
  use common::TestDb;
  use sauron_db::models::NewInspectorPolicy;
  use sauron_db::repo;
  use serde_json::json;

  async fn seed_policy(
      db: &TestDb,
      org_id: uuid::Uuid,
      app_id: uuid::Uuid,
      tz: &str,
      time: NaiveTime,
      days: i16,
  ) -> uuid::Uuid {
      let mut conn = db.conn().await;
      let keys = json!([{"key": "email", "scope": "any"}]);
      let dets = json!([]);
      let rollups = json!(["issues"]);
      let p = repo::create_inspector_policy(
          &mut conn,
          NewInspectorPolicy {
              org_id,
              target_type: "app",
              target_id: app_id,
              enabled: true,
              tracked_keys: &keys,
              detectors: &dets,
              scan_columns: None,
              rollups: &rollups,
              window_days: 30,
              schedule_enabled: true,
              schedule_days: days,
              schedule_time: time,
              schedule_tz: tz,
              created_by: None,
          },
      )
      .await
      .expect("create");
      repo::reschedule_policy(&mut conn, p.id).await.expect("reschedule");
      p.id
  }

  #[tokio::test]
  async fn every_weekday_bit_produces_a_future_run() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let ids = db.seed_two_envs().await;
      let id = seed_policy(&db, ids.org_id, ids.app_id, "UTC", NaiveTime::from_hms_opt(3, 0, 0).unwrap(), 127).await;
      let mut conn = db.conn().await;
      let p = repo::get_inspector_policy(&mut conn, id).await.unwrap().unwrap();
      let next = p.next_run_at.expect("next_run_at must be materialized");
      assert!(next > Utc::now(), "next_run_at must be strictly in the future");
      db.cleanup().await;
  }

  /// A zero mask means "no days selected", which must never become due — a row
  /// that is permanently due is a row the scheduler re-claims every tick.
  #[tokio::test]
  async fn a_zero_day_mask_is_never_due() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let ids = db.seed_two_envs().await;
      let id = seed_policy(&db, ids.org_id, ids.app_id, "UTC", NaiveTime::from_hms_opt(3, 0, 0).unwrap(), 0).await;
      let mut conn = db.conn().await;
      let p = repo::get_inspector_policy(&mut conn, id).await.unwrap().unwrap();
      assert!(p.next_run_at.is_none());
      let claimed = repo::claim_due_policies(&mut conn, 10).await.unwrap();
      assert!(claimed.iter().all(|c| c.id != id));
      db.cleanup().await;
  }

  /// DST, asserted rather than discovered in a November incident. Candidates are
  /// built as LOCAL timestamps and converted back with AT TIME ZONE, so Postgres
  /// resolves DST: spring-forward yields a valid instant, fall-back yields the
  /// first occurrence — never zero runs, never double runs.
  #[tokio::test]
  async fn dst_transitions_yield_exactly_one_future_instant() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let ids = db.seed_two_envs().await;
      for tz in ["America/New_York", "Europe/Paris"] {
          let id = seed_policy(
              &db,
              ids.org_id,
              ids.app_id,
              tz,
              NaiveTime::from_hms_opt(2, 30, 0).unwrap(),
              1 << 0, // Sundays, when both zones transition
          )
          .await;
          let mut conn = db.conn().await;
          let p = repo::get_inspector_policy(&mut conn, id).await.unwrap().unwrap();
          let next = p.next_run_at.expect("a Sunday schedule must resolve");
          assert!(next > Utc::now(), "{tz}: next_run_at must be in the future");
          repo::delete_inspector_policy(&mut conn, id).await.unwrap();
      }
      db.cleanup().await;
  }

  /// The claim ALWAYS advances next_run_at, so a row can never get stuck
  /// permanently due; the worker then decides whether to actually start a scan.
  #[tokio::test]
  async fn a_claim_advances_next_run_at_past_now() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let ids = db.seed_two_envs().await;
      let id = seed_policy(&db, ids.org_id, ids.app_id, "UTC", NaiveTime::from_hms_opt(3, 0, 0).unwrap(), 127).await;
      let mut conn = db.conn().await;
      // Force it due.
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query("UPDATE inspector_policies SET next_run_at = now() - interval '1 minute' WHERE id = $1")
              .bind::<diesel::sql_types::Uuid, _>(id),
          &mut conn,
      )
      .await
      .unwrap();
      let claimed = repo::claim_due_policies(&mut conn, 10).await.unwrap();
      let row = claimed.iter().find(|c| c.id == id).expect("must be claimed");
      assert!(row.next_run_at.unwrap() > Utc::now());
      assert!(row.last_run_at.is_some());
      // A second claim in the same instant returns nothing: it is no longer due.
      let again = repo::claim_due_policies(&mut conn, 10).await.unwrap();
      assert!(again.iter().all(|c| c.id != id));
      db.cleanup().await;
  }

  #[tokio::test]
  async fn a_target_outside_the_org_is_rejected() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let ids = db.seed_two_envs().await;
      let mut conn = db.conn().await;
      assert!(repo::validate_scope_in_org(&mut conn, ids.org_id, "app", ids.app_id).await.unwrap());
      assert!(repo::validate_scope_in_org(&mut conn, ids.org_id, "project", ids.project_id).await.unwrap());
      assert!(repo::validate_scope_in_org(&mut conn, ids.org_id, "app_env", ids.env_a).await.unwrap());
      // A different org's id must not validate — without this any authenticated
      // user can mint an org, POST a policy naming a victim's app_id, and have
      // the worker scan the victim's error_events into rows carrying the
      // attacker's org_id, which is exactly what list queries filter on.
      assert!(!repo::validate_scope_in_org(&mut conn, uuid::Uuid::new_v4(), "app", ids.app_id).await.unwrap());
      assert!(!repo::validate_scope_in_org(&mut conn, ids.org_id, "app", uuid::Uuid::new_v4()).await.unwrap());
      // An unknown target_type must be a hard false, never a permissive default.
      assert!(!repo::validate_scope_in_org(&mut conn, ids.org_id, "org", ids.org_id).await.unwrap());
      db.cleanup().await;
  }

  #[tokio::test]
  async fn timezone_validation_rejects_junk() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let mut conn = db.conn().await;
      assert!(repo::timezone_is_valid(&mut conn, "Europe/Paris").await);
      assert!(repo::timezone_is_valid(&mut conn, "UTC").await);
      assert!(!repo::timezone_is_valid(&mut conn, "Mars/Olympus").await);
      assert!(!repo::timezone_is_valid(&mut conn, "'; DROP TABLE users; --").await);
      db.cleanup().await;
  }

  /// Most specific wins, whole row. An app_env policy shadows the app policy
  /// which shadows the project policy, and the resolution is a database fact
  /// because of `UNIQUE (target_type, target_id)`.
  #[tokio::test]
  async fn effective_policy_prefers_the_most_specific_node() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let ids = db.seed_two_envs().await;
      let t = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
      let mut conn = db.conn().await;
      let keys = json!(["email"]);
      let empty = json!([]);
      let rollups = json!([]);
      let mut mk = |tt: &'static str, tid: uuid::Uuid| NewInspectorPolicy {
          org_id: ids.org_id,
          target_type: tt,
          target_id: tid,
          enabled: true,
          tracked_keys: &keys,
          detectors: &empty,
          scan_columns: None,
          rollups: &rollups,
          window_days: 30,
          schedule_enabled: false,
          schedule_days: 0,
          schedule_time: t,
          schedule_tz: "UTC",
          created_by: None,
      };
      let proj = repo::create_inspector_policy(&mut conn, mk("project", ids.project_id)).await.unwrap();
      let found = repo::effective_policy_for_app(&mut conn, ids.app_id).await.unwrap().unwrap();
      assert_eq!(found.id, proj.id);
      let app = repo::create_inspector_policy(&mut conn, mk("app", ids.app_id)).await.unwrap();
      let found = repo::effective_policy_for_app(&mut conn, ids.app_id).await.unwrap().unwrap();
      assert_eq!(found.id, app.id);
      db.cleanup().await;
  }
  ```

- [ ] **Step 2: Run it and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_schedule`. Expected: `error[E0425]: cannot find function 'create_inspector_policy' in module 'repo'`.

- [ ] **Step 3: Implement the scheduling fragment and the claim.** Append to `backend/crates/sauron-db/src/repo.rs`:
  ```rust
  // ===========================================================================
  // PII inspector: policies + scheduling
  // ===========================================================================

  /// The next due instant for a policy row aliased `p`.
  ///
  /// All timezone arithmetic is Postgres's, because `chrono-tz` is not a
  /// workspace dependency: Rust cannot resolve `Europe/Paris` at all, and adding
  /// it is a workspace edit plus ~1 MB of tz data in every binary. There is also
  /// no cron parser anywhere in the repo and no cron crate in Cargo.lock, so the
  /// cadence is a 7-bit weekday mask plus a local wall-clock TIME — trivially
  /// testable in SQL with `(days >> dow) & 1`, and a 1:1 map to a row of
  /// checkboxes.
  ///
  /// Eight days of candidates always covers a once-a-week schedule. Candidates
  /// are built as LOCAL timestamps and converted back with `AT TIME ZONE`, so
  /// Postgres resolves DST: on spring-forward a 02:30 schedule resolves to a
  /// valid instant, on fall-back to the first occurrence. Never zero runs,
  /// never double runs.
  ///
  /// The update target MUST be aliased (`UPDATE inspector_policies AS p`) —
  /// this fragment references `p.*`, and the pattern it copies
  /// (`claim_due_monitors`) aliases nothing. The inner sub-select gets its own
  /// alias so the two scopes cannot collide.
  pub const NEXT_RUN_SQL: &str = "(SELECT min(ts) FROM ( \
       SELECT ((date_trunc('day', now() AT TIME ZONE p.schedule_tz) \
                + (d || ' day')::interval + p.schedule_time) \
               AT TIME ZONE p.schedule_tz) AS ts \
       FROM generate_series(0, 8) d) c \
     WHERE ((p.schedule_days >> EXTRACT(DOW FROM (c.ts AT TIME ZONE p.schedule_tz))::int) & 1) = 1 \
       AND c.ts > now())";

  /// Recompute `next_run_at`. Called after EVERY schedule-field write so the
  /// materialized due time is never stale.
  pub async fn reschedule_policy(
      conn: &mut AsyncPgConnection,
      id: Uuid,
  ) -> QueryResult<Option<DateTime<Utc>>> {
      let sql = format!(
          "UPDATE inspector_policies AS p SET next_run_at = CASE \
             WHEN p.enabled AND p.schedule_enabled AND p.schedule_days <> 0 THEN {NEXT_RUN_SQL} \
             ELSE NULL END \
           WHERE p.id = $1 RETURNING p.next_run_at"
      );
      #[derive(QueryableByName)]
      struct NextRow {
          #[diesel(sql_type = Nullable<Timestamptz>)]
          next_run_at: Option<DateTime<Utc>>,
      }
      let row: Option<NextRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(id)
          .get_result(conn)
          .await
          .optional()?;
      Ok(row.and_then(|r| r.next_run_at))
  }

  /// Claim due policies, advancing `next_run_at` in the same statement.
  ///
  /// `FOR UPDATE SKIP LOCKED` is the only concurrency primitive this repository
  /// uses — there are zero advisory locks, deliberately, because a lock held by
  /// a process that took a SIGKILL has nobody to release it and there is no
  /// shutdown handler anywhere. The claim ALWAYS advances `next_run_at`, so a
  /// row can never get stuck permanently due; the worker then decides whether to
  /// actually start a scan.
  pub async fn claim_due_policies(
      conn: &mut AsyncPgConnection,
      batch: i64,
  ) -> QueryResult<Vec<InspectorPolicy>> {
      let sql = format!(
          "UPDATE inspector_policies AS p \
           SET next_run_at = {NEXT_RUN_SQL}, last_run_at = now() \
           WHERE p.id IN ( \
             SELECT q.id FROM inspector_policies q \
             WHERE q.enabled AND q.schedule_enabled AND q.schedule_days <> 0 \
               AND q.next_run_at IS NOT NULL AND q.next_run_at <= now() \
             ORDER BY q.next_run_at FOR UPDATE SKIP LOCKED LIMIT $1 \
           ) RETURNING p.*"
      );
      diesel::sql_query(sql)
          .bind::<BigInt, _>(batch)
          .get_results(conn)
          .await
  }
  ```
  Add `#[derive(QueryableByName)]` support by confirming `InspectorPolicy` also derives it — if `claim_due_policies` fails to compile with "the trait `QueryableByName` is not implemented", add `QueryableByName` to `InspectorPolicy`'s derive list in `models.rs` and annotate `schedule_time` with `#[diesel(sql_type = diesel::sql_types::Time)]` alongside the existing attributes.

- [ ] **Step 4: Implement scope validation and timezone validation.** Append to `repo.rs`:
  ```rust
  /// Whether `(target_type, target_id)` actually lives in `org_id`.
  ///
  /// `inspector_policies.target_id` has NO foreign key (it is polymorphic, like
  /// `role_grants`), so without this any authenticated user can mint an org
  /// where they hold `org:manage` (`POST /v1/orgs` requires only `AuthUser`),
  /// POST a policy naming a victim's `app_id`, and have the worker scan the
  /// victim's `error_events` into rows carrying the attacker's `org_id` — which
  /// is exactly what every list query filters on.
  ///
  /// Called on every policy create and PATCH, AND again in the worker when the
  /// scan is claimed, because grants outlive targets.
  pub async fn validate_scope_in_org(
      conn: &mut AsyncPgConnection,
      org_id: Uuid,
      target_type: &str,
      target_id: Uuid,
  ) -> QueryResult<bool> {
      // An unknown target_type is a hard false, never a permissive default.
      let sql = match target_type {
          "project" => "SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1 AND org_id = $2) AS ok",
          "app" => {
              "SELECT EXISTS (SELECT 1 FROM apps a JOIN projects p ON p.id = a.project_id \
               WHERE a.id = $1 AND p.org_id = $2) AS ok"
          }
          // For app_env the id is an app_environments ENROLLMENT id.
          "app_env" => {
              "SELECT EXISTS (SELECT 1 FROM app_environments ae \
               JOIN apps a ON a.id = ae.app_id JOIN projects p ON p.id = a.project_id \
               WHERE ae.id = $1 AND p.org_id = $2) AS ok"
          }
          _ => return Ok(false),
      };
      #[derive(QueryableByName)]
      struct OkRow {
          #[diesel(sql_type = Bool)]
          ok: bool,
      }
      let row: OkRow = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(target_id)
          .bind::<SqlUuid, _>(org_id)
          .get_result(conn)
          .await?;
      Ok(row.ok)
  }

  /// Whether Postgres recognises this IANA timezone name.
  ///
  /// The name is bound, never interpolated, and a failure is a plain `false`
  /// rather than an error: `SET`-style timezone errors abort the surrounding
  /// statement, and this runs inside a request handler that must answer 400.
  pub async fn timezone_is_valid(conn: &mut AsyncPgConnection, tz: &str) -> bool {
      #[derive(QueryableByName)]
      struct TsRow {
          #[diesel(sql_type = diesel::sql_types::Timestamp)]
          #[allow(dead_code)]
          t: chrono::NaiveDateTime,
      }
      diesel::sql_query("SELECT now() AT TIME ZONE $1 AS t")
          .bind::<Text, _>(tz)
          .get_result::<TsRow>(conn)
          .await
          .is_ok()
  }
  ```

- [ ] **Step 5: Implement policy CRUD and resolution.** Append to `repo.rs`:
  ```rust
  pub async fn create_inspector_policy(
      conn: &mut AsyncPgConnection,
      new: NewInspectorPolicy<'_>,
  ) -> QueryResult<InspectorPolicy> {
      diesel::insert_into(inspector_policies::table)
          .values(&new)
          .returning(InspectorPolicy::as_returning())
          .get_result(conn)
          .await
  }

  pub async fn get_inspector_policy(
      conn: &mut AsyncPgConnection,
      id: Uuid,
  ) -> QueryResult<Option<InspectorPolicy>> {
      inspector_policies::table
          .find(id)
          .select(InspectorPolicy::as_select())
          .first(conn)
          .await
          .optional()
  }

  pub async fn list_inspector_policies_for_org(
      conn: &mut AsyncPgConnection,
      org_id: Uuid,
  ) -> QueryResult<Vec<InspectorPolicy>> {
      inspector_policies::table
          .filter(inspector_policies::org_id.eq(org_id))
          .select(InspectorPolicy::as_select())
          .order(inspector_policies::created_at.desc())
          .load(conn)
          .await
  }

  pub async fn patch_inspector_policy(
      conn: &mut AsyncPgConnection,
      id: Uuid,
      patch: InspectorPolicyPatch<'_>,
  ) -> QueryResult<Option<InspectorPolicy>> {
      diesel::update(inspector_policies::table.find(id))
          .set(patch)
          .returning(InspectorPolicy::as_returning())
          .get_result(conn)
          .await
          .optional()
  }

  pub async fn delete_inspector_policy(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
      diesel::delete(inspector_policies::table.find(id)).execute(conn).await
  }

  /// The policy that governs `app_id`: most specific wins, whole row.
  ///
  /// `app_env` beats `app` beats `project`, and `UNIQUE (target_type, target_id)`
  /// means there is exactly one candidate per level, so the ranking is a
  /// database fact rather than an ordering problem. An `app_env` row is only
  /// preferred when the app has exactly one live enrollment; with several, the
  /// app-level answer is the honest one for an app-scoped question.
  pub async fn effective_policy_for_app(
      conn: &mut AsyncPgConnection,
      app_id: Uuid,
  ) -> QueryResult<Option<InspectorPolicy>> {
      diesel::sql_query(
          "SELECT p.* FROM inspector_policies p \
           WHERE (p.target_type = 'app' AND p.target_id = $1) \
              OR (p.target_type = 'project' \
                  AND p.target_id = (SELECT project_id FROM apps WHERE id = $1)) \
              OR (p.target_type = 'app_env' \
                  AND p.target_id IN (SELECT id FROM app_environments WHERE app_id = $1)) \
           ORDER BY CASE p.target_type \
                      WHEN 'app_env' THEN 0 WHEN 'app' THEN 1 ELSE 2 END, p.created_at \
           LIMIT 1",
      )
      .bind::<SqlUuid, _>(app_id)
      .get_result(conn)
      .await
      .optional()
  }

  /// Every policy row whose node falls strictly UNDER `(target_type, target_id)`,
  /// enabled or not.
  ///
  /// Enabled-or-not is the point: "most specific wins, whole row" applies to
  /// EXCLUSION as well as configuration. A disabled child policy is how an admin
  /// excludes one noisy environment, and a parent that keeps walking it would
  /// persist that environment's key paths for 90 days while the UI showed it as
  /// excluded.
  pub async fn list_inspector_policies_under(
      conn: &mut AsyncPgConnection,
      target_type: &str,
      target_id: Uuid,
  ) -> QueryResult<Vec<(String, Uuid)>> {
      #[derive(QueryableByName)]
      struct NodeRow {
          #[diesel(sql_type = Text)]
          target_type: String,
          #[diesel(sql_type = SqlUuid)]
          target_id: Uuid,
      }
      let sql = match target_type {
          "project" => {
              "SELECT p.target_type, p.target_id FROM inspector_policies p \
               WHERE (p.target_type = 'app' \
                      AND p.target_id IN (SELECT id FROM apps WHERE project_id = $1)) \
                  OR (p.target_type = 'app_env' \
                      AND p.target_id IN (SELECT ae.id FROM app_environments ae \
                                          JOIN apps a ON a.id = ae.app_id \
                                          WHERE a.project_id = $1))"
          }
          "app" => {
              "SELECT p.target_type, p.target_id FROM inspector_policies p \
               WHERE p.target_type = 'app_env' \
                 AND p.target_id IN (SELECT id FROM app_environments WHERE app_id = $1)"
          }
          // Nothing is narrower than an app_env node.
          _ => return Ok(Vec::new()),
      };
      let rows: Vec<NodeRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(target_id)
          .load(conn)
          .await?;
      Ok(rows.into_iter().map(|r| (r.target_type, r.target_id)).collect())
  }

  /// Expand a policy node into ordered `(app_id, app_env_id|NULL)` pairs.
  ///
  /// The NULL pair is the unattributed bucket and is only emitted for app- and
  /// project-scoped nodes: `EnvFilter::Subset` uses `= ANY`, which never matches
  /// NULL, so those rows are unreachable from an env-scoped policy. If a
  /// deployment runs mostly `app_env` policies those rows go silently unscanned,
  /// which is what the effective-policy endpoint surfaces.
  pub async fn scan_pairs_for_node(
      conn: &mut AsyncPgConnection,
      target_type: &str,
      target_id: Uuid,
  ) -> QueryResult<Vec<(Uuid, Option<Uuid>)>> {
      #[derive(QueryableByName)]
      struct PairRow {
          #[diesel(sql_type = SqlUuid)]
          app_id: Uuid,
          #[diesel(sql_type = Nullable<SqlUuid>)]
          env_id: Option<Uuid>,
      }
      let sql = match target_type {
          "project" => {
              "SELECT a.id AS app_id, ae.id AS env_id FROM apps a \
               LEFT JOIN app_environments ae ON ae.app_id = a.id AND ae.retired_at IS NULL \
               WHERE a.project_id = $1 ORDER BY a.id, ae.id"
          }
          "app" => {
              "SELECT a.id AS app_id, ae.id AS env_id FROM apps a \
               LEFT JOIN app_environments ae ON ae.app_id = a.id AND ae.retired_at IS NULL \
               WHERE a.id = $1 ORDER BY ae.id"
          }
          "app_env" => {
              "SELECT ae.app_id AS app_id, ae.id AS env_id FROM app_environments ae \
               WHERE ae.id = $1"
          }
          _ => return Ok(Vec::new()),
      };
      let rows: Vec<PairRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(target_id)
          .load(conn)
          .await?;
      let mut out: Vec<(Uuid, Option<Uuid>)> = rows.into_iter().map(|r| (r.app_id, r.env_id)).collect();
      // The unattributed bucket, once per app, for app/project nodes only.
      if target_type != "app_env" {
          let apps: Vec<Uuid> = {
              let mut a: Vec<Uuid> = out.iter().map(|(app, _)| *app).collect();
              a.sort_unstable();
              a.dedup();
              a
          };
          for app in apps {
              out.push((app, None));
          }
      }
      Ok(out)
  }
  ```

- [ ] **Step 6: Run the DB tests and see them pass.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_schedule`. All seven tests green.

- [ ] **Step 7: Prove they SKIP without a database.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_schedule`. Expected: seven `ok` results with `TEST_DATABASE_URL unset — skipping` on stderr, never a failure. CI has no Postgres.

- [ ] **Step 8: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 16: `repo.rs` — scans: create, claim, the flush CTE, finish, cancel

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs`
- Create `backend/crates/sauron-db/tests/inspector_scan.rs`

**Interfaces:**
- Consumes: `repo::validate_scope_in_org` (Task 15), `models::{InspectorScan, NewInspectorScan}` (Task 2).
- Produces: `repo::{FindingDelta, FlushOutcome, insert_inspector_scan, claim_one_scan, flush_scan_unit, finish_scan, request_scan_cancel, get_inspector_scan, list_scans_for_policy, active_scan_for_policy}`.

> Three properties of the flush CTE are load-bearing and each has a test below: **atomicity** (deltas and cursor advance in one commit, so a SIGKILL between them is impossible), **the `worker_id` fence** (a worker stalled past its lease can have its scan reclaimed while still alive, and `match_count + excluded.match_count` would then double-count — a flush that affects zero rows MUST abort the unit), and **`findings_count` reading the CTE's own `RETURNING`, not the table** (Postgres executes all sub-statements of a data-modifying `WITH` against one snapshot and documents that they cannot see one another's effects, so a `SELECT count(*) FROM inspector_findings` there is permanently one flush behind and a single-unit scan reports 0 while `GET /findings` returns rows).

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-db/tests/inspector_scan.rs`:
  ```rust
  mod common;

  use chrono::{Duration, Utc};
  use common::TestDb;
  use sauron_db::models::{NewInspectorPolicy, NewInspectorScan};
  use sauron_db::repo::{self, FindingDelta};
  use serde_json::json;

  async fn seed_scan(db: &TestDb) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
      let ids = db.seed_two_envs().await;
      let mut conn = db.conn().await;
      let keys = json!(["email"]);
      let empty = json!([]);
      let policy = repo::create_inspector_policy(
          &mut conn,
          NewInspectorPolicy {
              org_id: ids.org_id,
              target_type: "app",
              target_id: ids.app_id,
              enabled: true,
              tracked_keys: &keys,
              detectors: &empty,
              scan_columns: None,
              rollups: &empty,
              window_days: 30,
              schedule_enabled: false,
              schedule_days: 0,
              schedule_time: chrono::NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
              schedule_tz: "UTC",
              created_by: None,
          },
      )
      .await
      .unwrap();
      let params = json!({"tracked_keys": ["email"]});
      let targets = json!([[ids.app_id, ids.env_a]]);
      let scan = repo::insert_inspector_scan(
          &mut conn,
          NewInspectorScan {
              policy_id: policy.id,
              org_id: ids.org_id,
              trigger_type: "manual",
              requested_by: None,
              window_from: Utc::now() - Duration::days(30),
              window_to: Utc::now(),
              params: &params,
              targets: &targets,
              units_total: 2,
          },
      )
      .await
      .unwrap();
      (policy.id, scan.id, ids.app_id)
  }

  fn delta(app_id: uuid::Uuid, org_id: uuid::Uuid, path: &str, n: i64) -> FindingDelta {
      FindingDelta {
          org_id,
          app_id,
          environment_id: None,
          env_scope: "no_env_column".into(),
          source_table: "error_events".into(),
          source_column: "extra".into(),
          key_path: path.into(),
          matched_key: "email".into(),
          detector: String::new(),
          value_type: "string".into(),
          match_count: n,
          match_count_exact: true,
          sample_preview: "j…m".into(),
          sample_row_id: None,
          sample_occurred_at: None,
          partition_kind: "ranged".into(),
          first_seen_at: Some(Utc::now()),
          last_seen_at: Some(Utc::now()),
      }
  }

  /// The partial unique index is what makes "one active scan per policy" a
  /// database invariant instead of a race between the API and the scheduler.
  #[tokio::test]
  async fn a_second_queued_scan_is_a_unique_violation() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (policy_id, _scan_id, _) = seed_scan(&db).await;
      let mut conn = db.conn().await;
      let params = json!({});
      let targets = json!([]);
      let err = repo::insert_inspector_scan(
          &mut conn,
          NewInspectorScan {
              policy_id,
              org_id: uuid::Uuid::new_v4(),
              trigger_type: "manual",
              requested_by: None,
              window_from: Utc::now(),
              window_to: Utc::now(),
              params: &params,
              targets: &targets,
              units_total: 0,
          },
      )
      .await;
      assert!(matches!(
          err,
          Err(diesel::result::Error::DatabaseError(
              diesel::result::DatabaseErrorKind::UniqueViolation,
              _
          ))
      ));
      // The handler turns this into a 409 with the active scan id, never a 500.
      assert!(repo::active_scan_for_policy(&mut conn, policy_id).await.unwrap().is_some());
      db.cleanup().await;
  }

  #[tokio::test]
  async fn a_claim_is_exclusive_and_stamps_the_worker() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (_p, scan_id, _) = seed_scan(&db).await;
      let mut conn = db.conn().await;
      let first = repo::claim_one_scan(&mut conn, "w1", 120).await.unwrap();
      assert_eq!(first.as_ref().map(|s| s.id), Some(scan_id));
      assert_eq!(first.unwrap().attempts, 1);
      let second = repo::claim_one_scan(&mut conn, "w2", 120).await.unwrap();
      assert!(second.is_none(), "a running, heartbeating scan must not be re-claimable");
      db.cleanup().await;
  }

  #[tokio::test]
  async fn two_flushes_accumulate_and_advance_the_cursor() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (_p, scan_id, app_id) = seed_scan(&db).await;
      let mut conn = db.conn().await;
      let claimed = repo::claim_one_scan(&mut conn, "w1", 120).await.unwrap().unwrap();
      let org_id = claimed.org_id;
      let d = vec![delta(app_id, org_id, "customer.email", 10)];
      let out = repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 1}), 1, 100, &d)
          .await
          .unwrap()
          .expect("fence must hold");
      assert_eq!(out.new_findings, 1);
      let out = repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 2}), 2, 100, &d)
          .await
          .unwrap()
          .expect("fence must hold");
      assert_eq!(out.new_findings, 0, "the second flush updates, it does not insert");
      let s = repo::get_inspector_scan(&mut conn, scan_id).await.unwrap().unwrap();
      assert_eq!(s.rows_scanned, 200);
      assert_eq!(s.units_done, 2);
      assert_eq!(s.cursor, json!({"unit": 2}));
      let f = repo::list_findings_for_scan(&mut conn, scan_id, 100, None).await.unwrap();
      assert_eq!(f.len(), 1);
      assert_eq!(f[0].match_count, 20, "counts must SUM across units, not GREATEST");
      db.cleanup().await;
  }

  /// The assertion that catches the snapshot bug: a subquery counting
  /// inspector_findings inside the same data-modifying WITH sees the table as of
  /// BEFORE the insert, so the counter is permanently one flush behind and a
  /// single-unit scan reports 0 while GET /findings returns rows.
  #[tokio::test]
  async fn findings_count_equals_the_row_count_after_one_unit() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (_p, scan_id, app_id) = seed_scan(&db).await;
      let mut conn = db.conn().await;
      let claimed = repo::claim_one_scan(&mut conn, "w1", 120).await.unwrap().unwrap();
      let d = vec![
          delta(app_id, claimed.org_id, "a.email", 3),
          delta(app_id, claimed.org_id, "b.email", 4),
      ];
      repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 1}), 1, 7, &d)
          .await
          .unwrap()
          .unwrap();
      let s = repo::get_inspector_scan(&mut conn, scan_id).await.unwrap().unwrap();
      let rows = repo::list_findings_for_scan(&mut conn, scan_id, 100, None).await.unwrap();
      assert_eq!(s.findings_count as usize, rows.len());
      db.cleanup().await;
  }

  /// A worker stalled past its lease can have its scan reclaimed while still
  /// alive. Without the fence, `match_count + excluded.match_count` double-counts
  /// silently. A flush that affects zero rows MUST abort the unit.
  #[tokio::test]
  async fn a_stale_worker_id_affects_nothing_and_does_not_move_the_cursor() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (_p, scan_id, app_id) = seed_scan(&db).await;
      let mut conn = db.conn().await;
      let claimed = repo::claim_one_scan(&mut conn, "w1", 120).await.unwrap().unwrap();
      let d = vec![delta(app_id, claimed.org_id, "a.email", 5)];
      repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 1}), 1, 5, &d).await.unwrap().unwrap();
      let ghost = repo::flush_scan_unit(&mut conn, scan_id, "zombie", &json!({"unit": 99}), 99, 5, &d)
          .await
          .unwrap();
      assert!(ghost.is_none(), "a fenced-out flush must return None");
      let s = repo::get_inspector_scan(&mut conn, scan_id).await.unwrap().unwrap();
      assert_eq!(s.cursor, json!({"unit": 1}));
      assert_eq!(s.rows_scanned, 5);
      let f = repo::list_findings_for_scan(&mut conn, scan_id, 100, None).await.unwrap();
      assert_eq!(f[0].match_count, 5, "the zombie must not have added its delta");
      db.cleanup().await;
  }

  #[tokio::test]
  async fn cancellation_surfaces_on_the_next_flush() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (_p, scan_id, app_id) = seed_scan(&db).await;
      let mut conn = db.conn().await;
      let claimed = repo::claim_one_scan(&mut conn, "w1", 120).await.unwrap().unwrap();
      assert_eq!(repo::request_scan_cancel(&mut conn, scan_id).await.unwrap(), 1);
      let d = vec![delta(app_id, claimed.org_id, "a.email", 1)];
      let out = repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 1}), 1, 1, &d)
          .await
          .unwrap()
          .unwrap();
      assert!(out.cancel_requested_at.is_some());
      repo::finish_scan(&mut conn, scan_id, "w1", "cancelled", "partial", "stopped by operator", "")
          .await
          .unwrap();
      let s = repo::get_inspector_scan(&mut conn, scan_id).await.unwrap().unwrap();
      assert_eq!(s.status, "cancelled");
      assert!(s.finished_at.is_some());
      db.cleanup().await;
  }

  #[tokio::test]
  async fn a_scan_whose_heartbeat_expired_is_reclaimable() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (_p, scan_id, _) = seed_scan(&db).await;
      let mut conn = db.conn().await;
      repo::claim_one_scan(&mut conn, "w1", 120).await.unwrap().unwrap();
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query("UPDATE inspector_scans SET heartbeat_at = now() - interval '10 minutes' WHERE id = $1")
              .bind::<diesel::sql_types::Uuid, _>(scan_id),
          &mut conn,
      )
      .await
      .unwrap();
      let again = repo::claim_one_scan(&mut conn, "w2", 120).await.unwrap().unwrap();
      assert_eq!(again.worker_id.as_deref(), Some("w2"));
      assert_eq!(again.attempts, 2);
      db.cleanup().await;
  }
  ```

- [ ] **Step 2: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_scan`. Expected: `error[E0432]: unresolved import 'sauron_db::repo::FindingDelta'`.

- [ ] **Step 3: Implement create, claim, finish and cancel.** Append to `repo.rs`:
  ```rust
  // ===========================================================================
  // PII inspector: scans
  // ===========================================================================

  /// Insert a scan. A `UniqueViolation` here is the partial unique index
  /// `inspector_scans_active_key` refusing a second queued/running scan for the
  /// policy — the handler answers 409 with the active scan id, never 500.
  pub async fn insert_inspector_scan(
      conn: &mut AsyncPgConnection,
      new: NewInspectorScan<'_>,
  ) -> QueryResult<InspectorScan> {
      diesel::insert_into(inspector_scans::table)
          .values(&new)
          .returning(InspectorScan::as_returning())
          .get_result(conn)
          .await
  }

  pub async fn active_scan_for_policy(
      conn: &mut AsyncPgConnection,
      policy_id: Uuid,
  ) -> QueryResult<Option<InspectorScan>> {
      inspector_scans::table
          .filter(inspector_scans::policy_id.eq(policy_id))
          .filter(inspector_scans::status.eq_any(vec!["queued", "running"]))
          .select(InspectorScan::as_select())
          .first(conn)
          .await
          .optional()
  }

  /// Claim one scan, copying `claim_due_monitors` verbatim in shape.
  ///
  /// This is what makes N replicas safe, unlike `sauron-alerts` (no claim) and
  /// `sauron-tier` (a watermark row with no locking). Re-claiming a `running`
  /// row whose heartbeat expired IS the crash-resume mechanism; the caller
  /// finalizes the scan as `failed` once `attempts > inspector_max_attempts` so
  /// one poison unit cannot loop forever.
  pub async fn claim_one_scan(
      conn: &mut AsyncPgConnection,
      worker_id: &str,
      lease_secs: i64,
  ) -> QueryResult<Option<InspectorScan>> {
      diesel::sql_query(
          "UPDATE inspector_scans SET status='running', worker_id=$1, heartbeat_at=now(), \
                  attempts=attempts+1, started_at=COALESCE(started_at, now()) \
           WHERE id IN (SELECT id FROM inspector_scans \
                        WHERE status='queued' \
                           OR (status='running' AND heartbeat_at < now() - make_interval(secs => $2)) \
                        ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1) \
           RETURNING *",
      )
      .bind::<Text, _>(worker_id)
      .bind::<BigInt, _>(lease_secs)
      .get_result(conn)
      .await
      .optional()
  }

  #[allow(clippy::too_many_arguments)]
  pub async fn finish_scan(
      conn: &mut AsyncPgConnection,
      scan_id: Uuid,
      worker_id: &str,
      status: &str,
      coverage: &str,
      coverage_note: &str,
      error: &str,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_scans SET status=$3, coverage=$4, coverage_note=$5, error=$6, \
                  finished_at=now(), heartbeat_at=now() \
           WHERE id=$1 AND worker_id=$2",
      )
      .bind::<SqlUuid, _>(scan_id)
      .bind::<Text, _>(worker_id)
      .bind::<Text, _>(status)
      .bind::<Text, _>(coverage)
      .bind::<Text, _>(coverage_note)
      .bind::<Text, _>(error)
      .execute(conn)
      .await
  }

  /// Ask a running scan to stop. The worker observes this on the `RETURNING` of
  /// the next flush — a write it was making anyway.
  pub async fn request_scan_cancel(conn: &mut AsyncPgConnection, scan_id: Uuid) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_scans SET cancel_requested_at = COALESCE(cancel_requested_at, now()) \
           WHERE id = $1 AND status IN ('queued','running')",
      )
      .bind::<SqlUuid, _>(scan_id)
      .execute(conn)
      .await
  }

  pub async fn get_inspector_scan(
      conn: &mut AsyncPgConnection,
      id: Uuid,
  ) -> QueryResult<Option<InspectorScan>> {
      inspector_scans::table
          .find(id)
          .select(InspectorScan::as_select())
          .first(conn)
          .await
          .optional()
  }

  pub async fn list_scans_for_policy(
      conn: &mut AsyncPgConnection,
      policy_id: Uuid,
      limit: i64,
  ) -> QueryResult<Vec<InspectorScan>> {
      inspector_scans::table
          .filter(inspector_scans::policy_id.eq(policy_id))
          .select(InspectorScan::as_select())
          .order(inspector_scans::created_at.desc())
          .limit(limit.clamp(1, 200))
          .load(conn)
          .await
  }
  ```

- [ ] **Step 4: Implement the flush CTE.** Append to `repo.rs`:
  ```rust
  /// One unit's aggregated result, ready to be folded into `inspector_findings`.
  #[derive(Debug, Clone)]
  pub struct FindingDelta {
      pub org_id: Uuid,
      pub app_id: Uuid,
      pub environment_id: Option<Uuid>,
      pub env_scope: String,
      pub source_table: String,
      pub source_column: String,
      pub key_path: String,
      pub matched_key: String,
      pub detector: String,
      pub value_type: String,
      pub match_count: i64,
      pub match_count_exact: bool,
      pub sample_preview: String,
      pub sample_row_id: Option<Uuid>,
      pub sample_occurred_at: Option<DateTime<Utc>>,
      pub partition_kind: String,
      pub first_seen_at: Option<DateTime<Utc>>,
      pub last_seen_at: Option<DateTime<Utc>>,
  }

  #[derive(Debug, Clone, Copy)]
  pub struct FlushOutcome {
      /// How many rows the CTE actually INSERTED (as opposed to updated).
      pub new_findings: i64,
      /// Set once an operator has asked the scan to stop.
      pub cancel_requested_at: Option<DateTime<Utc>>,
  }

  /// Persist one unit's findings AND advance the cursor in ONE data-modifying
  /// CTE. There is no `conn.transaction` in this repository (MSRV 1.82).
  ///
  /// Three properties are load-bearing:
  ///
  /// ATOMICITY. The deltas and the cursor advance in one commit, so a SIGKILL
  /// between them is impossible and re-running the lost range re-adds exact
  /// counts from the last durable cursor. Counts stay correct without
  /// `GREATEST`-style deduplication — which would be correct across re-runs but
  /// WRONG across units, which must sum.
  ///
  /// THE `worker_id` FENCE. A worker stalled past the lease (GC, IO) can have
  /// its scan reclaimed while still alive, and `match_count +
  /// excluded.match_count` would then double-count. A flush that affects zero
  /// rows returns `None` and the caller MUST abort the unit. Any refactor that
  /// drops the fence silently corrupts counts.
  ///
  /// `findings_count` READS THE CTE, NOT THE TABLE. Postgres executes all
  /// sub-statements of a data-modifying `WITH` against one snapshot and
  /// documents that they cannot see one another's effects, so
  /// `(SELECT count(*) FROM inspector_findings WHERE scan_id = $1)` here counts
  /// the table as of BEFORE `f` ran: the counter is permanently one flush
  /// behind, the final flush's findings are never counted, and a single-unit
  /// scan reports 0 while `GET /findings` returns rows. It is also an aggregate
  /// over the whole finding set on every flush — hundreds of millions of index
  /// tuples over a scan, on the connection that is supposed to be duty-cycled.
  #[allow(clippy::too_many_arguments)]
  pub async fn flush_scan_unit(
      conn: &mut AsyncPgConnection,
      scan_id: Uuid,
      worker_id: &str,
      cursor: &Value,
      units_done: i32,
      rows_delta: i64,
      deltas: &[FindingDelta],
  ) -> QueryResult<Option<FlushOutcome>> {
      // Columnar unnest: one bound array per column keeps the statement text
      // constant regardless of how many findings a unit produced, so Postgres
      // reuses the plan instead of parsing a fresh VALUES list every flush.
      let org_ids: Vec<Uuid> = deltas.iter().map(|d| d.org_id).collect();
      let app_ids: Vec<Uuid> = deltas.iter().map(|d| d.app_id).collect();
      let env_ids: Vec<Option<Uuid>> = deltas.iter().map(|d| d.environment_id).collect();
      let env_scopes: Vec<String> = deltas.iter().map(|d| d.env_scope.clone()).collect();
      let tables: Vec<String> = deltas.iter().map(|d| d.source_table.clone()).collect();
      let columns: Vec<String> = deltas.iter().map(|d| d.source_column.clone()).collect();
      let paths: Vec<String> = deltas.iter().map(|d| d.key_path.clone()).collect();
      let keys: Vec<String> = deltas.iter().map(|d| d.matched_key.clone()).collect();
      let dets: Vec<String> = deltas.iter().map(|d| d.detector.clone()).collect();
      let types: Vec<String> = deltas.iter().map(|d| d.value_type.clone()).collect();
      let counts: Vec<i64> = deltas.iter().map(|d| d.match_count).collect();
      let exacts: Vec<bool> = deltas.iter().map(|d| d.match_count_exact).collect();
      let previews: Vec<String> = deltas.iter().map(|d| d.sample_preview.clone()).collect();
      let row_ids: Vec<Option<Uuid>> = deltas.iter().map(|d| d.sample_row_id).collect();
      let occurred: Vec<Option<DateTime<Utc>>> = deltas.iter().map(|d| d.sample_occurred_at).collect();
      let kinds: Vec<String> = deltas.iter().map(|d| d.partition_kind.clone()).collect();
      let firsts: Vec<Option<DateTime<Utc>>> = deltas.iter().map(|d| d.first_seen_at).collect();
      let lasts: Vec<Option<DateTime<Utc>>> = deltas.iter().map(|d| d.last_seen_at).collect();

      #[derive(QueryableByName)]
      struct FlushRow {
          #[diesel(sql_type = BigInt)]
          inserted: i64,
          #[diesel(sql_type = Nullable<Timestamptz>)]
          cancel_requested_at: Option<DateTime<Utc>>,
      }

      let row: Option<FlushRow> = diesel::sql_query(
          "WITH me AS (SELECT id FROM inspector_scans WHERE id = $1 AND worker_id = $2), \
           f AS ( \
             INSERT INTO inspector_findings ( \
               scan_id, org_id, app_id, environment_id, env_scope, source_table, source_column, \
               key_path, matched_key, detector, value_type, match_count, match_count_exact, \
               sample_preview, sample_row_id, sample_occurred_at, partition_kind, \
               first_seen_at, last_seen_at) \
             SELECT $1, u.org_id, u.app_id, u.env_id, u.env_scope, u.src_table, u.src_column, \
                    u.key_path, u.matched_key, u.detector, u.value_type, u.match_count, u.exact, \
                    u.preview, u.row_id, u.occurred, u.kind, u.first_seen, u.last_seen \
             FROM unnest($6::uuid[], $7::uuid[], $8::uuid[], $9::text[], $10::text[], $11::text[], \
                         $12::text[], $13::text[], $14::text[], $15::text[], $16::bigint[], \
                         $17::bool[], $18::text[], $19::uuid[], $20::timestamptz[], $21::text[], \
                         $22::timestamptz[], $23::timestamptz[]) \
                  AS u(org_id, app_id, env_id, env_scope, src_table, src_column, key_path, \
                       matched_key, detector, value_type, match_count, exact, preview, row_id, \
                       occurred, kind, first_seen, last_seen) \
             WHERE EXISTS (SELECT 1 FROM me) \
             ON CONFLICT (scan_id, app_id, env_scope, \
                          COALESCE(environment_id,'00000000-0000-0000-0000-000000000000'::uuid), \
                          source_table, source_column, key_path, detector) \
             DO UPDATE SET \
               match_count = inspector_findings.match_count + excluded.match_count, \
               last_seen_at = GREATEST(inspector_findings.last_seen_at, excluded.last_seen_at), \
               first_seen_at = LEAST(inspector_findings.first_seen_at, excluded.first_seen_at), \
               match_count_exact = inspector_findings.match_count_exact AND excluded.match_count_exact \
             RETURNING (xmax = 0) AS inserted \
           ) \
           UPDATE inspector_scans SET \
             cursor = $3, units_done = $4, \
             rows_scanned = rows_scanned + $5, \
             findings_count = findings_count + \
                 (SELECT count(*) FROM f WHERE inserted)::int, \
             heartbeat_at = now() \
           WHERE id = $1 AND worker_id = $2 \
           RETURNING (SELECT count(*) FROM f WHERE inserted)::bigint AS inserted, \
                     cancel_requested_at",
      )
      .bind::<SqlUuid, _>(scan_id)
      .bind::<Text, _>(worker_id)
      .bind::<Jsonb, _>(cursor)
      .bind::<Integer, _>(units_done)
      .bind::<BigInt, _>(rows_delta)
      .bind::<Array<SqlUuid>, _>(org_ids)
      .bind::<Array<SqlUuid>, _>(app_ids)
      .bind::<Array<Nullable<SqlUuid>>, _>(env_ids)
      .bind::<Array<Text>, _>(env_scopes)
      .bind::<Array<Text>, _>(tables)
      .bind::<Array<Text>, _>(columns)
      .bind::<Array<Text>, _>(paths)
      .bind::<Array<Text>, _>(keys)
      .bind::<Array<Text>, _>(dets)
      .bind::<Array<Text>, _>(types)
      .bind::<Array<BigInt>, _>(counts)
      .bind::<Array<Bool>, _>(exacts)
      .bind::<Array<Text>, _>(previews)
      .bind::<Array<Nullable<SqlUuid>>, _>(row_ids)
      .bind::<Array<Nullable<Timestamptz>>, _>(occurred)
      .bind::<Array<Text>, _>(kinds)
      .bind::<Array<Nullable<Timestamptz>>, _>(firsts)
      .bind::<Array<Nullable<Timestamptz>>, _>(lasts)
      .get_result(conn)
      .await
      .optional()?;

      Ok(row.map(|r| FlushOutcome {
          new_findings: r.inserted,
          cancel_requested_at: r.cancel_requested_at,
      }))
  }
  ```
  If `Array` is not already imported at the top of `repo.rs`, add it to the `use diesel::sql_types::{...}` list along with `Jsonb` and `Timestamptz`.

- [ ] **Step 5: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_scan`. All seven tests green. `list_findings_for_scan` is defined in Task 17 — if it is not yet present, temporarily assert against a raw `SELECT count(*) FROM inspector_findings WHERE scan_id = $1` and restore the call in Task 17 Step 5.

- [ ] **Step 6: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 17: `repo.rs` — findings listing, the reveal read, and the reveal audit row

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs`
- Modify `backend/crates/sauron-db/tests/inspector_scan.rs` (append two tests)

**Interfaces:**
- Consumes: `FindingDelta`/`flush_scan_unit` (Task 16), `sauron_inspector::columns::find` (Task 5) — reached from the API, not from `repo.rs`.
- Produces: `repo::{list_findings_for_scan, get_inspector_finding, count_findings_for_scan, reveal_one_value, insert_reveal_audit, first_error_event_locator}`.

- [ ] **Step 1: Write the failing tests.** Append to `backend/crates/sauron-db/tests/inspector_scan.rs`:
  ```rust
  /// The `app_id` predicate on the reveal read is NOT redundant. Without it the
  /// tenant decision rests entirely on `inspector_findings.app_id` being correct
  /// — a worker-written value with no constraint tying it to the row
  /// `sample_row_id` points at — so any attribution bug converts silently into
  /// cross-tenant raw-PII disclosure. It costs nothing: `app_id` leads
  /// `error_events_app_env_time_idx`.
  #[tokio::test]
  async fn reveal_returns_none_for_a_mismatched_app_id() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let ids = db.seed_two_envs().await;
      let mut conn = db.conn().await;
      let row: Option<(uuid::Uuid, chrono::DateTime<Utc>)> =
          repo::first_error_event_locator(&mut conn, ids.app_id).await.unwrap();
      let (event_id, occurred_at) = row.expect("the harness seeds error events");

      let hit = repo::reveal_one_value(&mut conn, "error_events", "extra", event_id, Some(occurred_at), ids.app_id)
          .await
          .unwrap();
      assert!(hit.is_some(), "the real locator must resolve");

      let miss = repo::reveal_one_value(
          &mut conn,
          "error_events",
          "extra",
          event_id,
          Some(occurred_at),
          uuid::Uuid::new_v4(),
      )
      .await
      .unwrap();
      assert!(miss.is_none(), "a mismatched app_id must be a benign miss, not a disclosure");

      // A dropped partition or a replaced rollup row is the same shape: 410.
      let gone = repo::reveal_one_value(
          &mut conn,
          "error_events",
          "extra",
          uuid::Uuid::new_v4(),
          Some(occurred_at),
          ids.app_id,
      )
      .await
      .unwrap();
      assert!(gone.is_none());
      db.cleanup().await;
  }

  #[tokio::test]
  async fn findings_list_is_ordered_by_match_count_and_keysets() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (_p, scan_id, app_id) = seed_scan(&db).await;
      let mut conn = db.conn().await;
      let claimed = repo::claim_one_scan(&mut conn, "w1", 120).await.unwrap().unwrap();
      let d = vec![
          delta(app_id, claimed.org_id, "a.email", 1),
          delta(app_id, claimed.org_id, "b.email", 50),
          delta(app_id, claimed.org_id, "c.email", 20),
      ];
      repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 1}), 1, 71, &d).await.unwrap().unwrap();
      let page = repo::list_findings_for_scan(&mut conn, scan_id, 2, None).await.unwrap();
      assert_eq!(page.len(), 2);
      assert_eq!(page[0].match_count, 50);
      assert_eq!(page[1].match_count, 20);
      let next = repo::list_findings_for_scan(&mut conn, scan_id, 2, Some((20, page[1].id)))
          .await
          .unwrap();
      assert_eq!(next.len(), 1);
      assert_eq!(next[0].match_count, 1);
      assert_eq!(repo::count_findings_for_scan(&mut conn, scan_id).await.unwrap(), 3);
      db.cleanup().await;
  }
  ```

- [ ] **Step 2: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_scan reveal`. Expected: `error[E0599]: no function or associated item named 'reveal_one_value'`.

- [ ] **Step 3: Implement the listing and count.** Append to `repo.rs`:
  ```rust
  // ===========================================================================
  // PII inspector: findings + reveal
  // ===========================================================================

  /// Findings for a scan, biggest first, keyset-paginated on
  /// `(match_count DESC, id)`. OFFSET is not offered: Postgres must walk and
  /// discard every skipped row, so deep paging over a 33k-finding scan turns a
  /// cheap request into a full ordered scan.
  pub async fn list_findings_for_scan(
      conn: &mut AsyncPgConnection,
      scan_id: Uuid,
      limit: i64,
      after: Option<(i64, Uuid)>,
  ) -> QueryResult<Vec<InspectorFinding>> {
      let limit = limit.clamp(1, 1_000);
      match after {
          Some((count, id)) => {
              inspector_findings::table
                  .filter(inspector_findings::scan_id.eq(scan_id))
                  .filter(
                      inspector_findings::match_count.lt(count).or(inspector_findings::match_count
                          .eq(count)
                          .and(inspector_findings::id.gt(id))),
                  )
                  .select(InspectorFinding::as_select())
                  .order((inspector_findings::match_count.desc(), inspector_findings::id.asc()))
                  .limit(limit)
                  .load(conn)
                  .await
          }
          None => {
              inspector_findings::table
                  .filter(inspector_findings::scan_id.eq(scan_id))
                  .select(InspectorFinding::as_select())
                  .order((inspector_findings::match_count.desc(), inspector_findings::id.asc()))
                  .limit(limit)
                  .load(conn)
                  .await
          }
      }
  }

  pub async fn count_findings_for_scan(
      conn: &mut AsyncPgConnection,
      scan_id: Uuid,
  ) -> QueryResult<i64> {
      inspector_findings::table
          .filter(inspector_findings::scan_id.eq(scan_id))
          .count()
          .get_result(conn)
          .await
  }

  pub async fn get_inspector_finding(
      conn: &mut AsyncPgConnection,
      id: Uuid,
  ) -> QueryResult<Option<InspectorFinding>> {
      inspector_findings::table
          .find(id)
          .select(InspectorFinding::as_select())
          .first(conn)
          .await
          .optional()
  }
  ```

- [ ] **Step 4: Implement the reveal read and its audit row.** Append to `repo.rs`:
  ```rust
  /// One live single-row read behind `POST /findings/{id}/reveal`.
  ///
  /// `table` and `column` are `&'static str`s from `sauron_inspector::columns`,
  /// never caller bytes — SQL identifiers cannot be bound, so the caller MUST
  /// have resolved them through the inventory first.
  ///
  /// The `app_id` predicate is not redundant: without it the tenant decision
  /// rests entirely on `inspector_findings.app_id` being correct, a
  /// worker-written value with no constraint tying it to the row
  /// `sample_row_id` points at. Any attribution bug would convert silently into
  /// cross-tenant raw-PII disclosure. `occurred_at` is mandatory for a
  /// partitioned source so the query prunes to one child.
  ///
  /// `None` is a 410 Gone: the partition was dropped by `sauron-tier`, the
  /// rollup row was replaced, or the locator points at another tenant. Nothing
  /// is persisted by this call.
  pub async fn reveal_one_value(
      conn: &mut AsyncPgConnection,
      table: &'static str,
      column: &'static str,
      row_id: Uuid,
      occurred_at: Option<DateTime<Utc>>,
      app_id: Uuid,
  ) -> QueryResult<Option<Value>> {
      #[derive(QueryableByName)]
      struct ValRow {
          #[diesel(sql_type = Nullable<Jsonb>)]
          v: Option<Value>,
      }
      // `to_jsonb` normalizes the TEXT columns into the same shape as the jsonb
      // ones so the handler has one extraction path instead of two.
      let sql = match occurred_at {
          Some(_) => format!(
              "SELECT to_jsonb({column}) AS v FROM {table} \
               WHERE id = $1 AND occurred_at = $2 AND app_id = $3"
          ),
          None => format!("SELECT to_jsonb({column}) AS v FROM {table} WHERE id = $1 AND app_id = $3"),
      };
      let q = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(row_id)
          .bind::<Nullable<Timestamptz>, _>(occurred_at)
          .bind::<SqlUuid, _>(app_id);
      let row: Option<ValRow> = q.get_result(conn).await.optional()?;
      Ok(row.and_then(|r| r.v))
  }

  /// Record who revealed what, BEFORE the value is returned, so a failure to
  /// audit is a failure to reveal.
  pub async fn insert_reveal_audit(
      conn: &mut AsyncPgConnection,
      new: NewInspectorRevealAudit<'_>,
  ) -> QueryResult<usize> {
      diesel::insert_into(inspector_reveal_audit::table)
          .values(&new)
          .execute(conn)
          .await
  }

  /// A real `(id, occurred_at)` locator for `app_id`, for tests and for the
  /// storage report's sanity checks. Returns `None` on an app with no errors.
  pub async fn first_error_event_locator(
      conn: &mut AsyncPgConnection,
      app_id: Uuid,
  ) -> QueryResult<Option<(Uuid, DateTime<Utc>)>> {
      #[derive(QueryableByName)]
      struct LocRow {
          #[diesel(sql_type = SqlUuid)]
          id: Uuid,
          #[diesel(sql_type = Timestamptz)]
          occurred_at: DateTime<Utc>,
      }
      let row: Option<LocRow> = diesel::sql_query(
          "SELECT id, occurred_at FROM error_events WHERE app_id = $1 ORDER BY occurred_at DESC LIMIT 1",
      )
      .bind::<SqlUuid, _>(app_id)
      .get_result(conn)
      .await
      .optional()?;
      Ok(row.map(|r| (r.id, r.occurred_at)))
  }
  ```
  Note the `None` arm of `reveal_one_value` still binds `$2` even though the SQL does not use it — Postgres rejects an unused parameter, so change that arm's placeholders to `$1`/`$2` and bind only `row_id` and `app_id` in a separate branch if the query errors with `bind message supplies 3 parameters, but prepared statement requires 2`.

- [ ] **Step 5: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_scan`. All nine tests green. If Task 16 Step 5 left a temporary raw-SQL assertion in place, restore the `list_findings_for_scan` call now.

- [ ] **Step 6: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 18: `repo.rs` — mask actions, masked keys, and the `upsert_issue` sticky guard

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (new section; plus `upsert_issue` at line ~1410)
- Create `backend/crates/sauron-db/tests/inspector_mask.rs`

**Interfaces:**
- Consumes: `models::{InspectorMaskAction, NewInspectorMaskAction, InspectorMaskedKey, NewInspectorMaskedKey}` (Task 3).
- Produces: `repo::{insert_mask_action, get_mask_action, list_mask_actions_for_app, list_mask_actions_for_org, finish_preview, confirm_mask_action, cancel_mask_action, claim_mask_action, fail_mask_action, finish_mask_action, set_mask_phase, insert_masked_keys, masked_keys_for_app, list_masked_keys, cancel_pending_mask_actions_for_user}`.

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-db/tests/inspector_mask.rs`:
  ```rust
  mod common;

  use chrono::Utc;
  use common::TestDb;
  use sauron_db::models::{NewInspectorMaskAction, NewInspectorMaskedKey};
  use sauron_db::repo;
  use serde_json::json;

  async fn seed_action(db: &TestDb, kind: &str) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
      let ids = db.seed_two_envs().await;
      let mut conn = db.conn().await;
      let targets = json!([{"table": "error_events", "column": "extra", "path": "customer.email"}]);
      let a = repo::insert_mask_action(
          &mut conn,
          NewInspectorMaskAction {
              org_id: ids.org_id,
              app_id: ids.app_id,
              kind,
              finding_id: None,
              scan_id: None,
              targets: &targets,
              requested_by: None,
              requested_by_email: "admin@example.com",
          },
      )
      .await
      .unwrap();
      (a.id, ids.app_id, ids.org_id)
  }

  /// Two independent claim slots. Routing previews through the mask FIFO means a
  /// preview requested while a multi-hour mask runs expires before it is ever
  /// computed, and confirm — which requires `previewed` — becomes permanently
  /// impossible on a busy app.
  #[tokio::test]
  async fn preview_and_mask_claim_slots_are_independent() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (preview_id, _app, _org) = seed_action(&db, "preview").await;
      let mut conn = db.conn().await;
      // A preview sits in status='preview' and is invisible to the mask slot.
      assert!(repo::claim_mask_action(&mut conn, "mask", "w1", 300).await.unwrap().is_none());
      let claimed = repo::claim_mask_action(&mut conn, "preview", "w1", 300).await.unwrap().unwrap();
      assert_eq!(claimed.id, preview_id);
      assert_eq!(claimed.phase, "counting");
      db.cleanup().await;
  }

  #[tokio::test]
  async fn confirm_requires_a_fresh_preview_and_a_ceiling() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (id, _app, _org) = seed_action(&db, "preview").await;
      let mut conn = db.conn().await;
      repo::claim_mask_action(&mut conn, "preview", "w1", 300).await.unwrap();
      repo::finish_preview(&mut conn, id, "w1", 1_000, 5, Some(Utc::now())).await.unwrap();

      // A wrong ceiling refuses.
      assert_eq!(repo::confirm_mask_action(&mut conn, id, "ip=1.2.3.4 (xff)", 900, 100).await.unwrap(), 0);
      // A fresh preview under the ceiling promotes to `pending`.
      assert_eq!(repo::confirm_mask_action(&mut conn, id, "ip=1.2.3.4 (xff)", 900, 20_000_000).await.unwrap(), 1);
      let a = repo::get_mask_action(&mut conn, id).await.unwrap().unwrap();
      assert_eq!(a.status, "pending");
      assert_eq!(a.kind, "mask", "confirm flips kind so the mask slot can see it");
      assert!(a.confirmed_at.is_some());
      // A second confirm is a no-op, so a double-click cannot enqueue twice.
      assert_eq!(repo::confirm_mask_action(&mut conn, id, "ip=1.2.3.4 (xff)", 900, 20_000_000).await.unwrap(), 0);
      db.cleanup().await;
  }

  /// The TTL is measured from `previewed_at` — the preview COMPLETING — not from
  /// the request, or a queued preview expires before it is readable.
  #[tokio::test]
  async fn a_stale_preview_cannot_be_confirmed() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (id, _app, _org) = seed_action(&db, "preview").await;
      let mut conn = db.conn().await;
      repo::claim_mask_action(&mut conn, "preview", "w1", 300).await.unwrap();
      repo::finish_preview(&mut conn, id, "w1", 10, 0, Some(Utc::now())).await.unwrap();
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query("UPDATE inspector_mask_actions SET previewed_at = now() - interval '2 hours' WHERE id = $1")
              .bind::<diesel::sql_types::Uuid, _>(id),
          &mut conn,
      )
      .await
      .unwrap();
      assert_eq!(repo::confirm_mask_action(&mut conn, id, "ip=x", 900, 20_000_000).await.unwrap(), 0);
      db.cleanup().await;
  }

  /// Cancel is attributable. In an audit table whose whole justification is "who
  /// did it", the one adversarial action the design permits — stopping a
  /// redaction — must not be the one it cannot attribute.
  #[tokio::test]
  async fn cancel_records_who_and_only_from_a_live_state() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (id, _app, _org) = seed_action(&db, "mask").await;
      let mut conn = db.conn().await;
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query("UPDATE inspector_mask_actions SET status='running' WHERE id=$1")
              .bind::<diesel::sql_types::Uuid, _>(id),
          &mut conn,
      )
      .await
      .unwrap();
      assert_eq!(repo::cancel_mask_action(&mut conn, id, None, "ops@example.com").await.unwrap(), 1);
      let a = repo::get_mask_action(&mut conn, id).await.unwrap().unwrap();
      assert_eq!(a.status, "cancelling");
      assert_eq!(a.cancelled_by_email, "ops@example.com");
      assert!(a.cancelled_at.is_some());
      // A terminal action refuses: the handler answers 409.
      repo::finish_mask_action(&mut conn, id, "w1", "cancelled", true, Some(Utc::now())).await.unwrap();
      assert_eq!(repo::cancel_mask_action(&mut conn, id, None, "ops@example.com").await.unwrap(), 0);
      db.cleanup().await;
  }

  #[tokio::test]
  async fn masked_keys_are_idempotent_per_app_and_path() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let ids = db.seed_two_envs().await;
      let mut conn = db.conn().await;
      let rows = vec![
          NewInspectorMaskedKey {
              app_id: ids.app_id,
              target_table: "error_events",
              target_column: "extra",
              json_path: "customer.email",
              created_by: None,
              source_action_id: None,
          },
          NewInspectorMaskedKey {
              app_id: ids.app_id,
              target_table: "error_events",
              target_column: "extra",
              json_path: "customer.email",
              created_by: None,
              source_action_id: None,
          },
      ];
      repo::insert_masked_keys(&mut conn, &rows).await.unwrap();
      repo::insert_masked_keys(&mut conn, &rows).await.unwrap();
      let loaded = repo::masked_keys_for_app(&mut conn, ids.app_id).await.unwrap();
      assert_eq!(loaded.len(), 1, "re-masking the same finding must be idempotent");
      db.cleanup().await;
  }

  /// Forward enforcement alone leaves two gaps on `issues.title` — PII inside
  /// `exception_type`, which `build_title` also concatenates, and the 30s cache
  /// window — and both restore the raw string on the very next occurrence.
  #[tokio::test]
  async fn a_masked_issue_title_stays_masked_across_upserts() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let ids = db.seed_two_envs().await;
      let mut conn = db.conn().await;
      let fp = format!("sticky-{}", uuid::Uuid::new_v4());
      let first = sauron_db::models::NewIssue {
          app_id: ids.app_id,
          fingerprint: &fp,
          type_: "error",
          title: "TypeError: jane@acme.com is not a function",
          culprit: "checkout",
          level: "error",
          first_seen: Utc::now(),
          last_seen: Utc::now(),
      };
      let issue_id = repo::upsert_issue(&mut conn, first).await.unwrap();
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query("UPDATE issues SET title='****', culprit='****' WHERE id=$1")
              .bind::<diesel::sql_types::Uuid, _>(issue_id),
          &mut conn,
      )
      .await
      .unwrap();
      let again = sauron_db::models::NewIssue {
          app_id: ids.app_id,
          fingerprint: &fp,
          type_: "error",
          title: "TypeError: jane@acme.com is not a function",
          culprit: "checkout",
          level: "error",
          first_seen: Utc::now(),
          last_seen: Utc::now(),
      };
      repo::upsert_issue(&mut conn, again).await.unwrap();
      let issue = repo::get_issue_row(&mut conn, issue_id).await.unwrap().unwrap();
      assert_eq!(issue.title, "****", "the sticky guard must hold");
      assert_eq!(issue.culprit, "****");
      db.cleanup().await;
  }
  ```
  If `NewIssue`'s field list differs from the above, read it with `grep -n 'pub struct NewIssue' -A 14 backend/crates/sauron-db/src/models.rs` and match it exactly; and if there is no `repo::get_issue_row`, replace that call with a direct `issues::table.find(issue_id).select(Issue::as_select()).first(&mut conn)`.

- [ ] **Step 2: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_mask`. Expected: `error[E0599]: no function or associated item named 'insert_mask_action'`.

- [ ] **Step 3: Implement the action lifecycle.** Append to `repo.rs`:
  ```rust
  // ===========================================================================
  // PII inspector: mask actions (audit + queue + cursor + progress meter)
  // ===========================================================================

  pub async fn insert_mask_action(
      conn: &mut AsyncPgConnection,
      new: NewInspectorMaskAction<'_>,
  ) -> QueryResult<InspectorMaskAction> {
      diesel::insert_into(inspector_mask_actions::table)
          .values(&new)
          .returning(InspectorMaskAction::as_returning())
          .get_result(conn)
          .await
  }

  pub async fn get_mask_action(
      conn: &mut AsyncPgConnection,
      id: Uuid,
  ) -> QueryResult<Option<InspectorMaskAction>> {
      inspector_mask_actions::table
          .find(id)
          .select(InspectorMaskAction::as_select())
          .first(conn)
          .await
          .optional()
  }

  pub async fn list_mask_actions_for_app(
      conn: &mut AsyncPgConnection,
      app_id: Uuid,
      limit: i64,
  ) -> QueryResult<Vec<InspectorMaskAction>> {
      inspector_mask_actions::table
          .filter(inspector_mask_actions::app_id.eq(app_id))
          .select(InspectorMaskAction::as_select())
          .order(inspector_mask_actions::requested_at.desc())
          .limit(limit.clamp(1, 1_000))
          .load(conn)
          .await
  }

  pub async fn list_mask_actions_for_org(
      conn: &mut AsyncPgConnection,
      org_id: Uuid,
      limit: i64,
  ) -> QueryResult<Vec<InspectorMaskAction>> {
      inspector_mask_actions::table
          .filter(inspector_mask_actions::org_id.eq(org_id))
          .select(InspectorMaskAction::as_select())
          .order(inspector_mask_actions::requested_at.desc())
          .limit(limit.clamp(1, 100_000))
          .load(conn)
          .await
  }

  /// Claim one action for the given slot.
  ///
  /// `kind` selects the SLOT, never the phase: previews and masks are two
  /// independent claim slots, because a single FIFO lets a multi-hour mask
  /// starve every preview past its 15-minute TTL and confirm — which requires
  /// `previewed` — becomes permanently impossible on a busy app.
  ///
  /// `LIMIT 1` is deliberate for masks: masking is heavy write and one action at
  /// a time per worker IS the throttle; N workers take N different actions.
  /// Re-claiming a stale row is the crash-resume mechanism.
  pub async fn claim_mask_action(
      conn: &mut AsyncPgConnection,
      kind: &str,
      worker_id: &str,
      stale_secs: i64,
  ) -> QueryResult<Option<InspectorMaskAction>> {
      let sql = if kind == "preview" {
          "UPDATE inspector_mask_actions SET phase='counting', claimed_at=now(), worker_id=$1, \
                  started_at=COALESCE(started_at, now()) \
           WHERE id IN (SELECT id FROM inspector_mask_actions \
                        WHERE kind='preview' AND status='preview' \
                          AND (claimed_at IS NULL OR claimed_at < now() - make_interval(secs => $2)) \
                        ORDER BY requested_at FOR UPDATE SKIP LOCKED LIMIT 1) \
           RETURNING *"
      } else {
          "UPDATE inspector_mask_actions SET status='running', claimed_at=now(), worker_id=$1, \
                  started_at=COALESCE(started_at, now()) \
           WHERE id IN (SELECT id FROM inspector_mask_actions \
                        WHERE kind='mask' \
                          AND (status='pending' \
                               OR (status IN ('running','cancelling') \
                                   AND claimed_at < now() - make_interval(secs => $2))) \
                        ORDER BY requested_at FOR UPDATE SKIP LOCKED LIMIT 1) \
           RETURNING *"
      };
      diesel::sql_query(sql)
          .bind::<Text, _>(worker_id)
          .bind::<BigInt, _>(stale_secs)
          .get_result(conn)
          .await
          .optional()
  }

  /// A preview finished counting. `previewed_at` is stamped HERE, not at
  /// request time, because the TTL must run from the moment the numbers became
  /// readable.
  pub async fn finish_preview(
      conn: &mut AsyncPgConnection,
      id: Uuid,
      worker_id: &str,
      estimated_rows: i64,
      cold_rows_skipped: i64,
      cold_boundary_at: Option<DateTime<Utc>>,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_mask_actions \
           SET status='previewed', phase='finished', previewed_at=now(), finished_at=now(), \
               estimated_rows=$3, cold_rows_skipped=$4, cold_boundary_at=$5 \
           WHERE id=$1 AND worker_id=$2 AND kind='preview' AND status='preview'",
      )
      .bind::<SqlUuid, _>(id)
      .bind::<Text, _>(worker_id)
      .bind::<BigInt, _>(estimated_rows)
      .bind::<BigInt, _>(cold_rows_skipped)
      .bind::<Nullable<Timestamptz>, _>(cold_boundary_at)
      .execute(conn)
      .await
  }

  /// Promote `previewed` -> `pending` and hand the row to the mask slot.
  ///
  /// Every gate is IN THE STATEMENT rather than in the handler, so a
  /// double-clicked confirm, a concurrent second confirm and a stale preview all
  /// resolve to "0 rows updated" instead of racing. `targets` is deliberately
  /// not a parameter: it was frozen at preview, so a confirm can never widen
  /// what was counted and shown.
  pub async fn confirm_mask_action(
      conn: &mut AsyncPgConnection,
      id: Uuid,
      confirm_source: &str,
      preview_ttl_secs: i64,
      max_rows: i64,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_mask_actions \
           SET kind='mask', status='pending', phase='idle', confirmed_at=now(), \
               confirm_source=$2, finished_at=NULL, claimed_at=NULL, worker_id=NULL \
           WHERE id=$1 AND kind='preview' AND status='previewed' \
             AND previewed_at IS NOT NULL \
             AND previewed_at > now() - make_interval(secs => $3) \
             AND estimated_rows <= $4",
      )
      .bind::<SqlUuid, _>(id)
      .bind::<Text, _>(confirm_source)
      .bind::<BigInt, _>(preview_ttl_secs)
      .bind::<BigInt, _>(max_rows)
      .execute(conn)
      .await
  }

  /// Ask a queued or running mask to stop.
  ///
  /// `running -> cancelling` is allowed; the batch loop observes it on the
  /// `RETURNING status` of a write it was making anyway and lands in terminal
  /// `cancelled` with the cursor and counters already durable. A terminal action
  /// updates zero rows and the handler answers 409.
  pub async fn cancel_mask_action(
      conn: &mut AsyncPgConnection,
      id: Uuid,
      cancelled_by: Option<Uuid>,
      cancelled_by_email: &str,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_mask_actions \
           SET status = CASE WHEN status = 'running' THEN 'cancelling' ELSE 'cancelled' END, \
               cancelled_by=$2, cancelled_by_email=$3, cancelled_at=now(), \
               finished_at = CASE WHEN status = 'running' THEN finished_at ELSE now() END \
           WHERE id=$1 AND status IN ('preview','previewed','pending','running')",
      )
      .bind::<SqlUuid, _>(id)
      .bind::<Nullable<SqlUuid>, _>(cancelled_by)
      .bind::<Text, _>(cancelled_by_email)
      .execute(conn)
      .await
  }

  /// Deactivating a member must also stop the destruction they queued.
  ///
  /// Confirm re-authorizes, but the action then sits in `pending` — with one
  /// slot per worker and a 200 ms inter-batch pause, a backlog can be hours
  /// deep. A member whose account was deactivated (which revokes refresh tokens
  /// and touches nothing queued) must not have their queued destruction execute.
  pub async fn cancel_pending_mask_actions_for_user(
      conn: &mut AsyncPgConnection,
      user_id: Uuid,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_mask_actions \
           SET status='cancelled', cancelled_at=now(), finished_at=now(), \
               error='requester was deactivated before the action ran' \
           WHERE requested_by=$1 AND status IN ('preview','previewed','pending')",
      )
      .bind::<SqlUuid, _>(user_id)
      .execute(conn)
      .await
  }

  pub async fn set_mask_phase(
      conn: &mut AsyncPgConnection,
      id: Uuid,
      worker_id: &str,
      phase: &str,
      day_cursor: Option<chrono::NaiveDate>,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_mask_actions \
           SET phase=$3, day_cursor=$4, cursor_occurred_at=NULL, cursor_id=NULL \
           WHERE id=$1 AND worker_id=$2",
      )
      .bind::<SqlUuid, _>(id)
      .bind::<Text, _>(worker_id)
      .bind::<Text, _>(phase)
      .bind::<Nullable<Date>, _>(day_cursor)
      .execute(conn)
      .await
  }

  pub async fn fail_mask_action(
      conn: &mut AsyncPgConnection,
      id: Uuid,
      reason: &str,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_mask_actions SET status='failed', phase='finished', \
                  finished_at=now(), error=$2 \
           WHERE id=$1 AND status NOT IN ('done','failed','cancelled')",
      )
      .bind::<SqlUuid, _>(id)
      .bind::<Text, _>(reason)
      .execute(conn)
      .await
  }

  /// `cold_boundary_at` is re-recorded HERE, not only at preview, so the audit
  /// shows what execution actually skipped rather than what the preview
  /// predicted — `sauron-tier` defers the drop to a later cycle than the export,
  /// so the boundary genuinely moves during a multi-hour pass.
  pub async fn finish_mask_action(
      conn: &mut AsyncPgConnection,
      id: Uuid,
      worker_id: &str,
      status: &str,
      vacuum_advised: bool,
      cold_boundary_at: Option<DateTime<Utc>>,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_mask_actions SET status=$3, phase='finished', finished_at=now(), \
                  vacuum_advised=$4, cold_boundary_at=COALESCE($5, cold_boundary_at) \
           WHERE id=$1 AND (worker_id=$2 OR worker_id IS NULL)",
      )
      .bind::<SqlUuid, _>(id)
      .bind::<Text, _>(worker_id)
      .bind::<Text, _>(status)
      .bind::<Bool, _>(vacuum_advised)
      .bind::<Nullable<Timestamptz>, _>(cold_boundary_at)
      .execute(conn)
      .await
  }
  ```
  Add `Date` to the `use diesel::sql_types::{...}` list if it is not already imported.

- [ ] **Step 4: Implement the masked-key list.** Append to `repo.rs`:
  ```rust
  /// Register mask targets for forward enforcement.
  ///
  /// `ON CONFLICT DO NOTHING` against `inspector_masked_keys_key` is what makes
  /// re-masking the same finding idempotent — an operator who runs the same mask
  /// twice must not end up with two rows the enforcer walks twice per event.
  pub async fn insert_masked_keys(
      conn: &mut AsyncPgConnection,
      rows: &[NewInspectorMaskedKey<'_>],
  ) -> QueryResult<usize> {
      if rows.is_empty() {
          return Ok(0);
      }
      diesel::insert_into(inspector_masked_keys::table)
          .values(rows)
          .on_conflict((
              inspector_masked_keys::app_id,
              inspector_masked_keys::target_table,
              inspector_masked_keys::target_column,
              inspector_masked_keys::json_path,
          ))
          .do_nothing()
          .execute(conn)
          .await
  }

  /// The enforcer's cache-miss load. One indexed read per app per 30 seconds.
  pub async fn masked_keys_for_app(
      conn: &mut AsyncPgConnection,
      app_id: Uuid,
  ) -> QueryResult<Vec<InspectorMaskedKey>> {
      inspector_masked_keys::table
          .filter(inspector_masked_keys::app_id.eq(app_id))
          .select(InspectorMaskedKey::as_select())
          .order(inspector_masked_keys::created_at.asc())
          .load(conn)
          .await
  }

  /// Same rows, for the read-only "Forward enforcement" card on the Policy tab.
  pub async fn list_masked_keys(
      conn: &mut AsyncPgConnection,
      app_id: Uuid,
  ) -> QueryResult<Vec<InspectorMaskedKey>> {
      masked_keys_for_app(conn, app_id).await
  }
  ```

- [ ] **Step 5: Add the `upsert_issue` sticky guard.** In `repo.rs`'s `upsert_issue` (~line 1410), replace the two lines
  ```rust
  issues::title.eq(excluded(issues::title)),
  issues::culprit.eq(excluded(issues::culprit)),
  ```
  with:
  ```rust
  // Sticky mask guard. `error_events.title` is derived server-side by
  // `build_title(exc, message)` and has no wire field, so forward
  // enforcement alone leaves two gaps on the Issues page: PII inside
  // `exception_type`, which `build_title` also concatenates, and the 30s
  // policy-cache window. Both restore the raw string on the very next
  // occurrence. One string compare on a write bounded by DISTINCT
  // FINGERPRINTS, not by event volume.
  //
  // This is permanent: once a fingerprint's title is '****' it stays
  // '****' forever, even if every subsequent occurrence is benign. That is
  // the correct trade — a fingerprint is a stable error identity — but it
  // is a visible regression on the most-looked-at page in the product, and
  // support will be asked about it. It is in the wiki.
  issues::title.eq(diesel::dsl::sql::<diesel::sql_types::Text>(
      "CASE WHEN issues.title = '****' THEN issues.title ELSE excluded.title END",
  )),
  issues::culprit.eq(diesel::dsl::sql::<diesel::sql_types::Text>(
      "CASE WHEN issues.culprit = '****' THEN issues.culprit ELSE excluded.culprit END",
  )),
  ```

- [ ] **Step 6: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_mask`. All six tests green.

- [ ] **Step 7: Confirm no existing issue test regressed.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db`. The `env_scoping.rs` suite asserts specific `issues.title` strings and must still pass — the guard only fires on the literal `'****'`.

- [ ] **Step 8: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 19: `repo.rs` — the mask and count batch statements

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs`
- Modify `backend/crates/sauron-db/tests/inspector_mask.rs` (append four tests)

**Interfaces:**
- Consumes: `sauron_inspector::targets::{TargetTable, TargetColumn}` (Task 12) — `sauron-db` gains `sauron-inspector` as a dependency so the batch signatures take the enums, never `String`.
- Produces: `repo::{BatchCursor, BatchOutcome, mask_batch_jsonb, explain_mask_batch_jsonb, mask_batch_jsonb_wildcard, mask_batch_text, count_batch_jsonb, count_batch_text, count_null_column, mask_default_partition_batch, mask_rollup_batch, mask_tail_sweep_batch}`.

> The day window appears **twice on purpose** in every partitioned statement. Joining `sel` on `(id, occurred_at)` does **not** reproduce `update_event_symbolication`'s pruning: that function compares `occurred_at` to a **bound scalar parameter**, which is eligible for runtime pruning; comparing it to a CTE column gives the planner no pruning key and it plans one `Update` node per child. The `EXPLAIN` test below is the regression that silently destroys the 2000-row/200 ms cost model.

- [ ] **Step 1: Add the crate dependency.** In `backend/crates/sauron-db/Cargo.toml`, add `sauron-inspector = { workspace = true }` under `[dependencies]`. Confirm with `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check -p sauron-db` that there is no dependency cycle — `sauron-inspector` depends on nothing internal.

- [ ] **Step 2: Write the failing tests.** Append to `backend/crates/sauron-db/tests/inspector_mask.rs`:
  ```rust
  use sauron_db::repo::BatchCursor;
  use sauron_inspector::targets::{TargetColumn, TargetTable};

  /// THE regression test. `EXPLAIN` the batch UPDATE and assert exactly one
  /// `Update on error_events_<child>` node, not one per partition. Comparing
  /// occurred_at to a CTE column instead of a bound scalar gives the planner no
  /// pruning key and the whole cost model behind the throttle evaporates.
  #[tokio::test]
  async fn the_batch_update_prunes_to_one_child() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let ids = db.seed_two_envs().await;
      let mut conn = db.conn().await;
      let day = Utc::now().date_naive();
      let plan = repo::explain_mask_batch_jsonb(
          &mut conn,
          TargetTable::ErrorEvents,
          TargetColumn::Extra,
          ids.app_id,
          day,
          &["customer".to_string(), "email".to_string()],
          BatchCursor::default(),
          10,
      )
      .await
      .unwrap();
      let update_nodes = plan.matches("Update on error_events").count();
      assert!(
          update_nodes <= 2,
          "expected pruning to one child, got {update_nodes} Update nodes:\n{plan}"
      );
      db.cleanup().await;
  }

  #[tokio::test]
  async fn a_jsonb_batch_masks_only_matching_rows_and_advances_the_cursor() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (action_id, app_id, _org) = seed_action(&db, "mask").await;
      let mut conn = db.conn().await;
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query("UPDATE inspector_mask_actions SET status='running', worker_id='w1' WHERE id=$1")
              .bind::<diesel::sql_types::Uuid, _>(action_id),
          &mut conn,
      )
      .await
      .unwrap();
      // Seed two rows in today's partition: one carrying the path, one not.
      let now = Utc::now();
      for (i, extra) in [json!({"customer": {"email": "jane@acme.com"}}), json!({"other": 1})]
          .into_iter()
          .enumerate()
      {
          common::seed_error_event_with_extra(&mut conn, app_id, now - chrono::Duration::seconds(i as i64), &extra).await;
      }
      let out = repo::mask_batch_jsonb(
          &mut conn,
          TargetTable::ErrorEvents,
          TargetColumn::Extra,
          app_id,
          now.date_naive(),
          &["customer".to_string(), "email".to_string()],
          BatchCursor::default(),
          100,
          action_id,
          "w1",
      )
      .await
      .unwrap()
      .expect("the fence must hold");
      assert_eq!(out.rows_masked, 1);
      assert!(out.rows_scanned >= 1);
      assert!(out.next_cursor.is_some(), "a full-ish batch must leave a resumable cursor");
      assert_eq!(out.status, "running");
      db.cleanup().await;
  }

  /// `jsonb_set` returns NULL if ANY argument is NULL, and a NULL written into a
  /// NOT NULL DEFAULT '{}' column is the single most likely implementation bug
  /// in this slice.
  #[tokio::test]
  async fn a_null_jsonb_column_is_never_written_as_sql_null() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (action_id, app_id, _org) = seed_action(&db, "mask").await;
      let mut conn = db.conn().await;
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query("UPDATE inspector_mask_actions SET status='running', worker_id='w1' WHERE id=$1")
              .bind::<diesel::sql_types::Uuid, _>(action_id),
          &mut conn,
      )
      .await
      .unwrap();
      let now = Utc::now();
      common::seed_error_event_with_extra(&mut conn, app_id, now, &json!({"customer": {"email": "a@b.c"}})).await;
      repo::mask_batch_jsonb(
          &mut conn,
          TargetTable::ErrorEvents,
          TargetColumn::Extra,
          app_id,
          now.date_naive(),
          &["customer".to_string(), "email".to_string()],
          BatchCursor::default(),
          100,
          action_id,
          "w1",
      )
      .await
      .unwrap();
      let nulls = repo::count_null_column(&mut conn, "error_events", "extra", app_id).await.unwrap();
      assert_eq!(nulls, 0, "no row may have been written to SQL NULL");
      db.cleanup().await;
  }

  /// The tail sweep is keyed on `received_at`, not `occurred_at`, while KEEPING
  /// an occurred_at range for pruning. `occurred_at` is the CLIENT's timestamp;
  /// a mobile offline queue flushes events whose occurred_at is days old, and
  /// those rows land in a partition the day loop already swept.
  #[tokio::test]
  async fn the_tail_sweep_filters_on_received_at() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (action_id, app_id, _org) = seed_action(&db, "mask").await;
      let mut conn = db.conn().await;
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query("UPDATE inspector_mask_actions SET status='running', worker_id='w1' WHERE id=$1")
              .bind::<diesel::sql_types::Uuid, _>(action_id),
          &mut conn,
      )
      .await
      .unwrap();
      let now = Utc::now();
      common::seed_error_event_with_extra(&mut conn, app_id, now, &json!({"customer": {"email": "a@b.c"}})).await;
      let out = repo::mask_tail_sweep_batch(
          &mut conn,
          TargetTable::ErrorEvents,
          TargetColumn::Extra,
          app_id,
          now - chrono::Duration::days(1),
          now + chrono::Duration::days(1),
          now + chrono::Duration::hours(1), // received_at floor in the future
          &["customer".to_string(), "email".to_string()],
          BatchCursor::default(),
          100,
          action_id,
          "w1",
      )
      .await
      .unwrap()
      .unwrap();
      assert_eq!(out.rows_masked, 0, "a received_at floor in the future must match nothing");
      db.cleanup().await;
  }
  ```
  Add the helper the tests call to `backend/crates/sauron-db/tests/common/mod.rs`:
  ```rust
  /// Insert one minimal `error_events` row carrying a chosen `extra` document.
  pub async fn seed_error_event_with_extra(
      conn: &mut AsyncPgConnection,
      app_id: Uuid,
      occurred_at: DateTime<Utc>,
      extra: &serde_json::Value,
  ) {
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query(
              "INSERT INTO error_events (id, app_id, issue_id, level, occurred_at, received_at, extra) \
               VALUES (gen_random_uuid(), $1, NULL, 'error', $2, $2, $3)",
          )
          .bind::<SqlUuid, _>(app_id)
          .bind::<diesel::sql_types::Timestamptz, _>(occurred_at)
          .bind::<diesel::sql_types::Jsonb, _>(extra),
          conn,
      )
      .await
      .expect("seed error event");
  }
  ```
  If `error_events` has NOT NULL columns beyond those, read `\d error_events` and add them with literal defaults; the row's other values are irrelevant to these assertions.

- [ ] **Step 3: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_mask`. Expected: `error[E0432]: unresolved import 'sauron_db::repo::BatchCursor'`.

- [ ] **Step 4: Implement the shared types and the jsonb batch.** Append to `repo.rs`:
  ```rust
  // ===========================================================================
  // PII inspector: mask + count batches
  // ===========================================================================

  use sauron_inspector::targets::{TargetColumn, TargetTable};

  /// Keyset position within one day's partition. The zero value starts a day.
  #[derive(Debug, Clone, Copy, Default)]
  pub struct BatchCursor {
      pub occurred_at: Option<DateTime<Utc>>,
      pub id: Option<Uuid>,
  }

  #[derive(Debug, Clone)]
  pub struct BatchOutcome {
      pub rows_scanned: i64,
      pub rows_masked: i64,
      /// `None` when the batch came back short — the day is finished.
      pub next_cursor: Option<BatchCursor>,
      /// Observed on a write the worker was making anyway. `cancelling` is how
      /// an operator stops a multi-hour grind at 3am without hand-written SQL.
      pub status: String,
  }

  #[derive(QueryableByName)]
  struct BatchRow {
      #[diesel(sql_type = BigInt)]
      scanned: i64,
      #[diesel(sql_type = BigInt)]
      masked: i64,
      #[diesel(sql_type = Nullable<Timestamptz>)]
      cur_occurred_at: Option<DateTime<Utc>>,
      #[diesel(sql_type = Nullable<SqlUuid>)]
      cur_id: Option<Uuid>,
      #[diesel(sql_type = Text)]
      status: String,
  }

  fn to_outcome(r: BatchRow, limit: i64) -> BatchOutcome {
      BatchOutcome {
          rows_scanned: r.scanned,
          rows_masked: r.masked,
          next_cursor: if r.scanned >= limit {
              Some(BatchCursor { occurred_at: r.cur_occurred_at, id: r.cur_id })
          } else {
              None
          },
          status: r.status,
      }
  }

  /// One day-bounded, keyset-paginated mask batch over a jsonb path.
  ///
  /// The day window appears TWICE on purpose. Joining `sel` on `(id,
  /// occurred_at)` does NOT reproduce `update_event_symbolication`'s pruning:
  /// that function compares `occurred_at` to a BOUND SCALAR PARAMETER, which is
  /// eligible for runtime pruning; comparing it to a CTE column gives the
  /// planner no pruning key and it plans one `Update` node per child.
  ///
  /// `coalesce(col, '{}'::jsonb)` is required because `jsonb_set` returns NULL
  /// if any argument is NULL, and a NULL written into a `NOT NULL DEFAULT '{}'`
  /// column is the single most likely implementation bug in this slice.
  /// `create_missing => false` leaves a row lacking the path untouched.
  ///
  /// The cursor and both counters advance in the same commit as the data change,
  /// so a SIGKILL loses at most one batch and can never double-count.
  #[allow(clippy::too_many_arguments)]
  pub async fn mask_batch_jsonb(
      conn: &mut AsyncPgConnection,
      table: TargetTable,
      column: TargetColumn,
      app_id: Uuid,
      day: chrono::NaiveDate,
      path: &[String],
      cursor: BatchCursor,
      limit: i64,
      action_id: Uuid,
      worker_id: &str,
  ) -> QueryResult<Option<BatchOutcome>> {
      // Identifiers are &'static str from the TargetTable/TargetColumn enums,
      // never caller bytes: SQL identifiers cannot be bound, and the worker
      // reads `targets` back out of Postgres in a different process from the one
      // that validated it.
      let (t, c) = (table.as_sql(), column.as_sql());
      let sql = format!(
          "WITH sel AS ( \
             SELECT id, occurred_at FROM {t} \
             WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
               AND ($4::timestamptz IS NULL OR (occurred_at, id) > ($4, $5)) \
               AND {c} #> $6 IS NOT NULL \
             ORDER BY occurred_at, id LIMIT $7), \
           upd AS ( \
             UPDATE {t} e \
             SET {c} = jsonb_set(coalesce(e.{c}, '{{}}'::jsonb), $6, '\"****\"'::jsonb, false) \
             FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
               AND e.occurred_at >= $2 AND e.occurred_at < $3 \
             RETURNING 1 AS one) \
           UPDATE inspector_mask_actions SET \
             cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
             cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
             rows_masked = rows_masked + (SELECT count(*) FROM upd), \
             rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
             claimed_at = now() \
           WHERE id = $8 AND worker_id = $9 \
           RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                     (SELECT count(*) FROM upd)::bigint AS masked, \
                     cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
      );
      let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
      let hi = lo + chrono::Duration::days(1);
      let row: Option<BatchRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(app_id)
          .bind::<Timestamptz, _>(lo)
          .bind::<Timestamptz, _>(hi)
          .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
          .bind::<Nullable<SqlUuid>, _>(cursor.id)
          .bind::<Array<Text>, _>(path.to_vec())
          .bind::<BigInt, _>(limit)
          .bind::<SqlUuid, _>(action_id)
          .bind::<Text, _>(worker_id)
          .get_result(conn)
          .await
          .optional()?;
      Ok(row.map(|r| to_outcome(r, limit)))
  }

  /// `EXPLAIN` for the same statement, so the pruning regression is a test and
  /// not a code review.
  #[allow(clippy::too_many_arguments)]
  pub async fn explain_mask_batch_jsonb(
      conn: &mut AsyncPgConnection,
      table: TargetTable,
      column: TargetColumn,
      app_id: Uuid,
      day: chrono::NaiveDate,
      path: &[String],
      cursor: BatchCursor,
      limit: i64,
  ) -> QueryResult<String> {
      let (t, c) = (table.as_sql(), column.as_sql());
      let sql = format!(
          "EXPLAIN WITH sel AS ( \
             SELECT id, occurred_at FROM {t} \
             WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
               AND ($4::timestamptz IS NULL OR (occurred_at, id) > ($4, $5)) \
               AND {c} #> $6 IS NOT NULL \
             ORDER BY occurred_at, id LIMIT $7) \
           UPDATE {t} e \
           SET {c} = jsonb_set(coalesce(e.{c}, '{{}}'::jsonb), $6, '\"****\"'::jsonb, false) \
           FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
             AND e.occurred_at >= $2 AND e.occurred_at < $3"
      );
      #[derive(QueryableByName)]
      struct PlanRow {
          #[diesel(sql_type = Text, column_name = "QUERY PLAN")]
          plan: String,
      }
      let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
      let hi = lo + chrono::Duration::days(1);
      let rows: Vec<PlanRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(app_id)
          .bind::<Timestamptz, _>(lo)
          .bind::<Timestamptz, _>(hi)
          .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
          .bind::<Nullable<SqlUuid>, _>(cursor.id)
          .bind::<Array<Text>, _>(path.to_vec())
          .bind::<BigInt, _>(limit)
          .load(conn)
          .await?;
      Ok(rows.into_iter().map(|r| r.plan).collect::<Vec<_>>().join("\n"))
  }
  ```

- [ ] **Step 5: Implement the wildcard, TEXT, default-partition, rollup and tail-sweep batches.** Append to `repo.rs`:
  ```rust
  /// The wildcard lowering: rebuild the array, per element.
  ///
  /// `WITH ORDINALITY` + `ORDER BY ord` is required because `jsonb_agg` order is
  /// NOT guaranteed, and `coalesce(..., '[]')` is required because `jsonb_agg`
  /// over an empty array returns NULL. The rebuild re-serializes the whole array
  /// per row, so it is measurably more expensive than the `jsonb_set` case —
  /// the caller halves the batch size when any target carries a wildcard.
  #[allow(clippy::too_many_arguments)]
  pub async fn mask_batch_jsonb_wildcard(
      conn: &mut AsyncPgConnection,
      table: TargetTable,
      column: TargetColumn,
      app_id: Uuid,
      day: chrono::NaiveDate,
      sub_path: &[String],
      cursor: BatchCursor,
      limit: i64,
      action_id: Uuid,
      worker_id: &str,
  ) -> QueryResult<Option<BatchOutcome>> {
      let (t, c) = (table.as_sql(), column.as_sql());
      let sql = format!(
          "WITH sel AS ( \
             SELECT id, occurred_at FROM {t} \
             WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
               AND ($4::timestamptz IS NULL OR (occurred_at, id) > ($4, $5)) \
               AND jsonb_typeof({c}) = 'array' \
               AND EXISTS (SELECT 1 FROM jsonb_array_elements({c}) el WHERE el #> $6 IS NOT NULL) \
             ORDER BY occurred_at, id LIMIT $7), \
           upd AS ( \
             UPDATE {t} e \
             SET {c} = coalesce(( \
                 SELECT jsonb_agg( \
                          CASE WHEN el #> $6 IS NOT NULL \
                               THEN jsonb_set(el, $6, '\"****\"'::jsonb, false) ELSE el END \
                          ORDER BY ord) \
                 FROM jsonb_array_elements(e.{c}) WITH ORDINALITY AS t(el, ord)), '[]'::jsonb) \
             FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
               AND e.occurred_at >= $2 AND e.occurred_at < $3 \
             RETURNING 1 AS one) \
           UPDATE inspector_mask_actions SET \
             cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
             cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
             rows_masked = rows_masked + (SELECT count(*) FROM upd), \
             rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
             claimed_at = now() \
           WHERE id = $8 AND worker_id = $9 \
           RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                     (SELECT count(*) FROM upd)::bigint AS masked, \
                     cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
      );
      let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
      let hi = lo + chrono::Duration::days(1);
      let row: Option<BatchRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(app_id)
          .bind::<Timestamptz, _>(lo)
          .bind::<Timestamptz, _>(hi)
          .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
          .bind::<Nullable<SqlUuid>, _>(cursor.id)
          .bind::<Array<Text>, _>(sub_path.to_vec())
          .bind::<BigInt, _>(limit)
          .bind::<SqlUuid, _>(action_id)
          .bind::<Text, _>(worker_id)
          .get_result(conn)
          .await
          .optional()?;
      Ok(row.map(|r| to_outcome(r, limit)))
  }

  /// TEXT columns take the WHOLE value. No partial redaction: the workspace has
  /// no direct regex dependency and partial masking leaves recoverable residue.
  #[allow(clippy::too_many_arguments)]
  pub async fn mask_batch_text(
      conn: &mut AsyncPgConnection,
      table: TargetTable,
      column: TargetColumn,
      app_id: Uuid,
      day: chrono::NaiveDate,
      cursor: BatchCursor,
      limit: i64,
      action_id: Uuid,
      worker_id: &str,
  ) -> QueryResult<Option<BatchOutcome>> {
      let (t, c) = (table.as_sql(), column.as_sql());
      let sql = format!(
          "WITH sel AS ( \
             SELECT id, occurred_at FROM {t} \
             WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
               AND ($4::timestamptz IS NULL OR (occurred_at, id) > ($4, $5)) \
               AND {c} IS NOT NULL AND {c} <> '****' \
             ORDER BY occurred_at, id LIMIT $6), \
           upd AS ( \
             UPDATE {t} e SET {c} = '****' \
             FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
               AND e.occurred_at >= $2 AND e.occurred_at < $3 \
             RETURNING 1 AS one) \
           UPDATE inspector_mask_actions SET \
             cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
             cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
             rows_masked = rows_masked + (SELECT count(*) FROM upd), \
             rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
             claimed_at = now() \
           WHERE id = $7 AND worker_id = $8 \
           RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                     (SELECT count(*) FROM upd)::bigint AS masked, \
                     cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
      );
      let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
      let hi = lo + chrono::Duration::days(1);
      let row: Option<BatchRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(app_id)
          .bind::<Timestamptz, _>(lo)
          .bind::<Timestamptz, _>(hi)
          .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
          .bind::<Nullable<SqlUuid>, _>(cursor.id)
          .bind::<BigInt, _>(limit)
          .bind::<SqlUuid, _>(action_id)
          .bind::<Text, _>(worker_id)
          .get_result(conn)
          .await
          .optional()?;
      Ok(row.map(|r| to_outcome(r, limit)))
  }

  /// The `_default` phase, against the child BY NAME.
  ///
  /// `repo::list_child_partitions` excludes `{table}_default` by design, so
  /// those rows are never tiered and never dropped — they are the longest-lived
  /// PII in the system. Rows CANNOT be in the default partition inside a covered
  /// range (Postgres rejects `CREATE TABLE ... PARTITION OF ...` if the default
  /// holds a conflicting row); they are there because their `occurred_at` is
  /// OUTSIDE every explicit range — clock-skewed clients, offline queues.
  ///
  /// Bounded by the same `>= now() - tier_hot_days` predicate as every other
  /// phase: without it this would happily rewrite rows years older than the hot
  /// window, contradicting the hot/cold rule and the `cold_rows_skipped` number.
  #[allow(clippy::too_many_arguments)]
  pub async fn mask_default_partition_batch(
      conn: &mut AsyncPgConnection,
      table: TargetTable,
      column: TargetColumn,
      app_id: Uuid,
      lo_bound: DateTime<Utc>,
      path: &[String],
      cursor: BatchCursor,
      limit: i64,
      action_id: Uuid,
      worker_id: &str,
  ) -> QueryResult<Option<BatchOutcome>> {
      // The child name is derived internally from our own suffix, never input.
      let child = format!("{}_default", table.as_sql());
      let c = column.as_sql();
      let sql = format!(
          "WITH sel AS ( \
             SELECT id, occurred_at FROM {child} \
             WHERE app_id=$1 AND occurred_at >= $2 \
               AND ($3::timestamptz IS NULL OR (occurred_at, id) > ($3, $4)) \
               AND {c} #> $5 IS NOT NULL \
             ORDER BY occurred_at, id LIMIT $6), \
           upd AS ( \
             UPDATE {child} e \
             SET {c} = jsonb_set(coalesce(e.{c}, '{{}}'::jsonb), $5, '\"****\"'::jsonb, false) \
             FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
             RETURNING 1 AS one) \
           UPDATE inspector_mask_actions SET \
             cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
             cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
             rows_masked = rows_masked + (SELECT count(*) FROM upd), \
             rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
             claimed_at = now() \
           WHERE id = $7 AND worker_id = $8 \
           RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                     (SELECT count(*) FROM upd)::bigint AS masked, \
                     cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
      );
      let row: Option<BatchRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(app_id)
          .bind::<Timestamptz, _>(lo_bound)
          .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
          .bind::<Nullable<SqlUuid>, _>(cursor.id)
          .bind::<Array<Text>, _>(path.to_vec())
          .bind::<BigInt, _>(limit)
          .bind::<SqlUuid, _>(action_id)
          .bind::<Text, _>(worker_id)
          .get_result(conn)
          .await
          .optional()?;
      Ok(row.map(|r| to_outcome(r, limit)))
  }

  /// One keyset pass over a non-partitioned companion table, filtered on
  /// `app_id`. No day loop — these are orders of magnitude smaller than the
  /// event tables. `path` empty means the column is TEXT and takes `'****'`.
  #[allow(clippy::too_many_arguments)]
  pub async fn mask_rollup_batch(
      conn: &mut AsyncPgConnection,
      table: TargetTable,
      column: TargetColumn,
      app_id: Uuid,
      path: &[String],
      after_id: Option<Uuid>,
      limit: i64,
      action_id: Uuid,
      worker_id: &str,
  ) -> QueryResult<Option<BatchOutcome>> {
      let (t, c) = (table.as_sql(), column.as_sql());
      let set_expr = if path.is_empty() {
          format!("{c} = '****'")
      } else {
          format!("{c} = jsonb_set(coalesce(e.{c}, '{{}}'::jsonb), $3, '\"****\"'::jsonb, false)")
      };
      let match_expr = if path.is_empty() {
          format!("{c} IS NOT NULL AND {c} <> '****'")
      } else {
          format!("{c} #> $3 IS NOT NULL")
      };
      let sql = format!(
          "WITH sel AS ( \
             SELECT id FROM {t} \
             WHERE app_id=$1 AND ($2::uuid IS NULL OR id > $2) AND {match_expr} \
             ORDER BY id LIMIT $4), \
           upd AS (UPDATE {t} e SET {set_expr} FROM sel WHERE e.id = sel.id RETURNING 1 AS one) \
           UPDATE inspector_mask_actions SET \
             cursor_id = (SELECT max(id) FROM sel), cursor_occurred_at = NULL, \
             rows_masked = rows_masked + (SELECT count(*) FROM upd), \
             rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
             claimed_at = now() \
           WHERE id = $5 AND worker_id = $6 \
           RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                     (SELECT count(*) FROM upd)::bigint AS masked, \
                     cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
      );
      let row: Option<BatchRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(app_id)
          .bind::<Nullable<SqlUuid>, _>(after_id)
          .bind::<Array<Text>, _>(path.to_vec())
          .bind::<BigInt, _>(limit)
          .bind::<SqlUuid, _>(action_id)
          .bind::<Text, _>(worker_id)
          .get_result(conn)
          .await
          .optional()?;
      Ok(row.map(|r| to_outcome(r, limit)))
  }

  /// The tail sweep closes the enforcement race between "mask applied" and
  /// "every pipeline replica's policy cache refreshed".
  ///
  /// Keyed on `received_at`, NOT `occurred_at`: `occurred_at` is the CLIENT's
  /// timestamp (`process.rs` sets `occurred_at: ev.timestamp`), so a mobile SDK
  /// offline queue or a skewed clock flushes events whose `occurred_at` is days
  /// old — those rows land in a partition the day loop already swept and would
  /// never be revisited. The `occurred_at` range stays for PRUNING only, because
  /// `error_events.received_at` has no index.
  #[allow(clippy::too_many_arguments)]
  pub async fn mask_tail_sweep_batch(
      conn: &mut AsyncPgConnection,
      table: TargetTable,
      column: TargetColumn,
      app_id: Uuid,
      lo: DateTime<Utc>,
      hi: DateTime<Utc>,
      received_since: DateTime<Utc>,
      path: &[String],
      cursor: BatchCursor,
      limit: i64,
      action_id: Uuid,
      worker_id: &str,
  ) -> QueryResult<Option<BatchOutcome>> {
      let (t, c) = (table.as_sql(), column.as_sql());
      let sql = format!(
          "WITH sel AS ( \
             SELECT id, occurred_at FROM {t} \
             WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
               AND received_at >= $4 \
               AND ($5::timestamptz IS NULL OR (occurred_at, id) > ($5, $6)) \
               AND {c} #> $7 IS NOT NULL \
             ORDER BY occurred_at, id LIMIT $8), \
           upd AS ( \
             UPDATE {t} e \
             SET {c} = jsonb_set(coalesce(e.{c}, '{{}}'::jsonb), $7, '\"****\"'::jsonb, false) \
             FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
               AND e.occurred_at >= $2 AND e.occurred_at < $3 \
             RETURNING 1 AS one) \
           UPDATE inspector_mask_actions SET \
             cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
             cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
             rows_masked = rows_masked + (SELECT count(*) FROM upd), \
             rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
             claimed_at = now() \
           WHERE id = $9 AND worker_id = $10 \
           RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                     (SELECT count(*) FROM upd)::bigint AS masked, \
                     cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
      );
      let row: Option<BatchRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(app_id)
          .bind::<Timestamptz, _>(lo)
          .bind::<Timestamptz, _>(hi)
          .bind::<Timestamptz, _>(received_since)
          .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
          .bind::<Nullable<SqlUuid>, _>(cursor.id)
          .bind::<Array<Text>, _>(path.to_vec())
          .bind::<BigInt, _>(limit)
          .bind::<SqlUuid, _>(action_id)
          .bind::<Text, _>(worker_id)
          .get_result(conn)
          .await
          .optional()?;
      Ok(row.map(|r| to_outcome(r, limit)))
  }

  /// Preview counting: the identical day loop with `count(*)` instead of UPDATE.
  ///
  /// Run on the INSPECTOR's pool, never the API's. Counting `col #> path IS NOT
  /// NULL` over an app's hot window is a Parallel Append seq scan — 184 ms per
  /// 210k rows measured — with no index that can serve it, since the tags GIN is
  /// `jsonb_path_ops` and answers `@>` only. On the API's 16-connection pool
  /// that is how the whole dashboard goes down.
  pub async fn count_batch_jsonb(
      conn: &mut AsyncPgConnection,
      table: TargetTable,
      column: TargetColumn,
      app_id: Uuid,
      day: chrono::NaiveDate,
      path: &[String],
  ) -> QueryResult<i64> {
      let (t, c) = (table.as_sql(), column.as_sql());
      let sql = format!(
          "SELECT count(*)::bigint AS n FROM {t} \
           WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 AND {c} #> $4 IS NOT NULL"
      );
      let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
      let hi = lo + chrono::Duration::days(1);
      let row: CountRow = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(app_id)
          .bind::<Timestamptz, _>(lo)
          .bind::<Timestamptz, _>(hi)
          .bind::<Array<Text>, _>(path.to_vec())
          .get_result(conn)
          .await?;
      Ok(row.n)
  }

  pub async fn count_batch_text(
      conn: &mut AsyncPgConnection,
      table: TargetTable,
      column: TargetColumn,
      app_id: Uuid,
      day: chrono::NaiveDate,
  ) -> QueryResult<i64> {
      let (t, c) = (table.as_sql(), column.as_sql());
      let sql = format!(
          "SELECT count(*)::bigint AS n FROM {t} \
           WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
             AND {c} IS NOT NULL AND {c} <> '****'"
      );
      let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
      let hi = lo + chrono::Duration::days(1);
      let row: CountRow = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(app_id)
          .bind::<Timestamptz, _>(lo)
          .bind::<Timestamptz, _>(hi)
          .get_result(conn)
          .await?;
      Ok(row.n)
  }

  /// How many rows have a SQL NULL in a jsonb column. Exists so the "jsonb_set
  /// returns NULL if any argument is NULL" bug is a test rather than a
  /// production incident.
  pub async fn count_null_column(
      conn: &mut AsyncPgConnection,
      table: &'static str,
      column: &'static str,
      app_id: Uuid,
  ) -> QueryResult<i64> {
      let sql = format!("SELECT count(*)::bigint AS n FROM {table} WHERE app_id=$1 AND {column} IS NULL");
      let row: CountRow = diesel::sql_query(sql).bind::<SqlUuid, _>(app_id).get_result(conn).await?;
      Ok(row.n)
  }
  ```

- [ ] **Step 6: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_mask`. All ten tests green. If `the_batch_update_prunes_to_one_child` reports many `Update on error_events_*` nodes, the second `e.occurred_at >= $2 AND e.occurred_at < $3` on the outer UPDATE is missing.

- [ ] **Step 7: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 20: `repo.rs` — the five reapers and the statement-timeout wrapper

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs`
- Modify `backend/crates/sauron-db/tests/inspector_mask.rs` (append two tests)

**Interfaces:**
- Consumes: the tables from Tasks 2 and 3.
- Produces: `repo::{set_statement_timeout, reset_statement_timeout, prune_inspector_scans, prune_inspector_findings, prune_mask_previews, prune_mask_actions, pseudonymize_mask_actions}`.

- [ ] **Step 1: Write the failing tests.** Append to `backend/crates/sauron-db/tests/inspector_mask.rs`:
  ```rust
  /// `prune_mask_actions` defaults to 0 = NEVER prune. This table grows per
  /// HUMAN ACTION, not per rule evaluation, and it is the record a compliance
  /// question is answered from.
  #[tokio::test]
  async fn audit_retention_of_zero_deletes_nothing() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (id, _app, _org) = seed_action(&db, "mask").await;
      let mut conn = db.conn().await;
      repo::finish_mask_action(&mut conn, id, "w1", "done", false, None).await.unwrap();
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query("UPDATE inspector_mask_actions SET requested_at = now() - interval '900 days' WHERE id=$1")
              .bind::<diesel::sql_types::Uuid, _>(id),
          &mut conn,
      )
      .await
      .unwrap();
      assert_eq!(repo::prune_mask_actions(&mut conn, 0, 1_000).await.unwrap(), 0);
      assert!(repo::get_mask_action(&mut conn, id).await.unwrap().is_some());
      db.cleanup().await;
  }

  /// Without pseudonymization the privacy feature is the only UN-ERASABLE store
  /// of staff PII in the schema: everywhere else a user row cascades, so
  /// deleting a user is the product's de-facto erasure mechanism, and
  /// `ON DELETE SET NULL` plus a denormalized email breaks it by design.
  #[tokio::test]
  async fn pseudonymization_keeps_counts_and_drops_identities() {
      let Some(db) = TestDb::setup().await else {
          eprintln!("TEST_DATABASE_URL unset — skipping");
          return;
      };
      let (id, _app, _org) = seed_action(&db, "mask").await;
      let mut conn = db.conn().await;
      repo::finish_mask_action(&mut conn, id, "w1", "done", false, None).await.unwrap();
      diesel_async::RunQueryDsl::execute(
          diesel::sql_query(
              "UPDATE inspector_mask_actions \
               SET requested_at = now() - interval '900 days', rows_masked = 41200, \
                   confirm_source = 'ip=10.0.0.5 (untrusted-peer)' WHERE id=$1",
          )
          .bind::<diesel::sql_types::Uuid, _>(id),
          &mut conn,
      )
      .await
      .unwrap();
      assert_eq!(repo::pseudonymize_mask_actions(&mut conn, 730).await.unwrap(), 1);
      let a = repo::get_mask_action(&mut conn, id).await.unwrap().unwrap();
      assert_eq!(a.requested_by_email, "");
      assert_eq!(a.cancelled_by_email, "");
      assert_eq!(a.confirm_source, "");
      assert_eq!(a.rows_masked, 41_200, "counts and targets survive");
      assert!(!a.targets.as_array().unwrap().is_empty());
      db.cleanup().await;
  }
  ```

- [ ] **Step 2: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_mask prune`. Expected: `error[E0599]: no function or associated item named 'prune_mask_actions'`.

- [ ] **Step 3: Implement the statement-timeout wrapper.** Append to `repo.rs`:
  ```rust
  // ===========================================================================
  // PII inspector: session settings + retention
  // ===========================================================================

  /// Bound every statement this connection runs.
  ///
  /// MUST be paired with [`reset_statement_timeout`] before `drop(conn)`:
  /// deadpool's recycle does NOT reset session state, so a leaked `SET` silently
  /// poisons a later checkout in the same process — an API request that has
  /// nothing to do with the inspector then fails at 30 seconds with a message
  /// nobody can trace. This is the ONLY place the setting is written; never an
  /// ad-hoc `SET` at a call site.
  ///
  /// The value is formatted, not bound, because `SET` does not take parameters.
  /// It is an `i64` from `Config`, never caller input.
  pub async fn set_statement_timeout(conn: &mut AsyncPgConnection, ms: u64) -> QueryResult<()> {
      conn.batch_execute(&format!("SET statement_timeout = {ms}")).await
  }

  pub async fn reset_statement_timeout(conn: &mut AsyncPgConnection) -> QueryResult<()> {
      conn.batch_execute("RESET statement_timeout").await
  }

  /// Keep the newest `keep` scans per policy.
  ///
  /// Findings are deleted in BOUNDED batches before the parent row is dropped.
  /// The house prune idiom has no LIMIT, and an unbounded cascading DELETE of up
  /// to 660k findings is a bloat and lock spike — a nightly scan producing 33k
  /// findings is 12M rows a year, which is the exact failure `alert_events`'
  /// reaper doc comment warns about.
  pub async fn prune_inspector_scans(
      conn: &mut AsyncPgConnection,
      keep: i64,
      batch: i64,
  ) -> QueryResult<usize> {
      // Findings first, in batches, so the cascade never has to.
      loop {
          let n = diesel::sql_query(
              "DELETE FROM inspector_findings WHERE ctid IN ( \
                 SELECT f.ctid FROM inspector_findings f \
                 WHERE f.scan_id IN ( \
                   SELECT id FROM ( \
                     SELECT id, row_number() OVER (PARTITION BY policy_id ORDER BY created_at DESC) rn \
                     FROM inspector_scans) r WHERE r.rn > $1) \
                 LIMIT $2)",
          )
          .bind::<BigInt, _>(keep)
          .bind::<BigInt, _>(batch)
          .execute(conn)
          .await?;
          if n == 0 {
              break;
          }
      }
      diesel::sql_query(
          "DELETE FROM inspector_scans WHERE id IN ( \
             SELECT id FROM ( \
               SELECT id, row_number() OVER (PARTITION BY policy_id ORDER BY created_at DESC) rn \
               FROM inspector_scans) r WHERE r.rn > $1)",
      )
      .bind::<BigInt, _>(keep)
      .execute(conn)
      .await
  }

  /// Age out findings, stamping the owning scan so a scan row's
  /// `findings_count` and its empty finding list never silently disagree.
  pub async fn prune_inspector_findings(
      conn: &mut AsyncPgConnection,
      days: i64,
      batch: i64,
  ) -> QueryResult<usize> {
      let mut total = 0usize;
      loop {
          let n = diesel::sql_query(
              "WITH doomed AS ( \
                 SELECT ctid, scan_id FROM inspector_findings \
                 WHERE created_at < now() - ($1 || ' days')::interval LIMIT $2), \
               stamped AS ( \
                 UPDATE inspector_scans s SET findings_reaped_at = now() \
                 WHERE s.id IN (SELECT scan_id FROM doomed) AND s.findings_reaped_at IS NULL \
                 RETURNING 1) \
               DELETE FROM inspector_findings f \
               WHERE f.ctid IN (SELECT ctid FROM doomed)",
          )
          .bind::<BigInt, _>(days)
          .bind::<BigInt, _>(batch)
          .execute(conn)
          .await?;
          total += n;
          if n == 0 {
              break;
          }
      }
      Ok(total)
  }

  /// Abandoned previews are not audit-relevant, so this ALWAYS runs.
  pub async fn prune_mask_previews(conn: &mut AsyncPgConnection, days: i64) -> QueryResult<usize> {
      diesel::sql_query(
          "DELETE FROM inspector_mask_actions \
           WHERE kind='preview' AND status IN ('preview','previewed','failed','cancelled') \
             AND requested_at < now() - ($1 || ' days')::interval",
      )
      .bind::<BigInt, _>(days)
      .execute(conn)
      .await
  }

  /// Prune terminal MASK actions, and ONLY when explicitly enabled.
  ///
  /// `days = 0` means never. This table grows per human action, not per rule
  /// evaluation, and it is the record a compliance question is answered from.
  pub async fn prune_mask_actions(
      conn: &mut AsyncPgConnection,
      days: i64,
      batch: i64,
  ) -> QueryResult<usize> {
      if days <= 0 {
          return Ok(0);
      }
      diesel::sql_query(
          "DELETE FROM inspector_mask_actions WHERE ctid IN ( \
             SELECT ctid FROM inspector_mask_actions \
             WHERE kind='mask' AND status IN ('done','failed','cancelled') \
               AND requested_at < now() - ($1 || ' days')::interval \
             LIMIT $2)",
      )
      .bind::<BigInt, _>(days)
      .bind::<BigInt, _>(batch)
      .execute(conn)
      .await
  }

  /// Null the staff identities on old audit rows, keeping counts and targets.
  ///
  /// Everywhere else in this schema a user row cascades (`refresh_tokens`,
  /// `role_grants`), so deleting a user IS the product's de-facto erasure
  /// mechanism. `ON DELETE SET NULL` plus a denormalized email breaks that by
  /// design — deliberately, so the trail survives — which makes this the only
  /// un-erasable store of staff PII in the product unless it is aged out.
  pub async fn pseudonymize_mask_actions(
      conn: &mut AsyncPgConnection,
      days: i64,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_mask_actions \
           SET requested_by_email='', cancelled_by_email='', confirm_source='' \
           WHERE requested_at < now() - ($1 || ' days')::interval \
             AND (requested_by_email <> '' OR cancelled_by_email <> '' OR confirm_source <> '')",
      )
      .bind::<BigInt, _>(days)
      .execute(conn)
      .await
  }
  ```

- [ ] **Step 4: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db --test inspector_mask`. All twelve tests green.

- [ ] **Step 5: Confirm the whole DB crate is green with and without a database.** Run both `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db` and the same command without the two `TEST_*` variables. Both green; the second skips.

- [ ] **Step 6: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 21: The `sauron-inspector` binary — package, four loops, one pool

**Files:**
- Create `backend/bins/sauron-inspector/Cargo.toml`
- Create `backend/bins/sauron-inspector/src/main.rs`
- Create `backend/bins/sauron-inspector/src/scan.rs` (stub with the real signature)
- Create `backend/bins/sauron-inspector/src/mask.rs` (stub)
- Create `backend/bins/sauron-inspector/src/preview.rs` (stub)
- Create `backend/bins/sauron-inspector/src/reap.rs` (stub)

**Interfaces:**
- Consumes: `Config` (Task 4), `repo::{claim_due_policies, set_statement_timeout, reset_statement_timeout}` (Tasks 15, 20).
- Produces: `checkout(&PgPool, &Config) -> anyhow::Result<PgConn>`, `release(PgConn)`; the module entry points `scan::tick`, `mask::tick`, `preview::tick`, `reap::tick`, each `async fn tick(pool: &PgPool, cfg: &Config, worker_id: &str) -> anyhow::Result<bool>` returning whether work was done; `repo::{record_policy_skip, record_policy_scan}` (added here, in Step 4).

> Package name is `sauron-inspector-bin` with `[[bin]] name = "sauron-inspector"`, because the library crate owns the plain name — the exact `sauron-alerts-bin` / `sauron-tier-bin` precedent. One binary per `bins/` directory; never add a `[[bin]]` to an existing bin package, because `binaries.txt`, CI and the spec would never see it.

- [ ] **Step 1: Create the manifest.** `backend/bins/sauron-inspector/Cargo.toml`:
  ```toml
  [package]
  name = "sauron-inspector-bin"
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  rust-version.workspace = true

  [[bin]]
  name = "sauron-inspector"
  path = "src/main.rs"

  # No Redis and NO DuckDB, deliberately: this binary must not inherit the
  # unbundled libduckdb constraint across a fourth build path.
  [dependencies]
  sauron-core = { workspace = true }
  sauron-db = { workspace = true }
  sauron-inspector = { workspace = true }
  sauron-tier = { workspace = true }
  sauron-telemetry = { workspace = true }
  tokio = { workspace = true }
  chrono = { workspace = true }
  uuid = { workspace = true }
  serde = { workspace = true }
  serde_json = { workspace = true }
  tracing = { workspace = true }
  anyhow = { workspace = true }
  ```
  `sauron-tier` is a pure planning crate here — only `TIERED_TABLES` is used, and the DuckDB engine behind `duck.rs` is not linked unless a symbol from it is referenced. If `cargo check` pulls `libduckdb` in anyway, drop the dependency and hard-code the three table names in `scan.rs` with a comment naming `sauron_tier::TIERED_TABLES` as the source of truth.

- [ ] **Step 2: Write the four stubs.** Create each of `scan.rs`, `mask.rs`, `preview.rs`, `reap.rs` in `backend/bins/sauron-inspector/src/` with exactly:
  ```rust
  use sauron_core::Config;
  use sauron_db::PgPool;

  /// One iteration. Returns `true` when it did work, so the caller can tighten
  /// its sleep instead of waiting a full interval behind a backlog.
  pub async fn tick(_pool: &PgPool, _cfg: &Config, _worker_id: &str) -> anyhow::Result<bool> {
      Ok(false)
  }
  ```

- [ ] **Step 3: Write `main.rs`.** Create `backend/bins/sauron-inspector/src/main.rs`:
  ```rust
  //! `sauron-inspector` — the PII scanner, retro-masker and audit reaper.
  //!
  //! FOUR independent loops, one 4-connection pool.
  //!
  //! The single-task shape every other worker in this repo uses does not work
  //! here. A project-scoped scan can run for hours, and a scheduler folded into
  //! the same loop would not execute for that whole time — so when the worker
  //! finally returned, everything queued behind it would be more than
  //! `INSPECTOR_CATCHUP_GRACE_HOURS` stale and get SKIPPED. Enabling one large
  //! policy would silently disable scheduling for every other policy, with the
  //! only signal buried in a column. Likewise, routing previews through the mask
  //! FIFO means a preview requested while a multi-hour mask runs expires before
  //! it is ever computed, and confirm becomes permanently impossible.
  //!
  //! ONE pool, not two. Today's peak pooled demand is sauron-api 16 +
  //! sauron-ingest 8 + sauron-alerts 8 + sauron-tier 4 + sauron-monitor (50 + 8)
  //! = 94, against `postgres:16` with no tuning — the default `max_connections`
  //! of 100 with 3 reserved for superusers. A second pool here pushes the
  //! shipped deployment over the edge, and connection exhaustion surfaces as API
  //! 500s and ingest 202-then-drop, not as an inspector error.

  mod mask;
  mod preview;
  mod reap;
  mod scan;

  use std::sync::Arc;
  use std::time::Duration;

  use sauron_core::Config;
  use sauron_db::{PgConn, PgPool};
  use tracing::{info, warn};

  /// Executor cadence. Deliberately much shorter than the scheduler's tick: an
  /// executor does ONE unit or ONE batch per iteration and re-enters, so the
  /// lease heartbeat is frequent and cancellation is observed quickly.
  const EXECUTOR_INTERVAL: Duration = Duration::from_secs(1);
  const REAPER_INTERVAL: Duration = Duration::from_secs(3600);

  /// Check out a connection AND bound every statement it will run.
  ///
  /// Always paired with [`release`]: deadpool's recycle does not reset session
  /// state, so a leaked `SET statement_timeout` silently poisons a later
  /// checkout in the same process.
  pub async fn checkout(pool: &PgPool, cfg: &Config) -> anyhow::Result<PgConn> {
      let mut conn = sauron_db::conn(pool).await?;
      sauron_db::repo::set_statement_timeout(&mut conn, cfg.inspector_statement_timeout_ms).await?;
      Ok(conn)
  }

  /// Reset the session setting, then drop. Never hold a pooled connection across
  /// the inter-batch sleep — the pool is 4 for the whole process.
  pub async fn release(mut conn: PgConn) {
      if let Err(e) = sauron_db::repo::reset_statement_timeout(&mut conn).await {
          // A failed RESET means this connection is poisoned for whoever gets it
          // next, and the failure mode (a 30s timeout on an unrelated query) is
          // untraceable, so say so loudly rather than dropping silently.
          warn!(error = %e, "could not reset statement_timeout; connection returned poisoned");
      }
      drop(conn);
  }

  #[tokio::main]
  async fn main() -> anyhow::Result<()> {
      sauron_telemetry::init("sauron-inspector");
      let cfg = Arc::new(Config::from_env()?);

      if !cfg.inspector_enabled {
          info!("INSPECTOR_ENABLED is false; sauron-inspector is idle");
          // Sleep forever rather than exit: systemd's Restart=on-failure would
          // not restart a clean exit, but an operator flipping the flag expects
          // `systemctl restart` to be the whole procedure, and a unit in
          // `inactive (dead)` looks like a crash in `systemctl status`.
          loop {
              tokio::time::sleep(Duration::from_secs(3600)).await;
          }
      }

      let pool = sauron_db::build_pool(&cfg.database_url, 4)?;
      // Distinct per process AND per restart: the worker-id fence on every flush
      // exists so a worker whose lease expired cannot double-count after coming
      // back, and reusing a stable id across restarts would defeat it.
      let worker_id = format!(
          "inspector-{}-{}",
          std::process::id(),
          sauron_core::ids::random_hex(4)
      );
      info!(
          worker_id,
          tick_secs = cfg.inspector_tick_secs,
          tail_sweep_secs = cfg.inspector_tail_sweep_secs,
          policy_cache_secs = cfg.inspector_policy_cache_secs,
          "sauron-inspector started"
      );

      let scheduler = spawn_loop(
          "scheduler",
          Duration::from_secs(cfg.inspector_tick_secs),
          pool.clone(),
          cfg.clone(),
          worker_id.clone(),
          |p, c, w| Box::pin(async move { schedule_tick(&p, &c, &w).await }),
      );
      let scans = spawn_loop(
          "scan",
          EXECUTOR_INTERVAL,
          pool.clone(),
          cfg.clone(),
          worker_id.clone(),
          |p, c, w| Box::pin(async move { scan::tick(&p, &c, &w).await }),
      );
      let masks = spawn_loop(
          "mask",
          EXECUTOR_INTERVAL,
          pool.clone(),
          cfg.clone(),
          worker_id.clone(),
          |p, c, w| Box::pin(async move { mask::tick(&p, &c, &w).await }),
      );
      let previews = spawn_loop(
          "preview",
          EXECUTOR_INTERVAL,
          pool.clone(),
          cfg.clone(),
          worker_id.clone(),
          |p, c, w| Box::pin(async move { preview::tick(&p, &c, &w).await }),
      );
      let reaper = spawn_loop(
          "reap",
          REAPER_INTERVAL,
          pool.clone(),
          cfg.clone(),
          worker_id.clone(),
          |p, c, w| Box::pin(async move { reap::tick(&p, &c, &w).await }),
      );

      let _ = tokio::join!(scheduler, scans, masks, previews, reaper);
      Ok(())
  }

  type TickFuture = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send>>;

  /// Spawn one supervised loop. Errors are logged and swallowed: a loop that
  /// returns is a loop that silently stops doing its job, and there is no
  /// graceful shutdown anywhere in this product to distinguish that from a
  /// deliberate stop.
  fn spawn_loop<F>(
      name: &'static str,
      interval: Duration,
      pool: PgPool,
      cfg: Arc<Config>,
      worker_id: String,
      f: F,
  ) -> tokio::task::JoinHandle<()>
  where
      F: Fn(PgPool, Arc<Config>, String) -> TickFuture + Send + 'static,
  {
      tokio::spawn(async move {
          loop {
              match f(pool.clone(), cfg.clone(), worker_id.clone()).await {
                  // Work was done, so come straight back: a backlog must drain at
                  // the batch pause, not at the loop interval.
                  Ok(true) => tokio::time::sleep(Duration::from_millis(10)).await,
                  Ok(false) => tokio::time::sleep(interval).await,
                  Err(e) => {
                      warn!(loop_name = name, error = %e, "inspector loop tick failed");
                      tokio::time::sleep(interval).await;
                  }
              }
          }
      })
  }

  /// Claim due policies and enqueue a scan for each. NEVER blocked by execution.
  ///
  /// Catch-up fires ONCE on recovery and never replays missed runs: a scan is a
  /// snapshot over a window, not an event stream, so three replayed runs produce
  /// three near-identical finding sets at 3x the load. And a 03:00 scan firing
  /// at 09:00 on a Monday is precisely the production load spike the schedule
  /// existed to avoid — so a run more than `INSPECTOR_CATCHUP_GRACE_HOURS` stale
  /// is skipped with the reason recorded in `last_skip_reason`.
  async fn schedule_tick(pool: &PgPool, cfg: &Config, worker_id: &str) -> anyhow::Result<bool> {
      let mut conn = checkout(pool, cfg).await?;
      let due = sauron_db::repo::claim_due_policies(&mut conn, 50).await?;
      release(conn).await;
      if due.is_empty() {
          return Ok(false);
      }
      let mut started = 0usize;
      for policy in due {
          let mut conn = checkout(pool, cfg).await?;
          let stale_hours = policy
              .last_run_at
              .map(|t| (chrono::Utc::now() - t).num_hours())
              .unwrap_or(0);
          if stale_hours > cfg.inspector_catchup_grace_hours {
              // Recorded through a dedicated statement so the reason string is
              // not a lifetime puzzle inside `InspectorPolicyPatch`, whose
              // borrowed fields would force this `format!` to outlive the call.
              let _ = sauron_db::repo::record_policy_skip(
                  &mut conn,
                  policy.id,
                  &format!("catch-up skipped: {stale_hours}h stale"),
              )
              .await;
              release(conn).await;
              continue;
          }
          match scan::enqueue_for_policy(&mut conn, cfg, &policy, "scheduled", None).await {
              Ok(true) => started += 1,
              Ok(false) => {}
              Err(e) => warn!(policy_id = %policy.id, error = %e, "could not enqueue scheduled scan"),
          }
          release(conn).await;
      }
      info!(started, "scheduler tick");
      Ok(started > 0)
  }
  ```

- [ ] **Step 4: Add the two repo functions `main.rs` calls.** Append to `backend/crates/sauron-db/src/repo.rs`:
  ```rust
  /// Record why a scheduled run was not started. Kept as its own statement so
  /// the reason is a plain `&str` rather than a lifetime inside the patch
  /// struct, and so the write is one round trip.
  pub async fn record_policy_skip(
      conn: &mut AsyncPgConnection,
      id: Uuid,
      reason: &str,
  ) -> QueryResult<usize> {
      diesel::sql_query("UPDATE inspector_policies SET last_skip_reason = $2 WHERE id = $1")
          .bind::<SqlUuid, _>(id)
          .bind::<Text, _>(reason)
          .execute(conn)
          .await
  }

  /// Point a policy at the scan it most recently started.
  pub async fn record_policy_scan(
      conn: &mut AsyncPgConnection,
      id: Uuid,
      scan_id: Uuid,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_policies SET last_scan_id = $2, last_skip_reason = '' WHERE id = $1",
      )
      .bind::<SqlUuid, _>(id)
      .bind::<SqlUuid, _>(scan_id)
      .execute(conn)
      .await
  }
  ```

- [ ] **Step 5: Add the enqueue stub so `main.rs` compiles.** In `backend/bins/sauron-inspector/src/scan.rs`, add above `tick`:
  ```rust
  use sauron_db::models::InspectorPolicy;
  use sauron_db::AsyncPgConnection;
  use uuid::Uuid;

  /// Freeze a policy into a scan row. Returns `false` when the policy already
  /// has a queued/running scan — the partial unique index is the arbiter, not a
  /// handler check, so two schedulers racing produce one scan.
  pub async fn enqueue_for_policy(
      _conn: &mut AsyncPgConnection,
      _cfg: &Config,
      _policy: &InspectorPolicy,
      _trigger: &str,
      _requested_by: Option<Uuid>,
  ) -> anyhow::Result<bool> {
      Ok(false)
  }
  ```

- [ ] **Step 6: Build and see it compile.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Clean. If `spawn_loop`'s closure bound fails to infer, annotate each call site's closure argument types explicitly: `|p: PgPool, c: Arc<Config>, w: String| -> TickFuture { ... }`.

- [ ] **Step 7: Run it against the live database with the flag off, then on.** `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu timeout 5 cargo run --bin sauron-inspector`. Expected: one line `INSPECTOR_ENABLED is false; sauron-inspector is idle` and no further output. Then re-run with `INSPECTOR_ENABLED=1` prepended; expected: `sauron-inspector started` with the worker id, then `scheduler tick` lines with `started=0`.

- [ ] **Step 8: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 22: `units.rs` and `scan.rs` — unit decomposition, the two phases, and the flush

**Files:**
- Create `backend/crates/sauron-inspector/src/units.rs`
- Modify `backend/crates/sauron-inspector/src/lib.rs`
- Modify `backend/crates/sauron-db/src/repo.rs`
- Modify `backend/bins/sauron-inspector/src/scan.rs`

**Interfaces:**
- Consumes: `sauron_inspector::{columns, walk, matching, detect, redact, prefilter, targets, units}` (Tasks 5–13), `repo::{claim_one_scan, flush_scan_unit, finish_scan, scan_pairs_for_node, list_inspector_policies_under, validate_scope_in_org, insert_inspector_scan}` (Tasks 15–16), `repo::record_policy_scan` (Task 21), `checkout`/`release` (Task 21).
- Produces: `sauron_inspector::units::{Unit, units_for, tables_for}`; `repo::{EnqueueOutcome, enqueue_scan_for_policy, note_scan_coverage, ScanShape, ScanCursor, ScanRow, scan_window_rows}` (added here); `scan::tick`, `scan::enqueue_for_policy` (the worker-side wrapper Task 21 stubbed).

> **The unit model lives in `sauron-inspector`, not in the worker binary, and the enqueue lives in `sauron-db`.** Both the scheduler loop and `POST /v1/inspector/policies/{id}/scans` freeze a scan, and a second copy of "which tables, which pairs, how many units" in the API is exactly how a manual scan comes to walk environments a narrower disabled policy excluded. One decomposition function, one enqueue function, two callers. `sauron-db` already depends on `sauron-inspector` (Task 19 Step 1) and on `sauron-core` for `Config`, so neither move adds an edge to the graph.

- [ ] **Step 1: Write the failing unit-decomposition test.** Create `backend/crates/sauron-inspector/src/units.rs` containing **only** this test module for now:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::targets::{PolicyTargetType, ScanPair};
      use chrono::TimeZone;
      use uuid::Uuid;

      fn pair(n: u128, env: Option<u128>) -> ScanPair {
          ScanPair { app_id: Uuid::from_u128(n), app_env_id: env.map(Uuid::from_u128) }
      }

      /// Units are ordered NEWEST DAY FIRST, so a scan killed halfway has
      /// already covered the most recent data — which is what an admin asking
      /// "does this app store email addresses" actually cares about.
      #[test]
      fn ranged_units_are_newest_day_first() {
          let to = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
          let from = to - chrono::Duration::days(3);
          let units = units_for(
              &[pair(1, Some(10))],
              &["error_events".to_string()],
              from,
              to,
              PolicyTargetType::App,
          );
          let days: Vec<String> = units
              .iter()
              .filter_map(|u| match u {
                  Unit::Ranged { day, .. } => Some(day.to_string()),
                  _ => None,
              })
              .collect();
          assert_eq!(days, ["2026-07-31", "2026-07-30", "2026-07-29"]);
      }

      /// The `_default` child is never tiered and never dropped, so those rows
      /// are the longest-lived PII in the system — and a time-windowed scan
      /// prunes them away precisely because their occurred_at is outside every
      /// explicit range. One extra unit per (table, app) covers them.
      #[test]
      fn a_default_sweep_unit_exists_per_table_and_app() {
          let to = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
          let units = units_for(
              &[pair(1, Some(10)), pair(1, Some(11))],
              &["error_events".to_string()],
              to - chrono::Duration::days(1),
              to,
              PolicyTargetType::App,
          );
          let defaults = units.iter().filter(|u| matches!(u, Unit::DefaultSweep { .. })).count();
          assert_eq!(defaults, 1, "one per (table, app), not per enrollment");
      }

      /// Neither rollups nor `_default` sweeps can be environment-attributed, so
      /// an env-scoped policy that ran them would persist key paths derived from
      /// PRODUCTION traffic under a policy an admin deliberately scoped to
      /// staging, readable by anyone with pii:read on staging.
      #[test]
      fn an_app_env_policy_gets_neither_rollups_nor_default_sweeps() {
          let to = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
          let units = units_for(
              &[pair(1, Some(10))],
              &["error_events".to_string(), "issues".to_string()],
              to - chrono::Duration::days(1),
              to,
              PolicyTargetType::AppEnv,
          );
          assert!(units.iter().all(|u| matches!(u, Unit::Ranged { .. })));
      }

      #[test]
      fn rollup_units_are_one_per_app_and_table() {
          let to = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
          let units = units_for(
              &[pair(1, Some(10)), pair(1, None), pair(2, Some(20))],
              &["issues".to_string(), "event_users".to_string()],
              to - chrono::Duration::days(1),
              to,
              PolicyTargetType::Project,
          );
          let rollups = units.iter().filter(|u| matches!(u, Unit::Rollup { .. })).count();
          assert_eq!(rollups, 4, "2 apps x 2 rollup tables");
      }

      /// The unit LIST is deterministically recomputable from the frozen window,
      /// params and targets, so only `{unit_index, row_cursor}` is persisted. A
      /// separate table would be ~13,500 bookkeeping rows for a 50-app project
      /// across a 30-day window, times 20 retained scans.
      #[test]
      fn the_unit_list_is_deterministic() {
          let to = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
          let from = to - chrono::Duration::days(5);
          let pairs = [pair(1, Some(10)), pair(2, None)];
          let tables = ["error_events".to_string(), "issues".to_string()];
          let a = units_for(&pairs, &tables, from, to, PolicyTargetType::Project);
          let b = units_for(&pairs, &tables, from, to, PolicyTargetType::Project);
          assert_eq!(a, b);
      }
  }
  ```

- [ ] **Step 2: Wire the module and run it, seeing it fail.** Add `pub mod units;` to `backend/crates/sauron-inspector/src/lib.rs`, then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector units`. Expected: `error[E0433]: failed to resolve: use of undeclared type 'Unit'`.

- [ ] **Step 3: Implement the unit model.** Prepend to `backend/crates/sauron-inspector/src/units.rs`, above the test module:
  ```rust
  //! Unit decomposition: how a frozen scan is cut into pieces small enough that
  //! one of them is a tick's worth of work.
  //!
  //! Pure on purpose, and NOT in the worker binary: the API freezes a manual
  //! scan and the scheduler freezes a scheduled one, and both must agree on the
  //! table list, the pair list and `units_total` down to the integer. A second
  //! copy of this in a handler is how a manual scan comes to walk environments
  //! a narrower disabled policy excluded.

  use chrono::{DateTime, Duration, NaiveDate, Utc};
  use uuid::Uuid;

  use crate::columns::{self, TableClass};
  use crate::targets::{self, PolicyTargetType, ScanPair};

  /// One indivisible piece of scan work.
  ///
  /// A unit is a single `(app, env, table, day)` for partitioned tables, so at
  /// most one day partition's pages are hot at a time — walking one ~30 MB child
  /// rather than the 678 MB parent is what keeps the ingest working set
  /// resident. It is also what bounds the phase-2 accumulator: keyed on
  /// `(column, path, matched_key, detector)`, its cardinality is keys x columns
  /// (~50 x 11 = 550 entries), so worker RSS is flat regardless of scan size.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Unit {
      Ranged { app_id: Uuid, env_id: Option<Uuid>, table: String, day: NaiveDate },
      /// The `_default` child, by name. Never tiered, never dropped.
      DefaultSweep { app_id: Uuid, table: String },
      /// A non-partitioned companion, PK keyset paginated.
      Rollup { app_id: Uuid, table: String },
  }

  /// Deterministically recompute a scan's unit list.
  ///
  /// Freezing `window_from`/`window_to`/`params`/`targets` is what makes this
  /// safe: an admin editing the policy mid-scan would otherwise silently change
  /// what unit #37 means, and a resume would walk a different list.
  pub fn units_for(
      pairs: &[ScanPair],
      tables: &[String],
      from: DateTime<Utc>,
      to: DateTime<Utc>,
      level: PolicyTargetType,
  ) -> Vec<Unit> {
      let mut units = Vec::new();
      let include_rollups = targets::include_rollups(level);

      // Newest day first.
      let mut days: Vec<NaiveDate> = Vec::new();
      let mut d = (to - Duration::days(1)).date_naive();
      while d >= from.date_naive() {
          days.push(d);
          d -= Duration::days(1);
      }

      for table in tables {
          match columns::table_class(table) {
              Some(TableClass::Partitioned) => {
                  for day in &days {
                      for p in pairs {
                          units.push(Unit::Ranged {
                              app_id: p.app_id,
                              env_id: p.app_env_id,
                              table: table.clone(),
                              day: *day,
                          });
                      }
                  }
                  if include_rollups {
                      let mut apps: Vec<Uuid> = pairs.iter().map(|p| p.app_id).collect();
                      apps.sort_unstable();
                      apps.dedup();
                      for app_id in apps {
                          units.push(Unit::DefaultSweep { app_id, table: table.clone() });
                      }
                  }
              }
              Some(TableClass::Rollup) => {
                  if !include_rollups {
                      continue;
                  }
                  let mut apps: Vec<Uuid> = pairs.iter().map(|p| p.app_id).collect();
                  apps.sort_unstable();
                  apps.dedup();
                  for app_id in apps {
                      units.push(Unit::Rollup { app_id, table: table.clone() });
                  }
              }
              // Not in the allowlist at all: silently absent, never scanned.
              None => {}
          }
      }
      units
  }

  /// The tables a policy scans: the default column set's tables plus whatever
  /// rollups it opted into.
  ///
  /// Takes the raw `rollups` jsonb rather than a policy row, so this stays in
  /// the pure crate that both `sauron-db` and the worker can call.
  pub fn tables_for(rollups: &serde_json::Value) -> Vec<String> {
      let mut tables = vec![
          "error_events".to_string(),
          "analytics_events".to_string(),
          "transactions".to_string(),
      ];
      if let Some(arr) = rollups.as_array() {
          for t in arr.iter().filter_map(|v| v.as_str()) {
              // Only names the inventory knows; a stale rollup id from a
              // downgraded binary must not become an interpolated identifier.
              if columns::table_class(t).is_some() && !tables.iter().any(|x| x == t) {
                  tables.push(t.to_string());
              }
          }
      }
      tables
  }
  ```

- [ ] **Step 4: Run and see the decomposition tests pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector units`. All five tests green.

- [ ] **Step 5: Implement the ONE enqueue both callers use.** Append to `backend/crates/sauron-db/src/repo.rs`:
  ```rust
  /// Why an enqueue did or did not produce a scan.
  ///
  /// An enum rather than a bool because the two callers need different
  /// answers from the same logic: the scheduler logs and moves on, the API
  /// turns each arm into a distinct status code.
  #[derive(Debug)]
  pub enum EnqueueOutcome {
      Queued(InspectorScan),
      /// The partial unique index refused a second active scan.
      AlreadyActive,
      /// The target is no longer inside the policy's org.
      TargetGone,
      /// Neither tracked keys nor detectors: it would report a confident false
      /// negative, which is the worst thing a privacy scan can emit.
      NoMatchers,
      /// Every target pair is covered by a more specific policy.
      FullySubtracted,
  }

  /// Freeze a policy into a scan row. The ONLY way a scan is created.
  ///
  /// Re-validates the target against the org even though the API already did:
  /// `inspector_policies.target_id` has no FK, and grants outlive targets.
  pub async fn enqueue_scan_for_policy(
      conn: &mut AsyncPgConnection,
      cfg: &sauron_core::Config,
      policy: &InspectorPolicy,
      trigger: &str,
      requested_by: Option<Uuid>,
  ) -> anyhow::Result<EnqueueOutcome> {
      if !validate_scope_in_org(conn, policy.org_id, &policy.target_type, policy.target_id).await? {
          return Ok(EnqueueOutcome::TargetGone);
      }
      let Some(level) = PolicyTargetType::from_sql(&policy.target_type) else {
          return Ok(EnqueueOutcome::TargetGone);
      };

      let keys = sauron_inspector::matching::parse_tracked_keys(&policy.tracked_keys);
      let dets = sauron_inspector::detect::parse_detectors(&policy.detectors);
      if keys.is_empty() && dets.is_empty() {
          return Ok(EnqueueOutcome::NoMatchers);
      }

      // Detector mode changes the cost model by an order of magnitude — no
      // prefilter, every row shipped out of Postgres, every string leaf walked —
      // so it gets its own much shorter window.
      let window_days = if dets.is_empty() {
          policy.window_days as i64
      } else {
          cfg.inspector_detector_window_days
      }
      .min(cfg.inspector_window_days);

      let to = Utc::now();
      let from = to - chrono::Duration::days(window_days);

      let pairs: Vec<ScanPair> = scan_pairs_for_node(conn, &policy.target_type, policy.target_id)
          .await?
          .into_iter()
          .map(|(app_id, app_env_id)| ScanPair { app_id, app_env_id })
          .collect();
      let narrower: Vec<PolicyNode> =
          list_inspector_policies_under(conn, &policy.target_type, policy.target_id)
              .await?
              .into_iter()
              .filter_map(|(t, id)| {
                  PolicyTargetType::from_sql(&t).map(|tt| PolicyNode { target_type: tt, target_id: id })
              })
              .collect();
      let node = PolicyNode { target_type: level, target_id: policy.target_id };
      let resolved = sauron_inspector::targets::resolve_targets(node, &pairs, &narrower);
      if resolved.pairs.is_empty() {
          return Ok(EnqueueOutcome::FullySubtracted);
      }

      let tables = tables_for(&policy.rollups);
      let units = units_for(&resolved.pairs, &tables, from, to, level);

      let params = serde_json::json!({
          "tracked_keys": policy.tracked_keys,
          "detectors": policy.detectors,
          "scan_columns": policy.scan_columns,
          "rollups": policy.rollups,
          "tables": tables,
          "level": policy.target_type,
      });
      let targets_json = serde_json::Value::Array(
          resolved
              .pairs
              .iter()
              .map(|p| serde_json::json!([p.app_id, p.app_env_id]))
              .collect(),
      );

      let scan = match insert_inspector_scan(
          conn,
          crate::models::NewInspectorScan {
              policy_id: policy.id,
              org_id: policy.org_id,
              trigger_type: trigger,
              requested_by,
              window_from: from,
              window_to: to,
              params: &params,
              targets: &targets_json,
              units_total: units.len() as i32,
          },
      )
      .await
      {
          Ok(s) => s,
          // The partial unique index refusing a second active scan is the
          // arbiter, not a handler check, so two schedulers racing produce one.
          Err(diesel::result::Error::DatabaseError(
              diesel::result::DatabaseErrorKind::UniqueViolation,
              _,
          )) => return Ok(EnqueueOutcome::AlreadyActive),
          Err(e) => return Err(e.into()),
      };

      if resolved.subtracted > 0 || resolved.truncated {
          let note = format!(
              "{} target pair(s) excluded by a more specific policy{}",
              resolved.subtracted,
              if resolved.truncated { "; target list truncated at the cap" } else { "" }
          );
          note_scan_coverage(conn, scan.id, "partial", &note).await?;
      }
      record_policy_scan(conn, policy.id, scan.id).await?;
      Ok(EnqueueOutcome::Queued(scan))
  }
  ```
  This needs `use sauron_inspector::targets::{PolicyNode, PolicyTargetType, ScanPair};` and `use sauron_inspector::units::{tables_for, units_for};` at the top of `repo.rs`.

  Then replace the Task 21 stub in `backend/bins/sauron-inspector/src/scan.rs` with the worker-side wrapper — the scheduler wants a bool and a log line, not the enum:
  ```rust
  /// Freeze a policy into a scan row for the scheduler. Returns whether a scan
  /// was actually queued.
  pub async fn enqueue_for_policy(
      conn: &mut AsyncPgConnection,
      cfg: &Config,
      policy: &InspectorPolicy,
      trigger: &str,
      requested_by: Option<Uuid>,
  ) -> anyhow::Result<bool> {
      match repo::enqueue_scan_for_policy(conn, cfg, policy, trigger, requested_by).await? {
          repo::EnqueueOutcome::Queued(scan) => {
              info!(scan_id = %scan.id, units = scan.units_total, "queued inspector scan");
              Ok(true)
          }
          repo::EnqueueOutcome::AlreadyActive => Ok(false),
          repo::EnqueueOutcome::TargetGone => {
              warn!(policy_id = %policy.id, "policy target is no longer in its org; not scanning");
              Ok(false)
          }
          // Rejected at the API with a 400; if one reaches here it must not
          // produce a confident false negative.
          repo::EnqueueOutcome::NoMatchers => {
              warn!(policy_id = %policy.id, "policy has neither tracked keys nor detectors; not scanning");
              Ok(false)
          }
          repo::EnqueueOutcome::FullySubtracted => {
              warn!(policy_id = %policy.id, "every target pair is covered by a narrower policy");
              Ok(false)
          }
      }
  }
  ```

- [ ] **Step 6: Add the coverage-note statement `enqueue_scan_for_policy` calls.** Append to `repo.rs`:
  ```rust
  /// Record a coverage downgrade on a scan without touching its status.
  pub async fn note_scan_coverage(
      conn: &mut AsyncPgConnection,
      scan_id: Uuid,
      coverage: &str,
      note: &str,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_scans SET coverage=$2, \
                  coverage_note = CASE WHEN coverage_note = '' THEN $3 \
                                       ELSE coverage_note || '; ' || $3 END \
           WHERE id=$1",
      )
      .bind::<SqlUuid, _>(scan_id)
      .bind::<Text, _>(coverage)
      .bind::<Text, _>(note)
      .execute(conn)
      .await
  }
  ```

- [ ] **Step 7: Rewrite the executor's header and imports.** Replace the top of `backend/bins/sauron-inspector/src/scan.rs` (above the wrapper from Step 5) with:
  ```rust
  //! The scan executor: recompute a frozen scan's units, run ONE unit per
  //! tick, flush, yield.
  //!
  //! One unit per tick rather than one whole scan, so the tick is short and the
  //! lease heartbeat is frequent. A scan that has held its lease for a full
  //! `INSPECTOR_LEASE_SECS` without finishing is a bug, not a design.

  use chrono::Duration;
  use sauron_core::Config;
  use sauron_db::models::InspectorPolicy;
  use sauron_db::repo::{self, FindingDelta};
  use sauron_db::{AsyncPgConnection, PgPool};
  use sauron_inspector::columns;
  use sauron_inspector::detect::{self, Detector};
  use sauron_inspector::matching::{self, TrackedKey};
  use sauron_inspector::prefilter;
  use sauron_inspector::redact;
  use sauron_inspector::targets::{PolicyTargetType, ScanPair};
  use sauron_inspector::units::{units_for, Unit};
  use sauron_inspector::walk;
  use serde_json::json;
  use std::collections::HashMap;
  use tracing::{info, warn};
  use uuid::Uuid;

  use crate::{checkout, release};
  ```
  The list is deliberately tight: `cargo clippy -- -D warnings` is a gate in this repo, so a leftover `DateTime`/`Utc`/`ColumnKind`/`Value` import from the earlier draft fails the build rather than merely warning. The window arithmetic, the TEXT-vs-jsonb decision and the `Utc::now()` all live in `repo::enqueue_scan_for_policy` and `repo::scan_window_rows` now.

- [ ] **Step 8: Implement the executor tick and the two phases.** Append to `scan.rs`:
  ```rust
  /// The phase-2 accumulator key. Bounded by keys x columns per unit, which is
  /// what keeps worker RSS flat regardless of scan size.
  type AccKey = (String, String, String, String);

  /// One unit per tick.
  pub async fn tick(pool: &PgPool, cfg: &Config, worker_id: &str) -> anyhow::Result<bool> {
      let mut conn = checkout(pool, cfg).await?;
      let claimed = repo::claim_one_scan(&mut conn, worker_id, cfg.inspector_lease_secs).await?;
      let Some(scan) = claimed else {
          release(conn).await;
          return Ok(false);
      };

      if scan.attempts > cfg.inspector_max_attempts {
          repo::finish_scan(
              &mut conn,
              scan.id,
              worker_id,
              "failed",
              "partial",
              "",
              "exceeded INSPECTOR_MAX_ATTEMPTS; one unit is failing repeatedly",
          )
          .await?;
          release(conn).await;
          return Ok(true);
      }

      // Recompute the unit list from the FROZEN inputs. Nothing about the live
      // policy is read here.
      let pairs: Vec<ScanPair> = scan
          .targets
          .as_array()
          .map(|arr| {
              arr.iter()
                  .filter_map(|p| {
                      let a = p.get(0)?.as_str()?.parse().ok()?;
                      let e = p.get(1).and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
                      Some(ScanPair { app_id: a, app_env_id: e })
                  })
                  .collect()
          })
          .unwrap_or_default();
      let tables: Vec<String> = scan.params["tables"]
          .as_array()
          .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
          .unwrap_or_default();
      let level = scan.params["level"]
          .as_str()
          .and_then(PolicyTargetType::from_sql)
          .unwrap_or(PolicyTargetType::App);
      let units = units_for(&pairs, &tables, scan.window_from, scan.window_to, level);

      let idx = scan.cursor["unit_index"].as_u64().unwrap_or(0) as usize;
      if idx >= units.len() {
          let coverage = if scan.coverage == "partial" { "partial" } else { "full" };
          repo::finish_scan(&mut conn, scan.id, worker_id, "succeeded", coverage, &scan.coverage_note, "").await?;
          release(conn).await;
          return Ok(true);
      }

      let keys = matching::parse_tracked_keys(&scan.params["tracked_keys"]);
      let dets = detect::parse_detectors(&scan.params["detectors"]);
      let unit = units[idx].clone();
      let outcome = run_unit(&mut conn, cfg, &scan, &unit, &keys, &dets, idx, worker_id).await;
      release(conn).await;

      match outcome {
          Ok(Some(cancelled)) if cancelled => {
              let mut conn = checkout(pool, cfg).await?;
              repo::finish_scan(
                  &mut conn,
                  scan.id,
                  worker_id,
                  "cancelled",
                  "partial",
                  "stopped by an operator",
                  "",
              )
              .await?;
              release(conn).await;
          }
          // The fence rejected the flush: another worker owns this scan now.
          // Abort the unit rather than retrying, or `match_count +
          // excluded.match_count` double-counts.
          Ok(None) => warn!(scan_id = %scan.id, "flush fenced out; another worker owns this scan"),
          Ok(Some(_)) => {}
          Err(e) => {
              warn!(scan_id = %scan.id, unit = idx, error = %e, "scan unit failed");
              let mut conn = checkout(pool, cfg).await?;
              repo::note_scan_coverage(&mut conn, scan.id, "partial", &format!("unit {idx} failed")).await?;
              release(conn).await;
          }
      }

      // The duty cycle. The whole feature is off by default, work proceeds in
      // INSPECTOR_BATCH_ROWS chunks, and this pause is what keeps the ingest
      // working set resident — the risk is buffer-cache eviction and CPU, not
      // lock contention (a seq scan takes ACCESS SHARE, which does not conflict
      // with INSERT's ROW EXCLUSIVE).
      tokio::time::sleep(std::time::Duration::from_millis(cfg.inspector_batch_pause_ms)).await;
      Ok(true)
  }

  /// Run one unit to completion. `Ok(None)` = fenced out; `Ok(Some(true))` =
  /// cancellation requested.
  #[allow(clippy::too_many_arguments)]
  async fn run_unit(
      conn: &mut AsyncPgConnection,
      cfg: &Config,
      scan: &sauron_db::models::InspectorScan,
      unit: &Unit,
      keys: &[TrackedKey],
      dets: &[Detector],
      idx: usize,
      worker_id: &str,
  ) -> anyhow::Result<Option<bool>> {
      let (table, app_id) = match unit {
          Unit::Ranged { table, app_id, .. }
          | Unit::DefaultSweep { table, app_id }
          | Unit::Rollup { table, app_id } => (table.clone(), *app_id),
      };

      // The policy's OPT-IN column set, frozen into params at enqueue. Reading
      // it is what makes `breadcrumbs`, `sdk`, `debug_meta`, `stacktrace`,
      // `stacktrace_symbolicated`, `identities.alias_id`/`distinct_id` and
      // `workflows.cancel_reason` reachable at all — every one of them is
      // `default_on: false`, so `default_columns` alone can never return them
      // and a rollup unit for `identities`/`workflows` would scan nothing.
      // NULL (the shipped default) means "the default set".
      let cols: Vec<&'static columns::ScanColumn> = match scan.params["scan_columns"].as_array() {
          Some(names) => names
              .iter()
              .filter_map(|v| v.as_str())
              // `find` is the allowlist: a name from a downgraded binary or a
              // hand-edited row is dropped, never interpolated.
              .filter_map(|n| columns::find(&table, n))
              .collect(),
          None => columns::default_columns(&table),
      };
      if cols.is_empty() {
          return Ok(Some(false));
      }
      let (patterns, text_patterns) = if prefilter::use_prefilter(keys, dets) {
          (prefilter::key_patterns(keys), prefilter::text_key_patterns(keys))
      } else {
          (Vec::new(), Vec::new())
      };

      // The default child has no time bound at all — that is the whole point of
      // sweeping it — so it gets its own budget instead of the per-unit one.
      let row_cap = match unit {
          Unit::DefaultSweep { .. } => cfg.inspector_default_sweep_rows,
          _ => cfg.inspector_max_phase2_rows_per_unit,
      };

      let mut acc: HashMap<AccKey, FindingDelta> = HashMap::new();
      let mut rows_seen: i64 = 0;
      let mut truncated = false;
      let mut cursor = repo::ScanCursor::default();

      loop {
          let page = repo::scan_window_rows(
              conn,
              &table,
              &cols.iter().map(|c| c.column).collect::<Vec<_>>(),
              app_id,
              unit_shape(unit),
              cursor,
              cfg.inspector_batch_rows,
              &patterns,
              &text_patterns,
          )
          .await?;
          if page.is_empty() {
              break;
          }
          rows_seen += page.len() as i64;
          for row in &page {
              cursor = repo::ScanCursor { occurred_at: row.occurred_at, id: Some(row.id) };
              accumulate(&mut acc, scan, unit, &table, row, keys, dets);
          }
          if rows_seen >= row_cap {
              // Hitting the cap sets match_count_exact = false on this unit's
              // findings and coverage = 'partial' — never a silent truncation.
              truncated = true;
              break;
          }
          if page.len() < cfg.inspector_batch_rows as usize {
              break;
          }
          tokio::time::sleep(std::time::Duration::from_millis(cfg.inspector_batch_pause_ms)).await;
      }

      let mut deltas: Vec<FindingDelta> = acc.into_values().collect();
      if truncated {
          for d in &mut deltas {
              d.match_count_exact = false;
          }
      }
      let flushed = repo::flush_scan_unit(
          conn,
          scan.id,
          worker_id,
          &json!({"unit_index": idx + 1}),
          (idx + 1) as i32,
          rows_seen,
          &deltas,
      )
      .await?;
      if truncated {
          let key = match unit {
              Unit::DefaultSweep { .. } => "INSPECTOR_DEFAULT_SWEEP_ROWS",
              _ => "INSPECTOR_MAX_PHASE2_ROWS_PER_UNIT",
          };
          repo::note_scan_coverage(
              conn,
              scan.id,
              "partial",
              &format!("a unit hit {key}; counts are lower bounds"),
          )
          .await?;
      }
      Ok(flushed.map(|o| o.cancel_requested_at.is_some()))
  }

  /// Which statement shape a unit reads with.
  ///
  /// `DefaultSweep` reads the `_default` CHILD BY NAME with no time predicate
  /// at all. Re-running the parent over the scan window — which is what a
  /// `Some((window_from, window_to))` range would do — reads exactly the rows
  /// the `Ranged` units already read (double-counting `match_count` and
  /// `rows_scanned`) while pruning away the only rows this phase exists for:
  /// rows are in the default child PRECISELY BECAUSE their `occurred_at` falls
  /// outside every explicit range, so a windowed query can never see them.
  ///
  /// `Rollup` reads a non-partitioned companion. `issues`, `event_users` and
  /// `identities` have neither an `occurred_at` nor an `environment_id` column,
  /// so any predicate on either is `column "occurred_at" does not exist` — and
  /// `inspector_policies.rollups` defaults to `["issues","event_users"]`, so
  /// that fires on the shipped default policy.
  fn unit_shape(unit: &Unit) -> repo::ScanShape {
      match unit {
          Unit::Ranged { day, env_id, .. } => {
              let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
              repo::ScanShape::Ranged { env_id: *env_id, from: lo, to: lo + Duration::days(1) }
          }
          Unit::DefaultSweep { .. } => repo::ScanShape::DefaultChild,
          Unit::Rollup { .. } => repo::ScanShape::Rollup,
      }
  }

  /// Phase 2: parse only the rows that survived the prefilter, walk each scanned
  /// column, and fold matches into the accumulator.
  fn accumulate(
      acc: &mut HashMap<AccKey, FindingDelta>,
      scan: &sauron_db::models::InspectorScan,
      unit: &Unit,
      table: &str,
      row: &repo::ScanRow,
      keys: &[TrackedKey],
      dets: &[Detector],
  ) {
      let (env_scope, environment_id) = match unit {
          Unit::Rollup { .. } => ("no_env_column", None),
          Unit::Ranged { env_id: Some(e), .. } => ("enrollment", Some(*e)),
          _ => ("unattributed", None),
      };
      let partition_kind = match unit {
          Unit::Ranged { .. } => "ranged",
          Unit::DefaultSweep { .. } => "default",
          Unit::Rollup { .. } => "rollup",
      };

      for (column, value) in &row.columns {
          // A TEXT column arrives as `to_jsonb(col)`, i.e. a JSON SCALAR, and
          // the walker returns nothing for a scalar root (its own test asserts
          // `walk(&json!("[Circular]")).is_empty()`). Without this branch none
          // of the ten `default_on` TEXT columns — `error_events.title`,
          // `culprit`, `message`, `exception_value`, `exception_type`,
          // `issues.title`, `culprit`, `transactions.url` — could EVER produce
          // a finding, and those are exactly what the Issues list renders. The
          // column name is the key; there is no path inside a scalar.
          let leaves: Vec<walk::Leaf<'_>> = if value.is_object() || value.is_array() {
              walk::walk(value)
          } else {
              vec![walk::Leaf { path: String::new(), key: column.to_lowercase(), value }]
          };
          for leaf in leaves {
              let (matched_key, detector) = match matching::matched(keys, &leaf) {
                  Some(k) => (k.key.clone(), String::new()),
                  None => match leaf.value.as_str().and_then(|s| detect::detect_first(dets, s)) {
                      Some(d) => (leaf.key.clone(), d.id().to_string()),
                      None => continue,
                  },
              };
              // key_path is UNTRUSTED INPUT: object keys are arbitrary
              // dev-controlled UTF-8, so a payload shaped
              // `extra.customers["jane@acme.com"].email` would write raw PII
              // straight into a column every pii:read holder can read with no
              // reveal call and no audit row.
              let key_path = redact::redact_path(&leaf.path);
              let k: AccKey = (column.clone(), key_path.clone(), matched_key.clone(), detector.clone());
              let entry = acc.entry(k).or_insert_with(|| FindingDelta {
                  org_id: scan.org_id,
                  app_id: match unit {
                      Unit::Ranged { app_id, .. }
                      | Unit::DefaultSweep { app_id, .. }
                      | Unit::Rollup { app_id, .. } => *app_id,
                  },
                  environment_id,
                  env_scope: env_scope.to_string(),
                  source_table: table.to_string(),
                  source_column: column.clone(),
                  key_path,
                  matched_key,
                  detector,
                  value_type: redact::value_type(leaf.value).to_string(),
                  match_count: 0,
                  match_count_exact: true,
                  sample_preview: redact::preview(leaf.value),
                  sample_row_id: Some(row.id),
                  // NULL on a rollup: `issues`, `event_users` and `identities`
                  // have no `occurred_at` column to read one from.
                  sample_occurred_at: row.occurred_at,
                  partition_kind: partition_kind.to_string(),
                  first_seen_at: row.occurred_at,
                  last_seen_at: row.occurred_at,
              });
              entry.match_count += 1;
              if let Some(ts) = row.occurred_at {
                  if entry.first_seen_at.is_none_or(|f| ts < f) {
                      entry.first_seen_at = Some(ts);
                  }
                  if entry.last_seen_at.is_none_or(|l| ts > l) {
                      entry.last_seen_at = Some(ts);
                  }
              }
          }
      }
  }
  ```

- [ ] **Step 9: Add the phase-1 read to `repo.rs`.** Append:
  ```rust
  /// One phase-1 page. THREE statement shapes, because the tables genuinely
  /// differ and one shape produces `column "occurred_at" does not exist`.
  ///
  /// `Ranged` is the partitioned case: an INDEX-BOUNDED inner window, then the
  /// prefilter on the outer statement. Both halves matter. Putting the LIMIT on
  /// the same statement as the ILIKE bounds MATCHES, not SCANNED ROWS — and the
  /// design's premise is that the prefilter eliminates 95-99% of rows, so such a
  /// statement must scan the ENTIRE app-day range to emit fewer than `limit`
  /// rows. Three consequences, all bad: no heartbeat and no inter-batch pause
  /// for the whole scan (so the duty cycle is fiction); `statement_timeout`
  /// aborts somewhere around 2-3M rows per app-day; and on abort THE CURSOR
  /// NEVER ADVANCES, so the retry replays the identical statement and
  /// `INSPECTOR_MAX_ATTEMPTS` permanently fails the scan. The
  /// `(app_id, environment_id, occurred_at)` predicate matches
  /// `error_events_app_env_time_idx` / `analytics_events_app_env_time_idx`
  /// exactly.
  ///
  /// `DefaultChild` reads `{table}_default` BY NAME with no time predicate: the
  /// rows are in that child precisely because their `occurred_at` is outside
  /// every explicit range, so a windowed query cannot see them. The child name
  /// is derived from our own suffix, never from input — the same construction
  /// `mask_default_partition_batch` uses.
  ///
  /// `Rollup` reads a non-partitioned companion with an `id` keyset and NO time
  /// and NO environment predicate. `issues`, `event_users` and `identities`
  /// have neither column, and `inspector_policies.rollups` defaults to
  /// `["issues","event_users"]` — so a shared shape here fails on the shipped
  /// default policy, not on some exotic configuration.
  ///
  /// Column names are `&'static str`s from the inventory; every value is bound.
  #[derive(Debug, Clone, Copy)]
  pub enum ScanShape {
      Ranged { env_id: Option<Uuid>, from: DateTime<Utc>, to: DateTime<Utc> },
      DefaultChild,
      Rollup,
  }

  /// Keyset position. `occurred_at` is `None` for a rollup, which has none.
  #[derive(Debug, Clone, Copy, Default)]
  pub struct ScanCursor {
      pub occurred_at: Option<DateTime<Utc>>,
      pub id: Option<Uuid>,
  }

  pub struct ScanRow {
      pub id: Uuid,
      /// `None` on a rollup. `inspector_findings.first_seen_at` /
      /// `last_seen_at` / `sample_occurred_at` are nullable for this reason.
      pub occurred_at: Option<DateTime<Utc>>,
      pub columns: Vec<(String, Value)>,
  }

  #[allow(clippy::too_many_arguments)]
  pub async fn scan_window_rows(
      conn: &mut AsyncPgConnection,
      table: &str,
      cols: &[&'static str],
      app_id: Uuid,
      shape: ScanShape,
      cursor: ScanCursor,
      limit: i64,
      patterns: &[String],
      text_patterns: &[String],
  ) -> QueryResult<Vec<ScanRow>> {
      // The identifiers come from the inventory in `sauron-inspector`; refuse
      // anything else rather than interpolating it. `table` is always the PARENT
      // name even for the default child, so this check is never bypassed.
      if sauron_inspector::columns::table_class(table).is_none()
          || cols.iter().any(|c| sauron_inspector::columns::find(table, c).is_none())
      {
          return Ok(Vec::new());
      }
      let payload = cols
          .iter()
          .map(|c| format!("'{c}', to_jsonb(e.{c})"))
          .collect::<Vec<_>>()
          .join(", ");

      // A TEXT column holds no JSON, so the quoted `%"email"%` pattern the jsonb
      // columns use matches nothing in it — which is how ten `default_on` TEXT
      // columns come to report zero findings with `coverage='full'`. Each column
      // gets the pattern array for its own kind.
      // Both pattern arrays are ALWAYS bound, so the statement must ALWAYS
      // mention both. Postgres derives a prepared statement's parameter count
      // from the highest `$n` it can see and answers `bind message supplies 9
      // parameters, but prepared statement requires 4` otherwise. Two ways to
      // hit that without this floor: a detector-only policy (no prefilter at
      // all, both arrays empty) and any all-jsonb column set (`$6` never
      // referenced), which is `analytics_events`' entire default set.
      const PARAM_FLOOR: &str = " AND ($5::text[] IS NOT NULL OR $6::text[] IS NOT NULL OR TRUE)";
      let ilike = if patterns.is_empty() && text_patterns.is_empty() {
          PARAM_FLOOR.to_string()
      } else {
          let ors = cols
              .iter()
              .map(|c| {
                  let is_text = sauron_inspector::columns::find(table, c)
                      .map(|e| e.kind == sauron_inspector::columns::ColumnKind::Text)
                      .unwrap_or(false);
                  if is_text {
                      format!("e.{c} ILIKE ANY($6)")
                  } else {
                      format!("e.{c}::text ILIKE ANY($5)")
                  }
              })
              .collect::<Vec<_>>()
              .join(" OR ");
          format!("{PARAM_FLOOR} AND ({ors})")
      };

      let (sql, env_id, lo, hi) = match shape {
          ScanShape::Ranged { env_id, from, to } => (
              format!(
                  "WITH win AS ( \
                     SELECT id, occurred_at FROM {table} \
                     WHERE app_id = $1 AND ($2::uuid IS NULL OR environment_id = $2) \
                       AND occurred_at >= $3 AND occurred_at < $4 \
                       AND ($7::timestamptz IS NULL OR (occurred_at, id) > ($7, $8)) \
                     ORDER BY occurred_at, id LIMIT $9) \
                   SELECT e.id, e.occurred_at, jsonb_build_object({payload}) AS payload \
                   FROM {table} e JOIN win ON e.id = win.id AND e.occurred_at = win.occurred_at \
                   WHERE e.occurred_at >= $3 AND e.occurred_at < $4{ilike} \
                   ORDER BY e.occurred_at, e.id"
              ),
              env_id,
              from,
              to,
          ),
          ScanShape::DefaultChild => {
              let child = format!("{table}_default");
              (
                  format!(
                      "WITH win AS ( \
                         SELECT id, occurred_at FROM {child} \
                         WHERE app_id = $1 AND ($2::uuid IS NULL OR TRUE) \
                           AND ($3::timestamptz IS NULL OR TRUE) \
                           AND ($4::timestamptz IS NULL OR TRUE) \
                           AND ($7::timestamptz IS NULL OR (occurred_at, id) > ($7, $8)) \
                         ORDER BY occurred_at, id LIMIT $9) \
                       SELECT e.id, e.occurred_at, jsonb_build_object({payload}) AS payload \
                       FROM {child} e JOIN win ON e.id = win.id AND e.occurred_at = win.occurred_at \
                       WHERE TRUE{ilike} \
                       ORDER BY e.occurred_at, e.id"
                  ),
                  None,
                  DateTime::<Utc>::MIN_UTC,
                  DateTime::<Utc>::MAX_UTC,
              )
          }
          // No `occurred_at`, no `environment_id`, no window CTE: these tables
          // are orders of magnitude smaller than the event tables and one `id`
          // keyset walks them.
          ScanShape::Rollup => (
              format!(
                  "SELECT e.id, NULL::timestamptz AS occurred_at, \
                          jsonb_build_object({payload}) AS payload \
                   FROM {table} e \
                   WHERE e.app_id = $1 AND ($2::uuid IS NULL OR TRUE) \
                     AND ($3::timestamptz IS NULL OR TRUE) \
                     AND ($4::timestamptz IS NULL OR TRUE) \
                     AND ($7::timestamptz IS NULL OR TRUE) \
                     AND ($8::uuid IS NULL OR e.id > $8){ilike} \
                   ORDER BY e.id LIMIT $9"
              ),
              None,
              DateTime::<Utc>::MIN_UTC,
              DateTime::<Utc>::MAX_UTC,
          ),
      };
      // Every shape binds all nine parameters in the same order, with the
      // irrelevant ones neutralized by an `IS NULL OR TRUE`. Postgres rejects a
      // statement whose parameter count does not match the bind list, and a
      // per-shape bind list is a fourth thing to keep in sync.

      #[derive(QueryableByName)]
      struct RawRow {
          #[diesel(sql_type = SqlUuid)]
          id: Uuid,
          #[diesel(sql_type = Nullable<Timestamptz>)]
          occurred_at: Option<DateTime<Utc>>,
          #[diesel(sql_type = Jsonb)]
          payload: Value,
      }
      let rows: Vec<RawRow> = diesel::sql_query(sql)
          .bind::<SqlUuid, _>(app_id)
          .bind::<Nullable<SqlUuid>, _>(env_id)
          .bind::<Timestamptz, _>(lo)
          .bind::<Timestamptz, _>(hi)
          .bind::<Array<Text>, _>(patterns.to_vec())
          .bind::<Array<Text>, _>(text_patterns.to_vec())
          .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
          .bind::<Nullable<SqlUuid>, _>(cursor.id)
          .bind::<BigInt, _>(limit)
          .load(conn)
          .await?;
      Ok(rows
          .into_iter()
          .map(|r| ScanRow {
              id: r.id,
              occurred_at: r.occurred_at,
              columns: r
                  .payload
                  .as_object()
                  .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                  .unwrap_or_default(),
          })
          .collect())
  }
  ```

- [ ] **Step 10: Build the workspace and run the unit tests.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector -p sauron-inspector-bin`. The five decomposition tests green, the crate's other suites still green, no warnings.

- [ ] **Step 11: Drive one real scan end to end, across all three shapes.** With the API and ingest running, seed one app-env with rows containing `extra.customer.email`, one row in `error_events_default`, one `error_events` row whose `title` literally contains `email`, and one `issues` row for the same app. Create a policy tracking `email` with the default `rollups` (`["issues","event_users"]`). Then run `cd /home/splimter/projects/freelance/sauron/backend && INSPECTOR_ENABLED=1 DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu timeout 120 cargo run --bin sauron-inspector`. Then `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "SELECT source_table, source_column, key_path, match_count, partition_kind, env_scope, sample_preview FROM inspector_findings ORDER BY match_count DESC"`. Expected, and each one is a distinct bug if it is missing:
  - a `partition_kind = 'ranged'` row with `key_path = 'customer.email'`;
  - a `partition_kind = 'default'` row — proving the sweep read the `_default` child rather than re-reading the parent;
  - a `source_column = 'title'` row with an EMPTY `key_path` — proving a TEXT column can produce a finding at all;
  - a `source_table = 'issues'`, `partition_kind = 'rollup'`, `env_scope = 'no_env_column'` row with a NULL `sample_occurred_at` — proving the rollup shape ran without an `occurred_at` predicate;
  - and **no `sample_preview` or `key_path` containing the seeded email**.

  Confirm the worker log contains no `column "occurred_at" does not exist`, and that `SELECT rows_scanned FROM inspector_scans` is not roughly double the seeded row count — a `DefaultSweep` that re-read the parent shows up exactly there.

- [ ] **Step 12: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 23: `mask.rs` and `preview.rs` — the retro-mask job and its counting twin

**Files:**
- Modify `backend/bins/sauron-inspector/src/mask.rs`
- Modify `backend/bins/sauron-inspector/src/preview.rs`

**Interfaces:**
- Consumes: `repo::{claim_mask_action, mask_batch_jsonb, mask_batch_jsonb_wildcard, mask_batch_text, mask_default_partition_batch, mask_rollup_batch, mask_tail_sweep_batch, count_batch_jsonb, count_batch_text, set_mask_phase, finish_preview, finish_mask_action, fail_mask_action, insert_masked_keys, get_watermark, BatchCursor}` (Tasks 18–20), `sauron_inspector::targets::{MaskTarget, TargetTable, TargetColumn}` + `sauron_inspector::path::parse_mask_path` (Tasks 11–12).
- Produces: `mask::{tick, day_floor, batch_size, parse_targets}`; `preview::tick`; `repo::{user_is_active_with_app_permission, add_cold_skip}`.

- [ ] **Step 1: Write the failing hot/cold-boundary test.** Append to `backend/bins/sauron-inspector/src/mask.rs`:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use chrono::TimeZone;

      /// The floor is recomputed PER DAY, not once at job start.
      ///
      /// `sauron-tier` defers the drop to a LATER cycle than the export — its own
      /// comment calls this "a real grace window" — and the masker grinds
      /// oldest-day-first for potentially hours. Two silent failures follow from
      /// a floor computed once: the masker updates rows in a partition already
      /// COPY'd to Parquet but not yet dropped, so Postgres shows masked,
      /// Parquet holds raw, and the drop destroys the only masked copy; and a day
      /// dropped mid-run matches zero rows while the action still reports `done`
      /// with `rows_masked > 0`.
      #[test]
      fn the_floor_refuses_a_day_at_or_below_the_watermark() {
          let now = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap();
          let watermark = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 0).unwrap();
          let floor = day_floor(now, 30, Some(watermark), 3600);
          assert!(floor > watermark);
          assert_eq!(floor, watermark + chrono::Duration::seconds(3600));
      }

      #[test]
      fn without_a_watermark_the_floor_is_the_hot_window() {
          let now = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap();
          assert_eq!(day_floor(now, 30, None, 3600), now - chrono::Duration::days(30));
      }

      /// Wildcard targets halve the batch, because the array rebuild
      /// re-serializes the whole array per row and is measurably more expensive
      /// than the 186 us/row jsonb_set case.
      #[test]
      fn a_wildcard_target_halves_the_batch() {
          let plain = vec![MaskTarget {
              table: TargetTable::ErrorEvents,
              column: TargetColumn::Extra,
              path: "customer.email".into(),
          }];
          // `error_events.breadcrumbs` IS the array, so the path is relative to
          // it and the wildcard is bare — see Task 11.
          let wild = vec![MaskTarget {
              table: TargetTable::ErrorEvents,
              column: TargetColumn::Breadcrumbs,
              path: "[*].data.email".into(),
          }];
          assert_eq!(batch_size(2000, &plain), 2000);
          assert_eq!(batch_size(2000, &wild), 1000);
      }

      /// `targets` is read back out of Postgres in a DIFFERENT PROCESS from the
      /// one that validated it, so an unknown table/column must fail the action
      /// rather than reach an interpolated identifier.
      #[test]
      fn an_unparseable_target_list_is_rejected() {
          let good = serde_json::json!([{"table": "error_events", "column": "extra", "path": "a.b"}]);
          assert!(parse_targets(&good).is_ok());
          let bad = serde_json::json!([{"table": "auth_sessions", "column": "token", "path": ""}]);
          assert!(parse_targets(&bad).is_err());
          let alsobad = serde_json::json!([{"table": "error_events", "column": "extra", "path": "a.3.b"}]);
          assert!(parse_targets(&alsobad).is_err());
          assert!(parse_targets(&serde_json::json!([])).is_err());
      }
  }
  ```

- [ ] **Step 2: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector-bin mask`. Expected: `error[E0425]: cannot find function 'day_floor' in this scope`.

- [ ] **Step 3: Implement the shared helpers.** Replace the stub body of `backend/bins/sauron-inspector/src/mask.rs` with this, keeping the test module at the bottom:
  ```rust
  //! The retro-mask job. Each `inspector_mask_actions` row is simultaneously the
  //! queue, the cursor, the progress meter and the audit record.
  //!
  //! Event tables are append-only, so mask UPDATEs never contend with ingest for
  //! row locks. The shared cost is WAL, buffer cache and 13 index updates per
  //! `error_events` row: MEASURED 186 us/row on `extra`, 136 us/row on `tags`. A
  //! 2000-row batch is ~0.37 s of write; with the 200 ms pause that is a ~65%
  //! duty cycle. A 210k-row full pass is ~60 s of write plus roughly a doubling
  //! of live tuples until autovacuum catches up, and a pass covers the whole
  //! TIER_HOT_DAYS window — budget from the row count that window actually
  //! holds, not from a sample day.
  //!
  //! The job deliberately does NOT run VACUUM — it sets `vacuum_advised` and
  //! emits a `warn!`, because an unattended VACUUM is exactly the kind of
  //! surprise an operator should authorize.

  use chrono::{DateTime, Duration, NaiveDate, Utc};
  use sauron_core::Config;
  use sauron_db::repo::{self, BatchCursor, BatchOutcome};
  use sauron_db::{AsyncPgConnection, PgPool};
  use sauron_inspector::columns::{self, ColumnKind};
  use sauron_inspector::path::parse_mask_path;
  use sauron_inspector::targets::{validate_target, MaskTarget, TargetColumn, TargetTable};
  use serde_json::Value;
  use tracing::{info, warn};
  use uuid::Uuid;

  use crate::{checkout, release};

  /// The permission the requester must still hold at claim time. Named here
  /// rather than importing `sauron-auth`, so this binary keeps its dependency
  /// list to the four crates it actually needs.
  const PII_MANAGE: &str = "pii:manage";

  /// The oldest instant this pass may write to, recomputed PER DAY.
  ///
  /// Reuses `symbolicate_with`'s expression and its comment "never write into
  /// cold/exported partitions": an exported partition already holds the raw
  /// bytes in immutable Parquet, so masking the Postgres copy buys nothing while
  /// paying the full write cost, and a partition that is exported-but-not-
  /// yet-dropped is on the tier worker's critical path.
  ///
  /// A floor computed from `tier_hot_days` alone is NOT sufficient however long
  /// the window is, because `sauron-tier` defers the drop to a later cycle than
  /// the export. The watermark plus one tier tick is the real boundary.
  pub fn day_floor(
      now: DateTime<Utc>,
      tier_hot_days: i64,
      watermark: Option<DateTime<Utc>>,
      tier_tick_secs: i64,
  ) -> DateTime<Utc> {
      let hot = now - Duration::days(tier_hot_days);
      match watermark {
          Some(w) => hot.max(w + Duration::seconds(tier_tick_secs)),
          None => hot,
      }
  }

  /// Halve the batch when any target carries a wildcard.
  pub fn batch_size(base: i64, targets: &[MaskTarget]) -> i64 {
      if targets.iter().any(|t| t.path.contains("[*]")) {
          (base / 2).max(1)
      } else {
          base
      }
  }

  /// Deserialize the frozen target list into ENUMS.
  ///
  /// SQL identifiers cannot be bound, so the batch statements interpolate the
  /// table and column names. This process is not the one that validated them, so
  /// "validated in Rust at write time" is not a control — an unknown pair fails
  /// the action instead of reaching an interpolated identifier in an unattended
  /// UPDATE running with full DB rights.
  pub fn parse_targets(v: &Value) -> Result<Vec<MaskTarget>, String> {
      let arr = v.as_array().ok_or_else(|| "targets is not an array".to_string())?;
      let mut out = Vec::with_capacity(arr.len());
      for item in arr {
          let t: MaskTarget =
              serde_json::from_value(item.clone()).map_err(|e| format!("unknown mask target: {e}"))?;
          validate_target(&t).map_err(|e| format!("invalid mask target: {e:?}"))?;
          out.push(t);
      }
      if out.is_empty() {
          return Err("targets is empty".to_string());
      }
      Ok(out)
  }

  /// The `text[]` a jsonb target lowers to, plus whether it is a wildcard.
  fn path_parts(t: &MaskTarget) -> (Vec<String>, bool) {
      match parse_mask_path(&t.path) {
          Ok(p) if p.wildcard => (p.sub_array(), true),
          Ok(p) => (p.text_array(), false),
          Err(_) => (Vec::new(), false),
      }
  }

  fn is_text_column(t: &MaskTarget) -> bool {
      columns::find(t.table.as_sql(), t.column.as_sql())
          .map(|c| c.kind == ColumnKind::Text)
          .unwrap_or(false)
  }
  ```

- [ ] **Step 4: Implement the executor.** Append to `mask.rs`, above the test module:
  ```rust
  /// One action per tick, run to completion or to cancellation.
  ///
  /// `LIMIT 1` on the claim is deliberate: masking is heavy write and one action
  /// at a time per worker IS the throttle; N workers take N different actions.
  pub async fn tick(pool: &PgPool, cfg: &Config, worker_id: &str) -> anyhow::Result<bool> {
      let mut conn = checkout(pool, cfg).await?;
      let claimed =
          repo::claim_mask_action(&mut conn, "mask", worker_id, cfg.inspector_claim_stale_secs).await?;
      let Some(action) = claimed else {
          release(conn).await;
          return Ok(false);
      };

      // AUTHORIZATION IS RE-CHECKED AT CLAIM. Confirm re-authorizes, but the
      // action then sits in `pending` — with one slot per worker and a 200 ms
      // inter-batch pause, a backlog can be hours deep. A member whose grant was
      // revoked, or whose account was deactivated (which revokes refresh tokens
      // and touches nothing queued), must not have their queued destruction run.
      if let Some(user_id) = action.requested_by {
          match repo::user_is_active_with_app_permission(&mut conn, user_id, action.app_id, PII_MANAGE)
              .await
          {
              Ok(true) => {}
              Ok(false) => {
                  repo::fail_mask_action(
                      &mut conn,
                      action.id,
                      "requester no longer holds pii:manage on this app, or is deactivated",
                  )
                  .await?;
                  release(conn).await;
                  return Ok(true);
              }
              Err(e) => {
                  warn!(action_id = %action.id, error = %e, "could not re-authorize mask requester");
                  release(conn).await;
                  return Ok(true);
              }
          }
      }

      let targets = match parse_targets(&action.targets) {
          Ok(t) => t,
          Err(reason) => {
              repo::fail_mask_action(&mut conn, action.id, &reason).await?;
              release(conn).await;
              return Ok(true);
          }
      };
      let limit = batch_size(cfg.inspector_mask_batch, &targets);
      let now = Utc::now();
      let mut cold_boundary = day_floor(now, cfg.tier_hot_days, None, cfg.tier_tick_secs as i64);
      let mut cancelled = false;

      // --- phase 'hot'. OLDEST day first, so the rows closest to the tier
      // boundary — the ones about to become permanently unreachable — go first.
      let mut day = action
          .day_cursor
          .unwrap_or_else(|| (now - Duration::days(cfg.tier_hot_days)).date_naive());
      while day < now.date_naive() && !cancelled {
          // Recomputed PER DAY, watermark re-read with it.
          let wm = repo::get_watermark(&mut conn, "error_events").await.unwrap_or(None);
          cold_boundary = day_floor(now, cfg.tier_hot_days, wm, cfg.tier_tick_secs as i64);
          if day.and_hms_opt(0, 0, 0).unwrap().and_utc() < cold_boundary {
              // `cold_rows_skipped` counts ROWS, not days: an operator reading
              // it next to `rows_masked` on a `done` action is comparing two
              // row counts, and the CSV header, the Audit column and the
              // MaskDialog all say rows. Count exactly what this day WOULD have
              // masked, with the same predicates the mask uses.
              let mut skipped: i64 = 0;
              for t in targets.iter().filter(|t| t.table.is_partitioned()) {
                  skipped += if is_text_column(t) {
                      repo::count_batch_text(&mut conn, t.table, t.column, action.app_id, day)
                          .await
                          .unwrap_or(0)
                  } else {
                      let (path, _) = path_parts(t);
                      if path.is_empty() {
                          0
                      } else {
                          repo::count_batch_jsonb(
                              &mut conn, t.table, t.column, action.app_id, day, &path,
                          )
                          .await
                          .unwrap_or(0)
                      }
                  };
              }
              if skipped > 0 {
                  repo::add_cold_skip(&mut conn, action.id, skipped).await?;
              }
              day += Duration::days(1);
              continue;
          }
          repo::set_mask_phase(&mut conn, action.id, worker_id, "hot", Some(day)).await?;
          for t in targets.iter().filter(|t| t.table.is_partitioned()) {
              let mut cursor = BatchCursor::default();
              loop {
                  let Some(out) =
                      run_partitioned_batch(&mut conn, t, action.app_id, day, cursor, limit, action.id, worker_id)
                          .await?
                  else {
                      // Fenced out: another worker owns this action now. Abort
                      // rather than retry, or the counters double-count.
                      release(conn).await;
                      return Ok(true);
                  };
                  if out.status == "cancelling" {
                      cancelled = true;
                      break;
                  }
                  match out.next_cursor {
                      Some(c) => cursor = c,
                      None => break,
                  }
                  tokio::time::sleep(std::time::Duration::from_millis(cfg.inspector_mask_pause_ms)).await;
              }
              if cancelled {
                  break;
              }
          }
          day += Duration::days(1);
      }

      // --- phase 'default_partition', bounded by the SAME floor.
      //
      // `repo::list_child_partitions` excludes `{table}_default` by design, so
      // those rows are never tiered and never dropped — the longest-lived PII in
      // the system. Rows CANNOT be there inside a covered range (Postgres
      // rejects `CREATE TABLE ... PARTITION OF ...` if the default holds a
      // conflicting row); they are there because their occurred_at is OUTSIDE
      // every explicit range. Which is exactly why the floor still applies: an
      // unbounded sweep would rewrite rows years older than tier_hot_days,
      // contradicting the hot/cold rule and the cold_rows_skipped number.
      if !cancelled {
          repo::set_mask_phase(&mut conn, action.id, worker_id, "default_partition", None).await?;
          for t in targets.iter().filter(|t| t.table.is_partitioned() && !is_text_column(t)) {
              let (path, wildcard) = path_parts(t);
              if wildcard || path.is_empty() {
                  continue;
              }
              let mut cursor = BatchCursor::default();
              loop {
                  let Some(out) = repo::mask_default_partition_batch(
                      &mut conn, t.table, t.column, action.app_id, cold_boundary, &path, cursor, limit,
                      action.id, worker_id,
                  )
                  .await?
                  else {
                      release(conn).await;
                      return Ok(true);
                  };
                  if out.status == "cancelling" {
                      cancelled = true;
                      break;
                  }
                  match out.next_cursor {
                      Some(c) => cursor = c,
                      None => break,
                  }
                  tokio::time::sleep(std::time::Duration::from_millis(cfg.inspector_mask_pause_ms)).await;
              }
              if cancelled {
                  break;
              }
          }
      }

      // --- phase 'companions': one keyset loop per non-partitioned table.
      if !cancelled {
          repo::set_mask_phase(&mut conn, action.id, worker_id, "companions", None).await?;
          for t in targets.iter().filter(|t| !t.table.is_partitioned()) {
              let path = if is_text_column(t) { Vec::new() } else { path_parts(t).0 };
              let mut after: Option<Uuid> = None;
              loop {
                  let Some(out) = repo::mask_rollup_batch(
                      &mut conn, t.table, t.column, action.app_id, &path, after, limit, action.id, worker_id,
                  )
                  .await?
                  else {
                      release(conn).await;
                      return Ok(true);
                  };
                  if out.status == "cancelling" {
                      cancelled = true;
                      break;
                  }
                  match out.next_cursor.and_then(|c| c.id) {
                      Some(id) => after = Some(id),
                      None => break,
                  }
                  tokio::time::sleep(std::time::Duration::from_millis(cfg.inspector_mask_pause_ms)).await;
              }
              if cancelled {
                  break;
              }
          }
      }

      // --- phase 'tail_sweep': close the enforcement race ONCE.
      //
      // Between "mask applied" and "every pipeline replica's policy cache
      // refreshes", new rows land unmasked and the retro-mask has already passed
      // them. Keyed on `received_at` while KEEPING an occurred_at range for
      // pruning: `occurred_at` is the CLIENT's timestamp, so a mobile offline
      // queue flushes events whose occurred_at is days old into a partition the
      // day loop already swept.
      if !cancelled {
          repo::set_mask_phase(&mut conn, action.id, worker_id, "tail_sweep", None).await?;
          let received_since = action.started_at.unwrap_or(now);
          let lo = now - Duration::days(cfg.tier_hot_days);
          let hi = now + Duration::days(1);
          for t in targets.iter().filter(|t| t.table.is_partitioned() && !is_text_column(t)) {
              let (path, wildcard) = path_parts(t);
              if wildcard || path.is_empty() {
                  continue;
              }
              let mut cursor = BatchCursor::default();
              loop {
                  let Some(out) = repo::mask_tail_sweep_batch(
                      &mut conn, t.table, t.column, action.app_id, lo, hi, received_since, &path, cursor,
                      limit, action.id, worker_id,
                  )
                  .await?
                  else {
                      release(conn).await;
                      return Ok(true);
                  };
                  match out.next_cursor {
                      Some(c) => cursor = c,
                      None => break,
                  }
                  tokio::time::sleep(std::time::Duration::from_millis(cfg.inspector_mask_pause_ms)).await;
              }
          }
      }

      // Register forward enforcement LAST, so a cancelled or failed pass does not
      // leave the pipeline masking a key the operator stopped masking at rest.
      if !cancelled {
          let rows: Vec<sauron_db::models::NewInspectorMaskedKey> = targets
              .iter()
              .filter(|t| columns::is_maskable_table(t.table.as_sql()))
              .map(|t| sauron_db::models::NewInspectorMaskedKey {
                  app_id: action.app_id,
                  target_table: t.table.as_sql(),
                  target_column: t.column.as_sql(),
                  json_path: t.path.as_str(),
                  created_by: action.requested_by,
                  source_action_id: Some(action.id),
              })
              .collect();
          repo::insert_masked_keys(&mut conn, &rows).await?;
      }

      let refreshed = repo::get_mask_action(&mut conn, action.id).await?;
      let masked = refreshed.map(|a| a.rows_masked).unwrap_or(0);
      let vacuum_advised = masked > 100_000;
      if vacuum_advised {
          warn!(
              action_id = %action.id,
              rows_masked = masked,
              "a large mask pass roughly doubled live tuples; schedule a VACUUM off-peak"
          );
      }
      let status = if cancelled { "cancelled" } else { "done" };
      repo::finish_mask_action(&mut conn, action.id, worker_id, status, vacuum_advised, Some(cold_boundary))
          .await?;
      info!(action_id = %action.id, status, rows_masked = masked, "mask action finished");
      release(conn).await;
      Ok(true)
  }

  #[allow(clippy::too_many_arguments)]
  async fn run_partitioned_batch(
      conn: &mut AsyncPgConnection,
      t: &MaskTarget,
      app_id: Uuid,
      day: NaiveDate,
      cursor: BatchCursor,
      limit: i64,
      action_id: Uuid,
      worker_id: &str,
  ) -> anyhow::Result<Option<BatchOutcome>> {
      if is_text_column(t) {
          return Ok(repo::mask_batch_text(
              conn, t.table, t.column, app_id, day, cursor, limit, action_id, worker_id,
          )
          .await?);
      }
      let (path, wildcard) = path_parts(t);
      if path.is_empty() && !wildcard {
          return Ok(None);
      }
      if wildcard {
          Ok(repo::mask_batch_jsonb_wildcard(
              conn, t.table, t.column, app_id, day, &path, cursor, limit, action_id, worker_id,
          )
          .await?)
      } else {
          Ok(repo::mask_batch_jsonb(
              conn, t.table, t.column, app_id, day, &path, cursor, limit, action_id, worker_id,
          )
          .await?)
      }
  }
  ```

- [ ] **Step 5: Add the two repo functions this calls.** Append to `backend/crates/sauron-db/src/repo.rs`:
  ```rust
  /// Whether `user_id` is active AND holds `permission` on `app_id`.
  ///
  /// Re-evaluated at claim time, in the worker's process, because confirm's
  /// authorization can be hours old by the time a queued action runs.
  /// Deliberately does NOT accept an env-scoped grant: masking is app-scoped, and
  /// `authorize_app` — which this mirrors — never resolves an env grant either.
  pub async fn user_is_active_with_app_permission(
      conn: &mut AsyncPgConnection,
      user_id: Uuid,
      app_id: Uuid,
      permission: &str,
  ) -> QueryResult<bool> {
      #[derive(QueryableByName)]
      struct OkRow {
          #[diesel(sql_type = Bool)]
          ok: bool,
      }
      let row: OkRow = diesel::sql_query(
          "SELECT EXISTS ( \
             SELECT 1 FROM role_grants g \
             JOIN roles r ON r.id = g.role_id \
             JOIN users u ON u.id = g.user_id \
             JOIN apps a ON a.id = $2 \
             JOIN projects p ON p.id = a.project_id \
             WHERE g.user_id = $1 AND u.is_active \
               AND r.permissions @> to_jsonb(ARRAY[$3::text]) \
               AND ( (g.scope_type = 'org' AND g.scope_id = p.org_id) \
                  OR (g.scope_type = 'project' AND g.scope_id = p.id) \
                  OR (g.scope_type = 'app' AND g.scope_id = a.id) ) \
           ) AS ok",
      )
      .bind::<SqlUuid, _>(user_id)
      .bind::<SqlUuid, _>(app_id)
      .bind::<Text, _>(permission)
      .get_result(conn)
      .await?;
      Ok(row.ok)
  }

  /// Fold the rows a day skipped for being at or below the tier boundary would
  /// have masked into the audit row, so a `done` action with a small
  /// `rows_masked` is explicable.
  ///
  /// ROWS, not days. The column, the CSV header, the Audit tab column and the
  /// MaskDialog all say rows, and a day count sitting in a column called
  /// `cold_rows_skipped` next to `rows_masked` is a number an operator will
  /// read as rows and act on.
  pub async fn add_cold_skip(
      conn: &mut AsyncPgConnection,
      action_id: Uuid,
      rows: i64,
  ) -> QueryResult<usize> {
      diesel::sql_query(
          "UPDATE inspector_mask_actions SET cold_rows_skipped = cold_rows_skipped + $2 WHERE id = $1",
      )
      .bind::<SqlUuid, _>(action_id)
      .bind::<BigInt, _>(rows)
      .execute(conn)
      .await
  }
  ```
  If `users` has no `is_active` column, find the real one with `cd /home/splimter/projects/freelance/sauron/backend && grep -n 'users (id)' -A 15 crates/sauron-db/src/schema.rs` and substitute it — the predicate's purpose is "the account is usable", not the column's spelling.

- [ ] **Step 6: Implement `preview.rs`.** Replace the stub body of `backend/bins/sauron-inspector/src/preview.rs` with:
  ```rust
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
      let claimed =
          repo::claim_mask_action(&mut conn, "preview", worker_id, cfg.inspector_claim_stale_secs).await?;
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
      let wm = repo::get_watermark(&mut conn, "error_events").await.unwrap_or(None);
      let cold_boundary = day_floor(now, cfg.tier_hot_days, wm, cfg.tier_tick_secs as i64);

      let mut estimated: i64 = 0;
      // ROWS, not days: `finish_preview` writes this to `cold_rows_skipped`,
      // and the dialog renders it as a row count next to `estimated_rows`.
      let mut cold_rows: i64 = 0;
      let mut day = (now - Duration::days(cfg.tier_hot_days)).date_naive();
      while day < now.date_naive() {
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
                          let path = if p.wildcard { p.sub_array() } else { p.text_array() };
                          repo::count_batch_jsonb(&mut conn, t.table, t.column, action.app_id, day, &path)
                              .await
                      }
                      Err(_) => Ok(0),
                  }
              };
              match n {
                  Ok(v) => day_total += v,
                  Err(e) => warn!(action_id = %action.id, error = %e, "preview count failed for a day"),
              }
          }
          if cold {
              cold_rows += day_total;
          } else {
              estimated += day_total;
          }
          day += Duration::days(1);
          tokio::time::sleep(std::time::Duration::from_millis(cfg.inspector_batch_pause_ms)).await;
      }

      repo::finish_preview(&mut conn, action.id, worker_id, estimated, cold_rows, Some(cold_boundary))
          .await?;
      info!(action_id = %action.id, estimated, cold_rows, "mask preview complete");
      release(conn).await;
      Ok(true)
  }
  ```

- [ ] **Step 7: Run the unit tests and see them pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-inspector-bin`. All nine tests green (five from Task 22, four here).

- [ ] **Step 8: Drive a preview then a mask against the live database.** Insert a `preview` action by hand for a real app id with `targets = '[{"table":"error_events","column":"extra","path":"customer.email"}]'::jsonb`, run the worker for 60 s, and confirm `status='previewed'` with `estimated_rows` populated. Then `UPDATE inspector_mask_actions SET kind='mask', status='pending', worker_id=NULL, claimed_at=NULL`, run again, and check `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "SELECT status, phase, rows_scanned, rows_masked, cold_rows_skipped, vacuum_advised FROM inspector_mask_actions ORDER BY requested_at DESC LIMIT 1"`.

- [ ] **Step 9: Prove crash resume does not double-count.** Note `rows_masked`, kill the worker mid-pass, run `UPDATE inspector_mask_actions SET claimed_at = now() - interval '1 hour' WHERE id = '<id>'`, restart, and assert the final `rows_masked` equals `SELECT count(*) FROM error_events WHERE app_id='<app>' AND extra #> '{customer,email}' = '"****"'::jsonb` with no gap and no double-count.

- [ ] **Step 10: Prove cancel lands terminal with a durable cursor.** Start a mask, then `UPDATE inspector_mask_actions SET status='cancelling' WHERE id='<id>'`, and confirm the worker lands `status='cancelled'` with `cursor_occurred_at` and `rows_masked` non-zero. Re-queue it (`status='pending'`) and confirm it completes.

- [ ] **Step 11: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 24: `reap.rs` — retention on its own cadence

**Files:**
- Modify `backend/bins/sauron-inspector/src/reap.rs`

**Interfaces:**
- Consumes: `repo::{prune_inspector_scans, prune_inspector_findings, prune_mask_previews, prune_mask_actions, pseudonymize_mask_actions}` (Task 20).
- Produces: `reap::tick`.

> A queue's reaper runs in the process that drains it. The inspector tables are drained here, so they are reaped here — never in `sauron-api`, and never inside a scan tick, where a multi-hour scan would mean retention silently stops for that whole time.

- [ ] **Step 1: Implement.** Replace the stub body of `backend/bins/sauron-inspector/src/reap.rs` with:
  ```rust
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

      match repo::prune_inspector_findings(&mut conn, cfg.inspector_finding_retention_days, DELETE_BATCH)
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
      match repo::prune_mask_actions(&mut conn, cfg.inspector_audit_retention_days, DELETE_BATCH).await {
          Ok(n) if n > 0 => info!(pruned = n, "pruned terminal mask actions"),
          Ok(_) => {}
          Err(e) => warn!(error = %e, "pruning mask actions failed"),
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
  ```

- [ ] **Step 2: Build.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Clean.

- [ ] **Step 3: Verify it runs on the first tick.** `cd /home/splimter/projects/freelance/sauron/backend && INSPECTOR_ENABLED=1 INSPECTOR_AUDIT_PII_DAYS=0 DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu timeout 20 cargo run --bin sauron-inspector 2>&1 | grep -i 'pseudonym\|prune'`. Expected: a `pseudonymized old mask audit rows` line if any audit row exists (at a 0-day threshold every row qualifies) and no error lines. Restore the default afterwards.

- [ ] **Step 4: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 25: Forward enforcement in `sauron-pipeline`

**Files:**
- Create `backend/crates/sauron-pipeline/src/mask.rs`
- Modify `backend/crates/sauron-pipeline/src/lib.rs`
- Modify `backend/crates/sauron-pipeline/src/worker.rs` (`spawn_workers` ~line 31, `worker_loop` ~line 56, `process_entries` ~line 148)
- Modify `backend/crates/sauron-pipeline/src/process.rs` (`process_job` ~lines 18–50)
- Modify `backend/bins/sauron-ingest/src/main.rs` (the `spawn_workers` call site)
- Modify `backend/crates/sauron-pipeline/Cargo.toml`

**Interfaces:**
- Consumes: `repo::masked_keys_for_app` (Task 18), `sauron_inspector::mask::{apply_wire_path, MASK_SENTINEL}` (Task 14).
- Produces: `sauron_pipeline::mask::{MaskSet, PolicyCache, apply_wire, apply_context}`; `spawn_workers(pool, redis, concurrency, sym, policies: Arc<PolicyCache>)`; `process_job(pool, redis, sym, masks, job)`.

> Enforcement lives here, **not at the ingest edge**. Three reasons in order: this is the only point that sees the server-derived enriched context (the `woothee` `ua` block and `device_key`) which `error_events.context` and `sessions.context` are both written from; it is off the HTTP handler (note the pipeline workers run as tokio tasks *inside* the `sauron-ingest` binary via `spawn_workers`, so "off the request path" means "off the handler", not "a different process"); and it needs no policy read at DSN resolution, which is what lets `EnvRef` stay untouched. Adding policy fields to `EnvRef` would require bumping the `sauron:dsn:v2:` cache prefix to `v3` — the `dsn_cache` doc comment says the version segment is load-bearing precisely because entries written by the previous binary would otherwise deserialize into the wrong struct for the full 300 s TTL after every deploy — and a policy edit would take up to 300 s to reach every ingest replica unless the API fanned out cache deletions over `repo::live_app_environment_keys`.

- [ ] **Step 1: Write the failing tests.** Create `backend/crates/sauron-pipeline/src/mask.rs` with only:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use serde_json::json;

      fn set(rows: &[(&str, &str, &str)]) -> MaskSet {
          MaskSet::from_rows(
              rows.iter()
                  .map(|(t, c, p)| (t.to_string(), c.to_string(), p.to_string()))
                  .collect(),
          )
      }

      #[test]
      fn masks_a_nested_path_in_a_jsonb_value() {
          let s = set(&[("error_events", "extra", "customer.email")]);
          let mut v = json!({"customer": {"email": "jane@acme.com", "keep": 1}});
          mask_json(&s, "error_events", "extra", &mut v);
          assert_eq!(v, json!({"customer": {"email": "****", "keep": 1}}));
      }

      #[test]
      fn a_row_for_another_column_does_nothing() {
          let s = set(&[("error_events", "tags", "email")]);
          let before = json!({"email": "a@b.c"});
          let mut v = before.clone();
          mask_json(&s, "error_events", "extra", &mut v);
          assert_eq!(v, before);
      }

      /// An empty `json_path` means the WHOLE column, which only makes sense for
      /// a TEXT column — the caller checks `masks_whole`, and the jsonb applier
      /// must skip it rather than collapsing the entire document.
      #[test]
      fn an_empty_path_never_collapses_a_jsonb_column() {
          let s = set(&[("error_events", "extra", "")]);
          let before = json!({"a": 1});
          let mut v = before.clone();
          mask_json(&s, "error_events", "extra", &mut v);
          assert_eq!(v, before);
          assert!(s.masks_whole("error_events", "extra"));
      }

      /// `apply_context` only ever touches targets whose column is `context` —
      /// the ENRICHED surface the ingest edge physically cannot see.
      #[test]
      fn apply_context_only_touches_context_targets() {
          let s = set(&[
              ("error_events", "context", "user.email"),
              ("error_events", "extra", "customer.email"),
          ]);
          let mut ctx = json!({"user": {"email": "a@b.c", "id": "u1"}, "ua": {"browser": "x"}});
          apply_context(&s, &mut ctx);
          assert_eq!(ctx["user"]["email"], json!("****"));
          assert_eq!(ctx["user"]["id"], json!("u1"));
          assert_eq!(ctx["ua"]["browser"], json!("x"));
      }

      #[test]
      fn an_empty_set_is_a_no_op() {
          let s = MaskSet::default();
          let before = json!({"user": {"email": "a@b.c"}});
          let mut ctx = before.clone();
          apply_context(&s, &mut ctx);
          assert_eq!(ctx, before);
          assert!(s.is_empty());
      }

      /// `issues.title` is masked at rest and by the sticky guard in
      /// `upsert_issue`, never on the wire — a row for it must be ignored here
      /// rather than panicking.
      #[test]
      fn rows_for_tables_the_wire_does_not_carry_are_ignored() {
          let s = set(&[("issues", "title", "")]);
          assert!(s.paths("error_events", "extra").is_empty());
          assert!(!s.masks_whole("error_events", "message"));
      }
  }
  ```

- [ ] **Step 2: Wire the module and the dependency.** Add `pub mod mask;` to `backend/crates/sauron-pipeline/src/lib.rs` and `sauron-inspector = { workspace = true }` to `backend/crates/sauron-pipeline/Cargo.toml`.

- [ ] **Step 3: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-pipeline mask`. Expected: `error[E0433]: failed to resolve: use of undeclared type 'MaskSet'`.

- [ ] **Step 4: Implement the mask set and the two appliers.** Prepend to `backend/crates/sauron-pipeline/src/mask.rs`:
  ```rust
  //! Forward enforcement: mask developer-supplied values on their way in.
  //!
  //! Two application sites, ONE policy lookup per job. `apply_wire` runs
  //! immediately after `serde_json::from_str::<IngestJob>` succeeds, on the
  //! owned wire payload; `apply_context` runs inside `process_job` right after
  //! `enrich_context`, and touches ONLY targets whose column is `context` —
  //! that is the enriched-only surface (the `woothee` `ua` block and
  //! `device_key`), which the ingest edge physically cannot see.
  //!
  //! What this does NOT reach is named in the wiki and in the mask dialog: the
  //! raw value still lives in `sauron:ingest:stream` for the `MAXLEN ~1e6`
  //! window, and a payload that fails to DESERIALIZE dead-letters raw.

  use std::collections::HashMap;
  use std::sync::{Arc, RwLock};
  use std::time::{Duration, Instant};

  use sauron_core::envelope::{EnvelopeItem, IngestJob};
  use sauron_db::PgPool;
  use sauron_inspector::mask::{apply_wire_path, MASK_SENTINEL};
  use serde_json::Value;
  use tracing::warn;
  use uuid::Uuid;

  /// The masked-key rows for one app, grouped for O(1) lookup per column.
  #[derive(Debug, Default, Clone)]
  pub struct MaskSet {
      by_column: HashMap<(String, String), Vec<String>>,
  }

  impl MaskSet {
      pub fn from_rows(rows: Vec<(String, String, String)>) -> MaskSet {
          let mut by_column: HashMap<(String, String), Vec<String>> = HashMap::new();
          for (table, column, path) in rows {
              by_column.entry((table, column)).or_default().push(path);
          }
          MaskSet { by_column }
      }

      pub fn is_empty(&self) -> bool {
          self.by_column.is_empty()
      }

      pub fn paths(&self, table: &str, column: &str) -> &[String] {
          self.by_column
              .get(&(table.to_string(), column.to_string()))
              .map(|v| v.as_slice())
              .unwrap_or(&[])
      }

      /// An empty `json_path` means the whole column, which only ever applies to
      /// a TEXT column.
      pub fn masks_whole(&self, table: &str, column: &str) -> bool {
          self.paths(table, column).iter().any(|p| p.is_empty())
      }
  }

  fn mask_json(set: &MaskSet, table: &str, column: &str, v: &mut Value) {
      for path in set.paths(table, column) {
          if path.is_empty() {
              // Never collapse a whole jsonb document: `masks_whole` is for TEXT.
              continue;
          }
          apply_wire_path(v, path);
      }
  }

  /// Mask the owned wire payload in place.
  ///
  /// Every field touched here is `pub` and owned on the envelope types.
  pub fn apply_wire(set: &MaskSet, job: &mut IngestJob) {
      if set.is_empty() {
          return;
      }
      match &mut job.item {
          EnvelopeItem::Error(e) => {
              mask_json(set, "error_events", "tags", &mut e.tags);
              mask_json(set, "error_events", "contexts", &mut e.contexts);
              mask_json(set, "error_events", "extra", &mut e.extra);
              if !set.paths("error_events", "breadcrumbs").is_empty() {
                  // `breadcrumbs` is a typed Vec on the wire but a jsonb column
                  // at rest, so it round-trips through Value to reuse ONE path
                  // applier rather than forking a second one that would drift.
                  let mut v = serde_json::to_value(&e.breadcrumbs).unwrap_or(Value::Null);
                  mask_json(set, "error_events", "breadcrumbs", &mut v);
                  if let Ok(back) = serde_json::from_value(v) {
                      e.breadcrumbs = back;
                  }
              }
              // `error_events.title`/`culprit` are derived server-side by
              // `build_title`/`build_culprit` and have NO wire field, so the
              // only way forward enforcement reaches them is by masking the
              // INPUTS. That is what `expand_targets` produces.
              if set.masks_whole("error_events", "message") && e.message.is_some() {
                  e.message = Some(MASK_SENTINEL.to_string());
              }
              if let Some(exc) = e.exception.as_mut() {
                  if set.masks_whole("error_events", "exception_value") {
                      exc.value = MASK_SENTINEL.to_string();
                  }
                  if set.masks_whole("error_events", "exception_type") {
                      exc.type_ = MASK_SENTINEL.to_string();
                  }
              }
              if !set.paths("error_events", "event_user").is_empty() {
                  if let Some(user) = e.user.as_mut() {
                      let mut v = serde_json::to_value(&*user).unwrap_or(Value::Null);
                      mask_json(set, "error_events", "event_user", &mut v);
                      if let Ok(back) = serde_json::from_value(v) {
                          *user = back;
                      }
                  }
              }
          }
          EnvelopeItem::Event(ev) => {
              mask_json(set, "analytics_events", "properties", &mut ev.properties);
              mask_json(set, "analytics_events", "tags", &mut ev.tags);
              mask_json(set, "analytics_events", "contexts", &mut ev.contexts);
              mask_json(set, "analytics_events", "extra", &mut ev.extra);
          }
          EnvelopeItem::Identify(id) => {
              // Reachable through forward enforcement ONLY: `upsert_event_user`
              // merges with `||`, which never removes keys, so an at-rest mask is
              // undone by the next identify(). The UI says so.
              mask_json(set, "event_users", "properties", &mut id.traits);
          }
          EnvelopeItem::Transaction(t) => {
              if set.masks_whole("transactions", "url") {
                  t.url = MASK_SENTINEL.to_string();
              }
          }
          // Breadcrumb batches carry no maskable column of their own; they are
          // folded into `error_events.breadcrumbs` when an error arrives.
          EnvelopeItem::BreadcrumbBatch(_) => {}
      }
  }

  /// Mask ONLY the enriched context. Called after `enrich_context`.
  pub fn apply_context(set: &MaskSet, context: &mut Value) {
      if set.is_empty() {
          return;
      }
      for table in ["error_events", "analytics_events"] {
          for path in set.paths(table, "context") {
              if path.is_empty() {
                  continue;
              }
              apply_wire_path(context, path);
          }
      }
  }
  ```
  If `TransactionItem` has no `url` field, or `EventUser` does not round-trip through `Value`, read the real shapes with `cd /home/splimter/projects/freelance/sauron/backend && grep -n 'pub struct TransactionItem' -A 20 crates/sauron-core/src/envelope.rs` and adjust those two arms; every other arm is unaffected.

- [ ] **Step 5: Implement the cache.** Append to `backend/crates/sauron-pipeline/src/mask.rs`, above the test module:
  ```rust
  /// Per-app masked-key cache with a short TTL, negative-cached.
  ///
  /// A mask takes effect on every pipeline replica within about
  /// `INSPECTOR_POLICY_CACHE_SECS`; the API returns that number so the UI can
  /// state it literally rather than hardcoding "30 seconds".
  ///
  /// FAILS STALE, NOT OPEN. Serving an empty set on error is tempting — failing
  /// closed would drop telemetry — but the trigger set is much wider than the
  /// RPM-upgrade case: a pool checkout timeout, a statement timeout, a failover
  /// or a rolled-back migration would all silently disable masking
  /// deployment-wide with only a `warn!`. Because the retro-mask is a one-shot
  /// job that ends at `done`, every row written during that window stays raw
  /// FOREVER. A five-minute Postgres blip must not permanently defeat an
  /// irreversible redaction the operator was told had converged.
  pub struct PolicyCache {
      pool: PgPool,
      ttl: Duration,
      inner: RwLock<HashMap<Uuid, Entry>>,
  }

  struct Entry {
      set: Arc<MaskSet>,
      loaded_at: Instant,
      /// Set when the last refresh FAILED. The warn is rate-limited to once per
      /// app per TTL — without that, an upgrade where migrations have not been
      /// re-run means one failing query and one log line PER INGESTED EVENT,
      /// doubling DB round-trips on the same 8 connections that accept traffic
      /// and flooding journald at ingest rate.
      last_error_at: Option<Instant>,
  }

  impl PolicyCache {
      pub fn new(pool: PgPool, ttl_secs: u64) -> PolicyCache {
          PolicyCache {
              pool,
              ttl: Duration::from_secs(ttl_secs.max(1)),
              inner: RwLock::new(HashMap::new()),
          }
      }

      pub async fn get(&self, app_id: Uuid) -> Arc<MaskSet> {
          if let Some(hit) = self.fresh(app_id) {
              return hit;
          }
          match self.load(app_id).await {
              Ok(set) => {
                  let set = Arc::new(set);
                  if let Ok(mut w) = self.inner.write() {
                      w.insert(
                          app_id,
                          Entry { set: set.clone(), loaded_at: Instant::now(), last_error_at: None },
                      );
                  }
                  set
              }
              Err(e) => self.serve_stale(app_id, e),
          }
      }

      fn fresh(&self, app_id: Uuid) -> Option<Arc<MaskSet>> {
          let r = self.inner.read().ok()?;
          let entry = r.get(&app_id)?;
          (entry.loaded_at.elapsed() < self.ttl).then(|| entry.set.clone())
      }

      async fn load(&self, app_id: Uuid) -> anyhow::Result<MaskSet> {
          // Never hold this across the rest of the job: the ingest pool is 8 for
          // the whole process and the workers share it with every insert.
          let mut conn = sauron_db::conn(&self.pool).await?;
          let rows = sauron_db::repo::masked_keys_for_app(&mut conn, app_id).await?;
          drop(conn);
          Ok(MaskSet::from_rows(
              rows.into_iter()
                  .map(|r| (r.target_table, r.target_column, r.json_path))
                  .collect(),
          ))
      }

      fn serve_stale(&self, app_id: Uuid, err: anyhow::Error) -> Arc<MaskSet> {
          let mut should_warn = true;
          let mut stale = None;
          if let Ok(mut w) = self.inner.write() {
              if let Some(entry) = w.get_mut(&app_id) {
                  should_warn = entry.last_error_at.map(|t| t.elapsed() >= self.ttl).unwrap_or(true);
                  entry.last_error_at = Some(Instant::now());
                  // Push `loaded_at` forward so the next event does not retry
                  // immediately; the set is served stale for one more TTL.
                  entry.loaded_at = Instant::now();
                  stale = Some(entry.set.clone());
              }
          }
          if should_warn {
              warn!(
                  app_id = %app_id,
                  error = %err,
                  serving_stale = stale.is_some(),
                  "could not load masked keys; forward masking is degraded \
                   (run `systemctl start sauron-migrate` after an upgrade)"
              );
          }
          // Only when NO successful load has ever happened for this app does the
          // enforcer fall back to an empty set.
          stale.unwrap_or_else(|| Arc::new(MaskSet::default()))
      }
  }
  ```

- [ ] **Step 6: Run and see the tests pass.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-pipeline mask`. All six tests green.

- [ ] **Step 7: Thread the cache through the worker.** In `backend/crates/sauron-pipeline/src/worker.rs`: add `use std::sync::Arc;` and `use crate::mask::PolicyCache;`; add a `policies: Arc<PolicyCache>` parameter to `spawn_workers` after `sym`, clone it per worker exactly as `sym` is cloned, and add it to `worker_loop`'s and `process_entries`' parameter lists (threading `&policies` through the two `process_entries` call sites in `worker_loop`).

- [ ] **Step 8: Apply the wire mask and the masked dead-letter.** In `process_entries`, replace the whole `match serde_json::from_str::<IngestJob>(&payload)` body with:
  ```rust
  match serde_json::from_str::<IngestJob>(&payload) {
      Ok(mut job) => {
          // Resolve the app's mask set ONCE per job, then mask the owned wire
          // payload before anything is persisted or re-queued.
          let set = policies.get(job.app_id).await;
          crate::mask::apply_wire(&set, &mut job);
          // Capture the MASKED payload now: at the call site below `job` has
          // already been moved into `process_job(...)` before the Err arm runs,
          // and `process_entries` returns () so `?` is not usable here.
          let masked_payload = serde_json::to_string(&job).unwrap_or_else(|_| payload.clone());
          match process_job(pool, redis, sym, &set, job).await {
              Ok(()) => {
                  let _ = redis.ack(&id).await;
              }
              Err(e) => {
                  warn!(consumer, id, error = %e, "job processing failed; dead-lettering");
                  // Dead-letter the MASKED job. `sauron:ingest:dlq` is XADD with
                  // no MAXLEN and no TTL and no reaper exists, so a raw
                  // dead-letter is permanent.
                  let _ = redis.dead_letter(&id, &masked_payload).await;
              }
          }
      }
      Err(e) => {
          warn!(consumer, id, error = %e, "malformed job; dead-lettering");
          // A payload that fails to DESERIALIZE still dead-letters raw — a
          // small, permanent, named hole. §1 of the design lists it.
          let _ = redis.dead_letter(&id, &payload).await;
      }
  }
  ```

- [ ] **Step 9: Apply the enriched-context mask.** In `backend/crates/sauron-pipeline/src/process.rs`, change `process_job`'s signature to take `masks: &crate::mask::MaskSet` between `sym` and `job`, and replace `let context = enrich_context(&job);` with:
  ```rust
  let mut context = enrich_context(&job);
  // The enriched-only surface. `error_events.context` and `sessions.context`
  // are both written from this value, and the ingest edge physically cannot
  // see it — the `woothee` ua block and `device_key` are derived right here.
  crate::mask::apply_context(masks, &mut context);
  ```
  Then update `lib.rs`'s `pub use process::process_job;` re-export (no change needed) and any other `process_job` call site the compiler names.

- [ ] **Step 10: Build the cache in `sauron-ingest`.** In `backend/bins/sauron-ingest/src/main.rs`, before the `spawn_workers` call, add:
  ```rust
  // One cache per process, shared by every worker task. `sauron-ingest` never
  // reads `inspector.env`, which is why INSPECTOR_POLICY_CACHE_SECS lives in
  // `sauron.env` — the "about 30 seconds" the API reports to the UI would
  // otherwise silently diverge from what the enforcer actually uses.
  let policies = std::sync::Arc::new(sauron_pipeline::mask::PolicyCache::new(
      pool.clone(),
      cfg.inspector_policy_cache_secs,
  ));
  ```
  and pass `policies` as the new fifth argument to `spawn_workers`.

- [ ] **Step 11: Build and test.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets` then `... cargo test -p sauron-pipeline`. Both clean.

- [ ] **Step 12: Verify enforcement end to end.** With `sauron-ingest` running against the live database, insert a masked-key row (`INSERT INTO inspector_masked_keys (app_id, target_table, target_column, json_path) VALUES ('<app>', 'error_events', 'extra', 'customer.email')`), wait 30 s, send one error event carrying `extra.customer.email`, then `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "SELECT extra->'customer'->>'email' FROM error_events ORDER BY received_at DESC LIMIT 1"`. Expected `****`. Repeat with `('error_events','context','user.email')` and confirm both `error_events.context` **and** `sessions.context` land masked on the same event.

- [ ] **Step 13: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 26: `routes/inspector.rs` — policies, effective policy, and scans

**Files:**
- Create `backend/bins/sauron-api/src/routes/inspector.rs`
- Modify `backend/bins/sauron-api/src/routes/mod.rs` (add `pub mod inspector;`)

**Interfaces:**
- Consumes: `perm::{PII_READ, PII_MANAGE}` (Task 1), `repo::{create_inspector_policy, get_inspector_policy, list_inspector_policies_for_org, patch_inspector_policy, delete_inspector_policy, reschedule_policy, validate_scope_in_org, timezone_is_valid, effective_policy_for_app, active_scan_for_policy, get_inspector_scan, list_scans_for_policy, request_scan_cancel}` (Tasks 15–16), `repo::{EnqueueOutcome, enqueue_scan_for_policy}` (Task 22), `sauron_inspector::{matching, detect}` (Tasks 7–8), `routes::scope::reject_environment_id_with_message`, `routes::db`.
- Produces: handlers `list_policies`, `create_policy`, `get_policy`, `patch_policy`, `delete_policy`, `effective_policy`, `list_scans`, `start_scan`, `get_scan`, `cancel_scan`; the module constant `ENV_SCOPE_MESSAGE`; helper `policy_ancestry`; `repo::app_id_for_enrollment` (added here, in Step 1).

> **Task 22 must land before this one**, because `start_scan` calls `repo::enqueue_scan_for_policy` rather than re-deriving the scan's `params`, `targets` and `units_total`. That is the whole point: a second copy of the freeze logic in a handler is how a manual scan comes to walk environments a narrower disabled policy excluded — the exact failure design §4's "target resolution must actually subtract" exists to prevent.

- [ ] **Step 1: Create the module with its header and the shared helpers.** Create `backend/bins/sauron-api/src/routes/inspector.rs`:
  ```rust
  //! PII inspector: policies, scans, findings, reveal, masking and the audit
  //! trail. Gated on `pii:read` / `pii:manage`, which Owner and Admin hold and
  //! Developer and Viewer deliberately do not — `pii:read` is bulk PII
  //! disclosure and `pii:manage` is irreversible bulk destruction, and neither
  //! should be inherited by the role every engineer gets by default.
  //!
  //! There is no `authorize_env` in this product and this module does not invent
  //! one: `require_permission`/`effective_at` have no env parameter and always
  //! resolve with `env: None`, so an env-scoped grant can never satisfy them. An
  //! `app_env`-scoped POLICY is therefore authorized at its PARENT APP — a
  //! member holding `pii:manage` on one environment only cannot edit that
  //! environment's policy. Same documented gap `orgs::delete_grant` carries.

  use axum::extract::{Path, Query, State};
  use axum::http::StatusCode;
  use axum::Json;
  use serde::Deserialize;
  use serde_json::{json, Value};
  use uuid::Uuid;

  use sauron_auth::{
      authorize_app, authorize_org, authorize_project, grants_from_rows, perm, reach_for, AuthUser,
  };
  // No `NewInspectorScan` here on purpose: scans are only ever created through
  // `repo::enqueue_scan_for_policy`, so the API cannot freeze a scan the
  // scheduler would have frozen differently.
  use sauron_db::models::{InspectorPolicyPatch, NewInspectorPolicy};
  use sauron_db::repo;
  use sauron_inspector::{detect, matching};

  use super::db;
  use crate::error::ApiError;
  use crate::AppState;

  /// The one message every `/v1/apps/{app_id}/inspector/*` route rejects
  /// `environment_id` with. Findings carry their own environment dimension in
  /// the payload and masking is app-scoped, so one consistent rule beats one
  /// exception.
  pub(crate) const ENV_SCOPE_MESSAGE: &str =
      "the inspector is app-scoped; masking cannot be limited to one environment";

  /// Ceiling on any list endpoint here. Findings and audit rows are both
  /// unbounded in principle.
  const MAX_LIMIT: i64 = 500;

  fn clamp_limit(raw: Option<i64>) -> i64 {
      raw.unwrap_or(100).clamp(1, MAX_LIMIT)
  }

  /// Resolve a policy row to the scope its permission is checked at.
  ///
  /// `app_env` authorizes at the PARENT APP (see the module header). The
  /// enrollment id is resolved to its app rather than refused, so an
  /// environment-scoped policy is still manageable by whoever manages the app.
  async fn authorize_policy(
      conn: &mut sauron_db::AsyncPgConnection,
      user_id: Uuid,
      target_type: &str,
      target_id: Uuid,
      permission: &str,
  ) -> Result<(), ApiError> {
      match target_type {
          "project" => {
              authorize_project(conn, user_id, target_id, permission).await?;
              Ok(())
          }
          // authorize_app, NEVER authorize_app_reachable: the latter is
          // read-only by explicit contract, and an env-scoped grant must not see
          // app-wide findings.
          "app" => {
              authorize_app(conn, user_id, target_id, permission).await?;
              Ok(())
          }
          "app_env" => {
              let app_id = repo::app_id_for_enrollment(conn, target_id)
                  .await
                  .map_err(|e| ApiError::Internal(e.to_string()))?
                  .ok_or(ApiError::NotFound)?;
              authorize_app(conn, user_id, app_id, permission).await?;
              Ok(())
          }
          _ => Err(ApiError::BadRequest("unknown policy target type".into())),
      }
  }
  ```
  Add the resolver to `repo.rs`:
  ```rust
  /// The app an `app_environments` ENROLLMENT belongs to.
  pub async fn app_id_for_enrollment(
      conn: &mut AsyncPgConnection,
      enrollment_id: Uuid,
  ) -> QueryResult<Option<Uuid>> {
      app_environments::table
          .find(enrollment_id)
          .select(app_environments::app_id)
          .first(conn)
          .await
          .optional()
  }
  ```

- [ ] **Step 2: Register the module and build.** Add `pub mod inspector;` to `backend/bins/sauron-api/src/routes/mod.rs` in alphabetical position (after `pub mod funnels;`). Then `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Expected: warnings about unused imports only.

- [ ] **Step 3: Implement policy create with all of its validation.** Append to `inspector.rs`:
  ```rust
  #[derive(Deserialize)]
  pub struct CreatePolicyReq {
      pub target_type: String,
      pub target_id: Uuid,
      #[serde(default)]
      pub tracked_keys: Value,
      #[serde(default)]
      pub detectors: Value,
      #[serde(default)]
      pub scan_columns: Option<Value>,
      #[serde(default)]
      pub rollups: Option<Value>,
      #[serde(default)]
      pub window_days: Option<i32>,
      #[serde(default)]
      pub schedule_enabled: Option<bool>,
      #[serde(default)]
      pub schedule_days: Option<i16>,
      /// `HH:MM` local wall clock.
      #[serde(default)]
      pub schedule_time: Option<String>,
      #[serde(default)]
      pub schedule_tz: Option<String>,
  }

  /// Normalize and validate the two matcher fields together.
  ///
  /// A policy with NEITHER tracked keys NOR detectors is rejected with 400.
  /// Without that, the single most likely first configuration — "I don't know my
  /// payload shape, turn on the email detector" — combined with the prefilter
  /// being built only from the key list produces a scan that reads zero rows and
  /// finishes `succeeded`, `coverage='full'`, zero findings. A confident false
  /// negative on a privacy scan is the worst thing this feature can emit.
  fn normalize_matchers(keys_in: &Value, dets_in: &Value) -> Result<(Value, Value), ApiError> {
      let keys = matching::parse_tracked_keys(keys_in);
      let dets = detect::parse_detectors(dets_in);
      if keys.is_empty() && dets.is_empty() {
          return Err(ApiError::BadRequest(
              "a policy needs at least one tracked key or one detector; \
               a policy with neither scans nothing and reports a false negative"
                  .into(),
          ));
      }
      // Keys are lowercased at write so the stored row and the matcher agree.
      let keys_json = serde_json::to_value(&keys).map_err(|e| ApiError::Internal(e.to_string()))?;
      let dets_json = Value::Array(dets.iter().map(|d| json!(d.id())).collect());
      Ok((keys_json, dets_json))
  }

  fn parse_hhmm(raw: &str) -> Result<chrono::NaiveTime, ApiError> {
      chrono::NaiveTime::parse_from_str(raw, "%H:%M")
          .or_else(|_| chrono::NaiveTime::parse_from_str(raw, "%H:%M:%S"))
          .map_err(|_| ApiError::BadRequest("schedule_time must be HH:MM".into()))
  }

  pub async fn create_policy(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(org_id): Path<Uuid>,
      Query(env): Query<super::scope::RejectEnvQuery>,
      Json(req): Json<CreatePolicyReq>,
  ) -> Result<Json<Value>, ApiError> {
      super::scope::reject_environment_id(env.environment_id.as_deref())?;
      let mut conn = db(&state).await?;
      authorize_org(&mut conn, auth.user_id, org_id, perm::PII_MANAGE).await?;

      if !matches!(req.target_type.as_str(), "project" | "app" | "app_env") {
          return Err(ApiError::BadRequest(
              "target_type must be project, app or app_env".into(),
          ));
      }
      // `target_id` has NO foreign key, so without this any authenticated user
      // can mint an org where they hold org:manage (POST /v1/orgs requires only
      // AuthUser), POST a policy naming a victim's app_id, and have the worker
      // scan the victim's error_events into rows carrying the ATTACKER's org_id
      // — which is exactly what every list query filters on. 404, not 403, so it
      // is not an existence oracle.
      if !repo::validate_scope_in_org(&mut conn, org_id, &req.target_type, req.target_id).await? {
          return Err(ApiError::NotFound);
      }

      let (keys, dets) = normalize_matchers(&req.tracked_keys, &req.detectors)?;
      let tz = req.schedule_tz.unwrap_or_else(|| "UTC".to_string());
      if !repo::timezone_is_valid(&mut conn, &tz).await {
          return Err(ApiError::BadRequest(format!("unknown timezone {tz:?}")));
      }
      let time = match req.schedule_time.as_deref() {
          Some(s) => parse_hhmm(s)?,
          None => chrono::NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
      };
      let days = req.schedule_days.unwrap_or(0);
      if !(0..=127).contains(&days) {
          return Err(ApiError::BadRequest("schedule_days is a 0..127 weekday bitmask".into()));
      }
      let window_days = req.window_days.unwrap_or(30);
      if !(1..=400).contains(&window_days) {
          return Err(ApiError::BadRequest("window_days must be between 1 and 400".into()));
      }
      let rollups = req.rollups.unwrap_or_else(|| json!(["issues", "event_users"]));

      let policy = repo::create_inspector_policy(
          &mut conn,
          NewInspectorPolicy {
              org_id,
              target_type: &req.target_type,
              target_id: req.target_id,
              enabled: true,
              tracked_keys: &keys,
              detectors: &dets,
              scan_columns: req.scan_columns.as_ref(),
              rollups: &rollups,
              window_days,
              schedule_enabled: req.schedule_enabled.unwrap_or(false),
              schedule_days: days,
              schedule_time: time,
              schedule_tz: &tz,
              created_by: Some(auth.user_id),
          },
      )
      .await
      .map_err(|e| match e {
          diesel::result::Error::DatabaseError(
              diesel::result::DatabaseErrorKind::UniqueViolation,
              _,
          ) => ApiError::Conflict("a policy already exists for this target".into()),
          other => ApiError::Internal(other.to_string()),
      })?;

      // Called after EVERY schedule-field write so `next_run_at` is never stale.
      repo::reschedule_policy(&mut conn, policy.id).await?;
      let fresh = repo::get_inspector_policy(&mut conn, policy.id).await?;
      Ok(Json(json!(fresh)))
  }
  ```

- [ ] **Step 4: Implement list, get, patch, delete and the effective-policy read.** Append to `inspector.rs`:
  ```rust
  /// Org-level policy LIST. Deliberately NOT `authorize_org`.
  ///
  /// A fixed-scope check can never be satisfied by a narrower grant — the
  /// historical 403-for-scoped-members bug. This is the house discovery pattern:
  /// load the caller's grants, 403 on empty, compute their reach for `pii:read`,
  /// and filter, lifting env grants to their app.
  pub async fn list_policies(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(org_id): Path<Uuid>,
      Query(env): Query<super::scope::RejectEnvQuery>,
  ) -> Result<Json<Value>, ApiError> {
      super::scope::reject_environment_id(env.environment_id.as_deref())?;
      let mut conn = db(&state).await?;
      let rows = repo::user_grants_in_org(&mut conn, auth.user_id, org_id).await?;
      if rows.is_empty() {
          return Err(ApiError::Forbidden("no grants in this organization".into()));
      }
      let grants = grants_from_rows(rows);
      let reach = reach_for(&grants, perm::PII_READ);
      if !reach.org && reach.projects.is_empty() && reach.apps.is_empty() && reach.envs.is_empty() {
          return Err(ApiError::Forbidden("pii:read is required".into()));
      }
      // Lift env grants to their app: an env grant cannot satisfy authorize_app,
      // but its holder should still SEE the app's policy list.
      let env_apps = repo::env_ancestries(&mut conn, &reach.envs).await?;
      let all = repo::list_inspector_policies_for_org(&mut conn, org_id).await?;
      let visible: Vec<_> = all
          .into_iter()
          .filter(|p| {
              reach.org
                  || match p.target_type.as_str() {
                      "project" => reach.projects.contains(&p.target_id),
                      "app" => reach.apps.contains(&p.target_id),
                      "app_env" => {
                          reach.envs.contains(&p.target_id)
                              || env_apps.iter().any(|(env_id, app_id, _)| {
                                  *env_id == p.target_id && reach.apps.contains(app_id)
                              })
                      }
                      _ => false,
                  }
          })
          .collect();
      Ok(Json(json!(visible)))
  }

  pub async fn get_policy(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(id): Path<Uuid>,
  ) -> Result<Json<Value>, ApiError> {
      let mut conn = db(&state).await?;
      let p = repo::get_inspector_policy(&mut conn, id).await?.ok_or(ApiError::NotFound)?;
      authorize_policy(&mut conn, auth.user_id, &p.target_type, p.target_id, perm::PII_READ).await?;
      Ok(Json(json!(p)))
  }

  #[derive(Deserialize)]
  pub struct PatchPolicyReq {
      #[serde(default)]
      pub enabled: Option<bool>,
      #[serde(default)]
      pub tracked_keys: Option<Value>,
      #[serde(default)]
      pub detectors: Option<Value>,
      #[serde(default)]
      pub scan_columns: Option<Value>,
      #[serde(default)]
      pub rollups: Option<Value>,
      #[serde(default)]
      pub window_days: Option<i32>,
      #[serde(default)]
      pub schedule_enabled: Option<bool>,
      #[serde(default)]
      pub schedule_days: Option<i16>,
      #[serde(default)]
      pub schedule_time: Option<String>,
      #[serde(default)]
      pub schedule_tz: Option<String>,
  }

  pub async fn patch_policy(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(id): Path<Uuid>,
      Json(req): Json<PatchPolicyReq>,
  ) -> Result<Json<Value>, ApiError> {
      let mut conn = db(&state).await?;
      let existing = repo::get_inspector_policy(&mut conn, id).await?.ok_or(ApiError::NotFound)?;
      authorize_policy(
          &mut conn,
          auth.user_id,
          &existing.target_type,
          existing.target_id,
          perm::PII_MANAGE,
      )
      .await?;
      // Re-validated on every PATCH as well as create: grants outlive targets.
      if !repo::validate_scope_in_org(
          &mut conn,
          existing.org_id,
          &existing.target_type,
          existing.target_id,
      )
      .await?
      {
          return Err(ApiError::NotFound);
      }

      // The two matcher fields are validated TOGETHER, against the merge of the
      // request and the stored row — patching only `detectors` to `[]` on a
      // policy with no keys must be refused, not silently accepted.
      let keys_in = req.tracked_keys.clone().unwrap_or(existing.tracked_keys.clone());
      let dets_in = req.detectors.clone().unwrap_or(existing.detectors.clone());
      let (keys, dets) = normalize_matchers(&keys_in, &dets_in)?;

      if let Some(tz) = req.schedule_tz.as_deref() {
          if !repo::timezone_is_valid(&mut conn, tz).await {
              return Err(ApiError::BadRequest(format!("unknown timezone {tz:?}")));
          }
      }
      if let Some(d) = req.schedule_days {
          if !(0..=127).contains(&d) {
              return Err(ApiError::BadRequest("schedule_days is a 0..127 weekday bitmask".into()));
          }
      }
      if let Some(w) = req.window_days {
          if !(1..=400).contains(&w) {
              return Err(ApiError::BadRequest("window_days must be between 1 and 400".into()));
          }
      }
      let time = match req.schedule_time.as_deref() {
          Some(s) => Some(parse_hhmm(s)?),
          None => None,
      };
      let now = chrono::Utc::now();
      let patched = repo::patch_inspector_policy(
          &mut conn,
          id,
          InspectorPolicyPatch {
              enabled: req.enabled,
              tracked_keys: Some(&keys),
              detectors: Some(&dets),
              scan_columns: req.scan_columns.as_ref().map(Some),
              rollups: req.rollups.as_ref(),
              window_days: req.window_days,
              schedule_enabled: req.schedule_enabled,
              schedule_days: req.schedule_days,
              schedule_time: time,
              schedule_tz: req.schedule_tz.as_deref(),
              updated_at: Some(now),
          },
      )
      .await?
      .ok_or(ApiError::NotFound)?;
      repo::reschedule_policy(&mut conn, patched.id).await?;
      let fresh = repo::get_inspector_policy(&mut conn, id).await?;
      Ok(Json(json!(fresh)))
  }

  pub async fn delete_policy(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(id): Path<Uuid>,
  ) -> Result<StatusCode, ApiError> {
      let mut conn = db(&state).await?;
      let p = repo::get_inspector_policy(&mut conn, id).await?.ok_or(ApiError::NotFound)?;
      authorize_policy(&mut conn, auth.user_id, &p.target_type, p.target_id, perm::PII_MANAGE).await?;
      repo::delete_inspector_policy(&mut conn, id).await?;
      Ok(StatusCode::NO_CONTENT)
  }

  /// The policy that actually governs this app, for the app picker.
  ///
  /// Also reports the enforcement latency the pipeline really uses, so the UI
  /// states a number rather than hardcoding "30 seconds" — the key lives in
  /// `sauron.env` precisely so the API and the enforcer cannot diverge.
  pub async fn effective_policy(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(app_id): Path<Uuid>,
      Query(env): Query<super::scope::RejectEnvQuery>,
  ) -> Result<Json<Value>, ApiError> {
      super::scope::reject_environment_id_with_message(env.environment_id.as_deref(), ENV_SCOPE_MESSAGE)?;
      let mut conn = db(&state).await?;
      authorize_app(&mut conn, auth.user_id, app_id, perm::PII_READ).await?;
      let policy = repo::effective_policy_for_app(&mut conn, app_id).await?;
      let masked_keys = repo::list_masked_keys(&mut conn, app_id).await?;
      Ok(Json(json!({
          "policy": policy,
          "masked_keys": masked_keys,
          "enforcement_latency_secs": state.cfg.inspector_policy_cache_secs,
          "hot_window_days": state.cfg.tier_hot_days,
      })))
  }
  ```

- [ ] **Step 5: Implement the scan routes.** Append to `inspector.rs`:
  ```rust
  #[derive(Deserialize)]
  pub struct ListLimit {
      #[serde(default)]
      pub limit: Option<i64>,
  }

  pub async fn list_scans(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(policy_id): Path<Uuid>,
      Query(q): Query<ListLimit>,
  ) -> Result<Json<Value>, ApiError> {
      let mut conn = db(&state).await?;
      let p = repo::get_inspector_policy(&mut conn, policy_id).await?.ok_or(ApiError::NotFound)?;
      authorize_policy(&mut conn, auth.user_id, &p.target_type, p.target_id, perm::PII_READ).await?;
      let rows = repo::list_scans_for_policy(&mut conn, policy_id, clamp_limit(q.limit)).await?;
      Ok(Json(json!(rows)))
  }

  /// Queue a manual scan.
  ///
  /// The 409 comes from the partial unique index `inspector_scans_active_key`,
  /// not from a handler pre-check: two clients racing must produce one scan, and
  /// a check-then-insert cannot promise that.
  pub async fn start_scan(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(policy_id): Path<Uuid>,
  ) -> Result<(StatusCode, Json<Value>), ApiError> {
      let mut conn = db(&state).await?;
      let p = repo::get_inspector_policy(&mut conn, policy_id).await?.ok_or(ApiError::NotFound)?;
      authorize_policy(&mut conn, auth.user_id, &p.target_type, p.target_id, perm::PII_MANAGE).await?;
      if !repo::validate_scope_in_org(&mut conn, p.org_id, &p.target_type, p.target_id).await? {
          return Err(ApiError::NotFound);
      }

      // ONE enqueue, shared with the scheduler. Re-deriving `params`, `targets`
      // and `units_total` here is how a manual scan comes to walk environments
      // a narrower disabled policy excluded, scan a table list that omits every
      // rollup, and record `units_total = 0` so the progress bar never moves.
      // Every one of those is invisible until someone reads a finding set and
      // trusts it.
      match repo::enqueue_scan_for_policy(
          &mut conn,
          &state.cfg,
          &p,
          "manual",
          Some(auth.user_id),
      )
      .await
      .map_err(|e| ApiError::Internal(e.to_string()))?
      {
          repo::EnqueueOutcome::Queued(scan) => Ok((StatusCode::ACCEPTED, Json(json!(scan)))),
          repo::EnqueueOutcome::AlreadyActive => {
              let active = repo::active_scan_for_policy(&mut conn, policy_id).await?;
              Err(ApiError::Conflict(format!(
                  "a scan is already queued or running for this policy (id {})",
                  active.map(|s| s.id.to_string()).unwrap_or_default()
              )))
          }
          repo::EnqueueOutcome::NoMatchers => Err(ApiError::BadRequest(
              "this policy has neither tracked keys nor detectors; it would report a false negative"
                  .into(),
          )),
          repo::EnqueueOutcome::TargetGone => Err(ApiError::NotFound),
          repo::EnqueueOutcome::FullySubtracted => Err(ApiError::BadRequest(
              "every app and environment under this policy is covered by a more specific policy; \
               there is nothing left for it to scan"
                  .into(),
          )),
      }
  }

  pub async fn get_scan(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(scan_id): Path<Uuid>,
  ) -> Result<Json<Value>, ApiError> {
      let mut conn = db(&state).await?;
      let s = repo::get_inspector_scan(&mut conn, scan_id).await?.ok_or(ApiError::NotFound)?;
      let p = repo::get_inspector_policy(&mut conn, s.policy_id).await?.ok_or(ApiError::NotFound)?;
      authorize_policy(&mut conn, auth.user_id, &p.target_type, p.target_id, perm::PII_READ).await?;
      Ok(Json(json!(s)))
  }

  pub async fn cancel_scan(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(scan_id): Path<Uuid>,
  ) -> Result<Json<Value>, ApiError> {
      let mut conn = db(&state).await?;
      let s = repo::get_inspector_scan(&mut conn, scan_id).await?.ok_or(ApiError::NotFound)?;
      let p = repo::get_inspector_policy(&mut conn, s.policy_id).await?.ok_or(ApiError::NotFound)?;
      // PII_MANAGE, not the group's PII_READ: inheriting the read permission
      // would let every audit reader block a queued scan.
      authorize_policy(&mut conn, auth.user_id, &p.target_type, p.target_id, perm::PII_MANAGE).await?;
      let n = repo::request_scan_cancel(&mut conn, scan_id).await?;
      if n == 0 {
          return Err(ApiError::Conflict("this scan is already finished".into()));
      }
      Ok(Json(json!({ "ok": true })))
  }
  ```

- [ ] **Step 6: Build and lint.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Clean. If `repo::env_ancestries`' return shape is not `(env_id, app_id, project_id)`, read it with `grep -n 'pub async fn env_ancestries' -A 12 crates/sauron-db/src/repo.rs` and adjust the destructuring in `list_policies`.

- [ ] **Step 7: Drive the routes over HTTP.** Start the API, then with an Owner token: `POST /v1/orgs/{org}/inspector/policies` with `{"target_type":"app","target_id":"<app>","tracked_keys":["Email"]}` → 200 with `tracked_keys` lowercased and `next_run_at` null. Repeat with `{"tracked_keys":[],"detectors":[]}` → 400 naming the false-negative reason. Repeat with a foreign `target_id` → 404. `POST /v1/inspector/policies/{id}/scans` twice → 202 then 409 carrying the active scan id. `POST /v1/inspector/scans/{id}/cancel` with a `pii:read`-only token → 403.

- [ ] **Step 8: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 27: `routes/inspector.rs` — findings, reveal, and the findings CSV

**Files:**
- Modify `backend/bins/sauron-api/src/routes/inspector.rs`

**Interfaces:**
- Consumes: `repo::{list_findings_for_scan, count_findings_for_scan, get_inspector_finding, reveal_one_value, insert_reveal_audit}` (Task 17), `sauron_inspector::columns::find` (Task 5), `crate::csv` (S4's writer), `routes::auth::client_addr` (S2, `pub(crate)`).
- Produces: handlers `list_findings`, `reveal_finding`; helper `csv_response`; `repo::user_email`; the new `ApiError::Gone` variant and its `StatusCode::GONE` arm in the error mapper — a masked or tier-dropped source row is a 410, not a 404, because "the row is gone" and "you cannot see this finding" must not be the same answer.

> **No value or sample column in the export.** An export of a PII report that contains the PII is a PII dump with a friendly filename — it lands in email, Slack and laptops, i.e. precisely the places this feature exists to keep data out of. Locations are what the export is for: handing a remediation list to the team that owns the SDK integration. An `include_values=1` opt-in was rejected because an opt-in that everybody ticks is a default.

- [ ] **Step 1: Implement the findings list with keyset paging and CSV.** Append to `inspector.rs`:
  ```rust
  use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
  use axum::response::{IntoResponse, Response};

  #[derive(Deserialize)]
  pub struct FindingsQuery {
      #[serde(default)]
      pub limit: Option<i64>,
      /// Keyset position: the previous page's last `(match_count, id)`.
      #[serde(default)]
      pub after_count: Option<i64>,
      #[serde(default)]
      pub after_id: Option<Uuid>,
      #[serde(default)]
      pub format: Option<String>,
  }

  /// Build a buffered CSV response.
  ///
  /// The CORS layer needs `.expose_headers([CONTENT_DISPOSITION])` for the
  /// split-origin topology the product ships, or the browser cannot read the
  /// filename — S4 added that line; this route only depends on it.
  fn csv_response(filename: &str, body: String) -> Response {
      (
          [
              (CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
              (
                  CONTENT_DISPOSITION,
                  format!("attachment; filename=\"{filename}\""),
              ),
          ],
          body,
      )
          .into_response()
  }

  pub async fn list_findings(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(scan_id): Path<Uuid>,
      Query(q): Query<FindingsQuery>,
  ) -> Result<Response, ApiError> {
      let mut conn = db(&state).await?;
      let scan = repo::get_inspector_scan(&mut conn, scan_id).await?.ok_or(ApiError::NotFound)?;
      let policy = repo::get_inspector_policy(&mut conn, scan.policy_id).await?.ok_or(ApiError::NotFound)?;
      authorize_policy(
          &mut conn,
          auth.user_id,
          &policy.target_type,
          policy.target_id,
          perm::PII_READ,
      )
      .await?;

      if q.format.as_deref() == Some("csv") {
          let total = repo::count_findings_for_scan(&mut conn, scan_id).await?;
          // A buffered export cannot be truncated honestly, so refuse rather
          // than silently ship a prefix of the answer.
          if total > state.cfg.inspector_export_max_rows {
              return Err(ApiError::BadRequest(format!(
                  "too_many_rows: {total} findings exceeds INSPECTOR_EXPORT_MAX_ROWS \
                   ({}); narrow the scan or raise the ceiling",
                  state.cfg.inspector_export_max_rows
              )));
          }
          let rows = repo::list_findings_for_scan(&mut conn, scan_id, total.max(1), None).await?;
          let mut w = crate::csv::Writer::new();
          w.row(&[
              "finding_id", "scan_id", "detected_at", "app_id", "environment_id", "env_scope",
              "table", "column", "json_path", "matched_key", "detector", "match_count",
              "match_count_exact", "first_seen_at", "last_seen_at", "partition_kind", "value_type",
          ]);
          for f in &rows {
              // The formula-injection guard applies to `json_path` and
              // `matched_key` too, not only to free text: both are
              // DEV-CONTROLLED BYTES, so a key literally named `=cmd|'...'` is a
              // spreadsheet payload.
              w.row(&[
                  &f.id.to_string(),
                  &f.scan_id.to_string(),
                  &f.created_at.to_rfc3339(),
                  &f.app_id.to_string(),
                  &f.environment_id.map(|e| e.to_string()).unwrap_or_default(),
                  &f.env_scope,
                  &f.source_table,
                  &f.source_column,
                  &f.key_path,
                  &f.matched_key,
                  &f.detector,
                  &f.match_count.to_string(),
                  &f.match_count_exact.to_string(),
                  &f.first_seen_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
                  &f.last_seen_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
                  &f.partition_kind,
                  &f.value_type,
              ]);
          }
          let name = format!(
              "sauron-inspector-findings_{}_{}.csv",
              scan_id,
              scan.window_to.format("%Y-%m-%d")
          );
          return Ok(csv_response(&name, w.finish()));
      }

      let after = match (q.after_count, q.after_id) {
          (Some(c), Some(i)) => Some((c, i)),
          _ => None,
      };
      let rows = repo::list_findings_for_scan(&mut conn, scan_id, clamp_limit(q.limit), after).await?;
      Ok(Json(json!({
          "findings": rows,
          "coverage": scan.coverage,
          "coverage_note": scan.coverage_note,
          // Non-dismissible in the UI. The phase-1 prefilter greps the JSON TEXT
          // for the quoted key name, so a key serialized with a unicode escape
          // evades it, as does anything inside a base64 or URL-encoded blob.
          "detection_caveat": "Detection is best-effort, not a compliance guarantee. \
                               Keys hidden by unicode escapes, base64 or URL encoding are not found.",
      }))
      .into_response())
  }
  ```
  If S4's CSV writer exposes a different API than `Writer::new()` / `.row(&[..])` / `.finish()`, read it with `cd /home/splimter/projects/freelance/sauron/backend && sed -n '1,80p' bins/sauron-api/src/csv.rs` and call it as written there. **Do not add a second escaper.**

- [ ] **Step 2: Implement reveal.** Append to `inspector.rs`:
  ```rust
  /// The ONLY place a raw value is ever produced.
  ///
  /// POST rather than GET so the identifier does not land in access logs and so
  /// the audit row has a request body to record. The audit row is written BEFORE
  /// the value is returned, so a failure to audit is a failure to reveal.
  pub async fn reveal_finding(
      auth: AuthUser,
      State(state): State<AppState>,
      headers: axum::http::HeaderMap,
      axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
      Path(finding_id): Path<Uuid>,
  ) -> Result<Json<Value>, ApiError> {
      let mut conn = db(&state).await?;
      let f = repo::get_inspector_finding(&mut conn, finding_id).await?.ok_or(ApiError::NotFound)?;
      authorize_app(&mut conn, auth.user_id, f.app_id, perm::PII_READ).await?;

      // `stacktrace_symbolicated` frames carry context_line/pre_context/
      // post_context — verbatim customer source — which `strip_source_context`
      // removes from RESPONSES only when the caller lacks `source:read`. A
      // pii:read holder without source:read could otherwise track the key
      // `pre_context`, reveal, and receive de-obfuscated proprietary source.
      let entry = sauron_inspector::columns::find(&f.source_table, &f.source_column)
          .ok_or_else(|| ApiError::BadRequest("this finding's column is not in the inventory".into()))?;
      if !entry.reveal_ok {
          return Err(ApiError::BadRequest(format!(
              "{}.{} is not reveal-eligible; the redacted preview is all this endpoint returns",
              f.source_table, f.source_column
          )));
      }
      let Some(row_id) = f.sample_row_id else {
          return Err(ApiError::NotFound);
      };

      let source = crate::routes::auth::client_addr(&headers, peer, &state);
      let email = repo::user_email(&mut conn, auth.user_id).await?.unwrap_or_default();
      repo::insert_reveal_audit(
          &mut conn,
          sauron_db::models::NewInspectorRevealAudit {
              app_id: f.app_id,
              org_id: f.org_id,
              finding_id: Some(f.id),
              user_id: Some(auth.user_id),
              user_email: &email,
              source_table: &f.source_table,
              source_column: &f.source_column,
              key_path: &f.key_path,
              request_source: &source,
          },
      )
      .await?;

      let value = repo::reveal_one_value(
          &mut conn,
          entry.table,
          entry.column,
          row_id,
          f.sample_occurred_at,
          f.app_id,
      )
      .await?;
      // 410 when the row is absent — its partition was dropped by `sauron-tier`,
      // or a rollup row was replaced. Also 410 on an app_id mismatch, so an
      // attribution bug becomes a benign miss rather than a cross-tenant
      // disclosure.
      let Some(doc) = value else {
          return Err(ApiError::Gone("the row this finding points at is gone".into()));
      };

      // Extract exactly the one key_path in Rust. Nothing is persisted.
      let mut cur = &doc;
      for seg in f.key_path.split('.') {
          let seg = seg.strip_suffix("[]").unwrap_or(seg);
          match cur.get(seg) {
              Some(next) => cur = next,
              None => return Err(ApiError::Gone("the path no longer exists in this row".into())),
          }
      }
      Ok(Json(json!({
          "path": f.key_path,
          "value": cur,
          "type": sauron_inspector::redact::value_type(cur),
      })))
  }
  ```

- [ ] **Step 3: Add the two things reveal needs.** In `backend/bins/sauron-api/src/error.rs`, add a `Gone(String)` variant to `ApiError` and its arm in `IntoResponse`:
  ```rust
  /// The locator resolved to nothing: the partition was dropped by
  /// `sauron-tier`, the rollup row was replaced, or the tenant did not match.
  /// Distinct from 404 so the UI can say "this data has aged out" rather than
  /// "no such finding".
  Gone(String),
  ```
  and in the match:
  ```rust
  ApiError::Gone(m) => body(StatusCode::GONE, "gone", &m),
  ```
  Then append to `repo.rs`:
  ```rust
  /// The email to denormalize into an audit row. `SET NULL` on the FK loses the
  /// identity, so the trail carries a snapshot.
  pub async fn user_email(conn: &mut AsyncPgConnection, user_id: Uuid) -> QueryResult<Option<String>> {
      users::table
          .find(user_id)
          .select(users::email)
          .first(conn)
          .await
          .optional()
  }
  ```

- [ ] **Step 4: Build.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Clean. If `client_addr`'s signature differs, read it with `grep -n 'pub(crate) fn client_addr' -A 10 bins/sauron-api/src/routes/auth.rs` and call it as written — do not re-implement it.

- [ ] **Step 5: Drive reveal and the CSV over HTTP.** With a scan that produced findings: `POST /v1/inspector/findings/{id}/reveal` → 200 with the raw value, and `SELECT user_email, key_path, request_source FROM inspector_reveal_audit ORDER BY created_at DESC LIMIT 1` shows the row. Reveal a finding whose `source_column` is `stacktrace_symbolicated` → 400. Reveal one whose `sample_row_id` points at a dropped partition → 410. `GET /v1/inspector/scans/{id}/findings?format=csv` → a file whose header row has **no** value or sample column; open it in a spreadsheet and confirm a `json_path` beginning `=` renders as text, not a formula.

- [ ] **Step 6: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 28: `routes/inspector.rs` — mask preview, confirm, cancel, audit, and the deactivation hook

**Files:**
- Modify `backend/bins/sauron-api/src/routes/inspector.rs`
- Modify `backend/bins/sauron-api/src/routes/orgs.rs` (`set_member_active`)

**Interfaces:**
- Consumes: `repo::{insert_mask_action, get_mask_action, list_mask_actions_for_app, list_mask_actions_for_org, confirm_mask_action, cancel_mask_action, list_masked_keys, cancel_pending_mask_actions_for_user, get_inspector_finding}` (Tasks 17–18), `repo::user_email` (Task 27), `repo::app_ancestry` (already in `repo.rs`, `(conn, app_id) -> QueryResult<Option<(project_id, org_id)>>`), `sauron_inspector::targets::{MaskTarget, expand_targets, validate_target}` + `path::finding_path_to_mask_path` (Tasks 11–12), `routes::auth::client_addr` (S2).
- Produces: handlers `mask_preview`, `get_mask_action_handler`, `confirm_mask`, `cancel_mask`, `list_app_mask_actions`, `list_org_mask_actions`, `list_app_masked_keys`.

- [ ] **Step 1: Implement mask preview.** Append to `inspector.rs`:
  ```rust
  use sauron_inspector::path::finding_path_to_mask_path;
  use sauron_inspector::targets::{expand_targets, validate_target, MaskTarget};

  #[derive(Deserialize)]
  pub struct MaskPreviewReq {
      /// Preferred form: derive the targets from a finding, so the paths the
      /// scanner actually saw are the paths the mask writes.
      #[serde(default)]
      pub finding_id: Option<Uuid>,
      /// Explicit form, for a target an admin knows about without a scan.
      #[serde(default)]
      pub targets: Option<Vec<MaskTarget>>,
  }

  /// Start a counting pass. Returns 202 and an id the dashboard polls.
  ///
  /// The count is NOT run here. `col #> path IS NOT NULL` over an app's hot
  /// window is a Parallel Append seq scan — 184 ms per 210k rows measured — with
  /// no index that can serve it, since the tags GIN is `jsonb_path_ops` and
  /// answers `@>` only. Running that on the API's 16-connection pool is how the
  /// whole dashboard goes down.
  pub async fn mask_preview(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(app_id): Path<Uuid>,
      Query(env): Query<super::scope::RejectEnvQuery>,
      Json(req): Json<MaskPreviewReq>,
  ) -> Result<(StatusCode, Json<Value>), ApiError> {
      super::scope::reject_environment_id_with_message(env.environment_id.as_deref(), ENV_SCOPE_MESSAGE)?;
      let mut conn = db(&state).await?;
      let app = authorize_app(&mut conn, auth.user_id, app_id, perm::PII_MANAGE).await?;

      let (mut base, finding_id, scan_id) = match (req.finding_id, req.targets) {
          (Some(fid), _) => {
              let f = repo::get_inspector_finding(&mut conn, fid).await?.ok_or(ApiError::NotFound)?;
              // Both `finding_id` and `scan_id` are validated against `app_id`
              // here, at preview: the audit row outlives finding pruning through
              // ON DELETE SET NULL, so this is the last moment the link is
              // checkable.
              if f.app_id != app_id {
                  return Err(ApiError::NotFound);
              }
              let table = sauron_inspector::targets::TargetTable::from_sql(&f.source_table)
                  .ok_or_else(|| ApiError::BadRequest(format!("{} is not maskable", f.source_table)))?;
              let column = sauron_inspector::targets::TargetColumn::from_sql(&f.source_column)
                  .ok_or_else(|| ApiError::BadRequest(format!("{} is not maskable", f.source_column)))?;
              let entry = sauron_inspector::columns::find(&f.source_table, &f.source_column)
                  .ok_or_else(|| ApiError::BadRequest("unknown column".into()))?;
              let path = if entry.kind == sauron_inspector::columns::ColumnKind::Text {
                  String::new()
              } else {
                  finding_path_to_mask_path(&f.key_path).map_err(|e| {
                      ApiError::BadRequest(format!(
                          "this finding's path cannot be expressed as a mask path ({e:?})"
                      ))
                  })?
              };
              (vec![MaskTarget { table, column, path }], Some(f.id), Some(f.scan_id))
          }
          (None, Some(t)) if !t.is_empty() => (t, None, None),
          _ => {
              return Err(ApiError::BadRequest(
                  "supply either finding_id or a non-empty targets array".into(),
              ))
          }
      };

      // Companion expansion happens HERE, at preview, and is frozen into
      // `targets` — confirm cannot supply targets at all, so it can never widen
      // what was counted and shown.
      let mut expanded: Vec<MaskTarget> = Vec::new();
      for t in base.drain(..) {
          for e in expand_targets(&t) {
              // `expand_targets` is a pure map and can produce entries the
              // allowlist refuses (stacktrace_symbolicated); validation is the
              // gate, and a refused companion is dropped rather than failing the
              // whole request.
              if validate_target(&e).is_ok() && !expanded.contains(&e) {
                  expanded.push(e);
              }
          }
      }
      if expanded.is_empty() {
          return Err(ApiError::BadRequest("no maskable target survived validation".into()));
      }

      let targets_json = serde_json::to_value(&expanded).map_err(|e| ApiError::Internal(e.to_string()))?;
      let email = repo::user_email(&mut conn, auth.user_id).await?.unwrap_or_default();
      let (project_id, org_id) = repo::app_ancestry(&mut conn, app_id)
          .await
          .map_err(|e| ApiError::Internal(e.to_string()))?
          .ok_or(ApiError::NotFound)?;
      let _ = project_id;
      let action = repo::insert_mask_action(
          &mut conn,
          sauron_db::models::NewInspectorMaskAction {
              org_id,
              app_id,
              kind: "preview",
              finding_id,
              scan_id,
              targets: &targets_json,
              requested_by: Some(auth.user_id),
              requested_by_email: &email,
          },
      )
      .await?;
      Ok((
          StatusCode::ACCEPTED,
          Json(json!({
              "action": action,
              "app_slug": app.slug,
              "preview_ttl_secs": state.cfg.inspector_preview_ttl_secs,
              "mask_max_rows": state.cfg.inspector_mask_max_rows,
              "enforcement_latency_secs": state.cfg.inspector_policy_cache_secs,
          })),
      ))
  }
  ```

- [ ] **Step 2: Implement confirm and cancel.** Append to `inspector.rs`:
  ```rust
  #[derive(Deserialize)]
  pub struct ConfirmReq {
      /// Must equal the app's slug.
      pub confirm_text: String,
  }

  /// Promote `previewed` -> `pending`.
  ///
  /// Typing the SLUG is the only confirmation that forces attention onto the
  /// thing that actually goes wrong. The realistic failure is not a mis-click —
  /// it is masking the WRONG APP, because the operator saw a finding and forgot
  /// which app was selected. A typed literal like `MASK` proves intent and
  /// proves nothing about scope, and `ConfirmDialog` has no text input at all.
  pub async fn confirm_mask(
      auth: AuthUser,
      State(state): State<AppState>,
      headers: axum::http::HeaderMap,
      axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
      Path(action_id): Path<Uuid>,
      Json(req): Json<ConfirmReq>,
  ) -> Result<Json<Value>, ApiError> {
      let mut conn = db(&state).await?;
      let action = repo::get_mask_action(&mut conn, action_id).await?.ok_or(ApiError::NotFound)?;
      // A FRESH authorization, not the one preview did: an operator can lose
      // pii:manage between counting and confirming.
      let app = authorize_app(&mut conn, auth.user_id, action.app_id, perm::PII_MANAGE).await?;
      if req.confirm_text.trim() != app.slug {
          return Err(ApiError::BadRequest(
              "confirm_text must be the app slug exactly".into(),
          ));
      }

      // `client_addr` records its own trust decision, because
      // API_TRUST_FORWARDED_HEADERS defaults to FALSE in Config::from_env, in
      // packaging/rpm/config/api.env and in docker-compose, and the RPM ships
      // nginx in front of the API — so behind the only packaged topology this
      // field records the same constant for every actor unless the operator
      // turns the flag on.
      let source = format!(
          "{} ua={}",
          crate::routes::auth::client_addr(&headers, peer, &state),
          headers
              .get(axum::http::header::USER_AGENT)
              .and_then(|v| v.to_str().ok())
              .map(|s| s.chars().take(120).collect::<String>())
              .unwrap_or_default()
      );

      // Every gate — status, TTL from `previewed_at`, and the row ceiling — is
      // IN THE STATEMENT, so a double-clicked confirm and a concurrent second
      // confirm both resolve to "0 rows updated" instead of racing.
      let n = repo::confirm_mask_action(
          &mut conn,
          action_id,
          &source,
          state.cfg.inspector_preview_ttl_secs,
          state.cfg.inspector_mask_max_rows,
      )
      .await?;
      if n == 0 {
          let fresh = repo::get_mask_action(&mut conn, action_id).await?.ok_or(ApiError::NotFound)?;
          if fresh.estimated_rows > state.cfg.inspector_mask_max_rows {
              return Err(ApiError::Conflict(format!(
                  "this mask would rewrite {} rows, above INSPECTOR_MASK_MAX_ROWS ({}); \
                   raise the ceiling explicitly if that is intended",
                  fresh.estimated_rows, state.cfg.inspector_mask_max_rows
              )));
          }
          return Err(ApiError::Conflict(
              "the preview is not ready or has expired; run it again".into(),
          ));
      }
      let fresh = repo::get_mask_action(&mut conn, action_id).await?;
      Ok(Json(json!({
          "action": fresh,
          // The literal number the enforcer uses, so the UI never hardcodes it.
          "enforcement_latency_secs": state.cfg.inspector_policy_cache_secs,
      })))
  }

  /// Stop a queued or running mask.
  ///
  /// PII_MANAGE, NOT the group's PII_READ: inheriting the read permission would
  /// let every audit reader block a queued redaction. And the actor is recorded,
  /// because in an audit table whose whole justification is "who did it", the
  /// one adversarial action the design permits must not be the one it cannot
  /// attribute.
  pub async fn cancel_mask(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(action_id): Path<Uuid>,
  ) -> Result<Json<Value>, ApiError> {
      let mut conn = db(&state).await?;
      let action = repo::get_mask_action(&mut conn, action_id).await?.ok_or(ApiError::NotFound)?;
      authorize_app(&mut conn, auth.user_id, action.app_id, perm::PII_MANAGE).await?;
      let email = repo::user_email(&mut conn, auth.user_id).await?.unwrap_or_default();
      let n = repo::cancel_mask_action(&mut conn, action_id, Some(auth.user_id), &email).await?;
      if n == 0 {
          return Err(ApiError::Conflict("this action is already finished".into()));
      }
      let fresh = repo::get_mask_action(&mut conn, action_id).await?;
      Ok(Json(json!(fresh)))
  }
  ```

- [ ] **Step 3: Implement the audit reads and the masked-key list.** Append to `inspector.rs`:
  ```rust
  pub async fn get_mask_action_handler(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(action_id): Path<Uuid>,
  ) -> Result<Json<Value>, ApiError> {
      let mut conn = db(&state).await?;
      let a = repo::get_mask_action(&mut conn, action_id).await?.ok_or(ApiError::NotFound)?;
      // pii:read only — deliberately readable by someone other than the actor,
      // which is affordable precisely because the row stores PATHS AND COUNTS
      // and never a value.
      authorize_app(&mut conn, auth.user_id, a.app_id, perm::PII_READ).await?;
      Ok(Json(json!(a)))
  }

  fn audit_csv(rows: &[sauron_db::models::InspectorMaskAction], label: &str) -> Response {
      let mut w = crate::csv::Writer::new();
      w.row(&[
          "action_id", "requested_at", "confirmed_at", "finished_at", "requested_by_email",
          "cancelled_by_email", "app_id", "status", "targets", "estimated_rows", "rows_masked",
          "cold_rows_skipped", "cold_boundary_at", "error",
      ]);
      for a in rows {
          // Semicolon-joined `table.column.path`. Paths only — never values.
          let targets = a
              .targets
              .as_array()
              .map(|arr| {
                  arr.iter()
                      .map(|t| {
                          format!(
                              "{}.{}{}",
                              t.get("table").and_then(|v| v.as_str()).unwrap_or(""),
                              t.get("column").and_then(|v| v.as_str()).unwrap_or(""),
                              match t.get("path").and_then(|v| v.as_str()) {
                                  Some(p) if !p.is_empty() => format!(".{p}"),
                                  _ => String::new(),
                              }
                          )
                      })
                      .collect::<Vec<_>>()
                      .join(";")
              })
              .unwrap_or_default();
          w.row(&[
              &a.id.to_string(),
              &a.requested_at.to_rfc3339(),
              &a.confirmed_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
              &a.finished_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
              &a.requested_by_email,
              &a.cancelled_by_email,
              &a.app_id.to_string(),
              &a.status,
              &targets,
              &a.estimated_rows.to_string(),
              &a.rows_masked.to_string(),
              &a.cold_rows_skipped.to_string(),
              &a.cold_boundary_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
              &a.error,
          ]);
      }
      csv_response(
          &format!("sauron-inspector-mask-actions_{label}.csv"),
          w.finish(),
      )
  }

  /// The findings query struct has no `environment_id` field, and passing
  /// `None` to the rejection helper is a call that can never reject — so this
  /// route needs its own struct that actually carries the parameter.
  #[derive(Deserialize)]
  pub struct AuditQuery {
      #[serde(default)]
      pub limit: Option<i64>,
      #[serde(default)]
      pub format: Option<String>,
      #[serde(default)]
      pub environment_id: Option<String>,
  }

  pub async fn list_app_mask_actions(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(app_id): Path<Uuid>,
      Query(q): Query<AuditQuery>,
  ) -> Result<Response, ApiError> {
      super::scope::reject_environment_id_with_message(q.environment_id.as_deref(), ENV_SCOPE_MESSAGE)?;
      let mut conn = db(&state).await?;
      let app = authorize_app(&mut conn, auth.user_id, app_id, perm::PII_READ).await?;
      let limit = if q.format.as_deref() == Some("csv") {
          state.cfg.inspector_export_max_rows
      } else {
          clamp_limit(q.limit)
      };
      let rows = repo::list_mask_actions_for_app(&mut conn, app_id, limit).await?;
      if q.format.as_deref() == Some("csv") {
          if rows.len() as i64 >= state.cfg.inspector_export_max_rows {
              return Err(ApiError::BadRequest(
                  "too_many_rows: narrow the range or raise INSPECTOR_EXPORT_MAX_ROWS".into(),
              ));
          }
          return Ok(audit_csv(&rows, &app.slug));
      }
      Ok(Json(json!(rows)).into_response())
  }

  /// Org-wide audit export.
  ///
  /// Note this exports `requested_by_email` for every action, which makes a
  /// downloadable STAFF-EMAIL ROSTER available to any org-scoped pii:read
  /// holder. That is a deliberate trade for an audit trail, it is bounded by the
  /// pseudonymization reaper, and it is stated in the wiki.
  pub async fn list_org_mask_actions(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(org_id): Path<Uuid>,
      Query(q): Query<FindingsQuery>,
  ) -> Result<Response, ApiError> {
      let mut conn = db(&state).await?;
      authorize_org(&mut conn, auth.user_id, org_id, perm::PII_READ).await?;
      let limit = if q.format.as_deref() == Some("csv") {
          state.cfg.inspector_export_max_rows
      } else {
          clamp_limit(q.limit)
      };
      let rows = repo::list_mask_actions_for_org(&mut conn, org_id, limit).await?;
      if q.format.as_deref() == Some("csv") {
          if rows.len() as i64 >= state.cfg.inspector_export_max_rows {
              return Err(ApiError::BadRequest(
                  "too_many_rows: narrow the range or raise INSPECTOR_EXPORT_MAX_ROWS".into(),
              ));
          }
          return Ok(audit_csv(&rows, &org_id.to_string()));
      }
      Ok(Json(json!(rows)).into_response())
  }

  pub async fn list_app_masked_keys(
      auth: AuthUser,
      State(state): State<AppState>,
      Path(app_id): Path<Uuid>,
      Query(env): Query<super::scope::RejectEnvQuery>,
  ) -> Result<Json<Value>, ApiError> {
      super::scope::reject_environment_id_with_message(env.environment_id.as_deref(), ENV_SCOPE_MESSAGE)?;
      let mut conn = db(&state).await?;
      authorize_app(&mut conn, auth.user_id, app_id, perm::PII_READ).await?;
      let rows = repo::list_masked_keys(&mut conn, app_id).await?;
      Ok(Json(json!({
          "masked_keys": rows,
          "enforcement_latency_secs": state.cfg.inspector_policy_cache_secs,
      })))
  }
  ```

- [ ] **Step 4: Cancel a deactivated member's queued destruction.** In `backend/bins/sauron-api/src/routes/orgs.rs`, inside `set_member_active`, immediately after the branch that revokes the target's sessions on deactivation, add:
  ```rust
  // A deactivated member's QUEUED mask actions must not execute. Confirm
  // re-authorizes, but the action then sits in `pending` — with one slot per
  // worker and a 200 ms inter-batch pause, a backlog can be hours deep — and
  // deactivation revokes refresh tokens while touching nothing queued. The
  // worker re-checks authorization at claim too; this is the fast path so the
  // action never runs at all.
  if !req.is_active {
      let cancelled = repo::cancel_pending_mask_actions_for_user(&mut conn, user_id).await?;
      if cancelled > 0 {
          tracing::info!(
              user_id = %user_id,
              cancelled,
              "cancelled queued PII mask actions for a deactivated member"
          );
      }
  }
  ```
  Match the surrounding code's names for `req.is_active`, `user_id` and `conn` — read the function first with `cd /home/splimter/projects/freelance/sauron/backend && grep -n 'pub async fn set_member_active' -A 90 bins/sauron-api/src/routes/orgs.rs`.

- [ ] **Step 5: Build.** `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`. Clean.

- [ ] **Step 6: Drive the mask lifecycle over HTTP.** `POST /v1/apps/{app}/inspector/mask-preview` with `{"finding_id":"<id>"}` → 202 with the action and `app_slug`. Poll `GET /v1/inspector/mask-actions/{id}` until `status='previewed'`. `POST .../confirm` with the wrong slug → 400; with the right slug → 200 and `status='pending'`; a second confirm → 409. `POST .../cancel` from a `pii:read`-only token → 403. Wait out `INSPECTOR_PREVIEW_TTL_SECS` on a second preview and confirm → 409.

- [ ] **Step 7: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 29: Router wiring and the environment-scoping contract

**Files:**
- Modify `backend/bins/sauron-api/src/main.rs` (router chain, after the alerting routes)
- Modify `dashboard/src/lib/api/scope.ts` (`BACKEND_REJECTS_ENVIRONMENT_ID`)
- Modify `backend/bins/sauron-api/tests/http_env_scoping.rs` (no logic change; confirm it still passes)

**Interfaces:**
- Consumes: every handler from Tasks 26–28.
- Produces: 17 mounted routes.

> `bins/sauron-api/tests/http_env_scoping.rs` reads `BACKEND_REJECTS_ENVIRONMENT_ID` **out of the TypeScript source** and asserts it equals the set of app-scoped GETs that actually 400 on a valid `environment_id`. An entry added on one side without the other now fails a test instead of drifting silently.

- [ ] **Step 1: Mount the routes.** In `backend/bins/sauron-api/src/main.rs`, in the `Router::new()` chain after the alerting block, add:
  ```rust
  // --- pii inspector ---
  .route(
      "/v1/orgs/{org_id}/inspector/policies",
      get(routes::inspector::list_policies).post(routes::inspector::create_policy),
  )
  .route(
      "/v1/inspector/policies/{policy_id}",
      get(routes::inspector::get_policy)
          .patch(routes::inspector::patch_policy)
          .delete(routes::inspector::delete_policy),
  )
  .route(
      "/v1/apps/{app_id}/inspector/policy",
      get(routes::inspector::effective_policy),
  )
  .route(
      "/v1/inspector/policies/{policy_id}/scans",
      get(routes::inspector::list_scans).post(routes::inspector::start_scan),
  )
  .route("/v1/inspector/scans/{scan_id}", get(routes::inspector::get_scan))
  .route(
      "/v1/inspector/scans/{scan_id}/cancel",
      post(routes::inspector::cancel_scan),
  )
  .route(
      "/v1/inspector/scans/{scan_id}/findings",
      get(routes::inspector::list_findings),
  )
  .route(
      "/v1/inspector/findings/{finding_id}/reveal",
      post(routes::inspector::reveal_finding),
  )
  .route(
      "/v1/apps/{app_id}/inspector/mask-preview",
      post(routes::inspector::mask_preview),
  )
  .route(
      "/v1/apps/{app_id}/inspector/mask-actions",
      get(routes::inspector::list_app_mask_actions),
  )
  .route(
      "/v1/apps/{app_id}/inspector/masked-keys",
      get(routes::inspector::list_app_masked_keys),
  )
  .route(
      "/v1/inspector/mask-actions/{action_id}",
      get(routes::inspector::get_mask_action_handler),
  )
  .route(
      "/v1/inspector/mask-actions/{action_id}/confirm",
      post(routes::inspector::confirm_mask),
  )
  .route(
      "/v1/inspector/mask-actions/{action_id}/cancel",
      post(routes::inspector::cancel_mask),
  )
  .route(
      "/v1/orgs/{org_id}/inspector/mask-actions",
      get(routes::inspector::list_org_mask_actions),
  )
  ```

- [ ] **Step 2: Confirm the CORS header is exposed.** Run `cd /home/splimter/projects/freelance/sauron/backend && grep -n 'expose_headers' bins/sauron-api/src/main.rs`. Expected: `.expose_headers([CONTENT_DISPOSITION])` on the `CorsLayer`, added by S4. If it is absent, S4 did not land — stop and report rather than adding it here.

- [ ] **Step 3: Add the three app-scoped GET rejections to the dashboard mirror.** In `dashboard/src/lib/api/scope.ts`, extend the `BACKEND_REJECTS_ENVIRONMENT_ID` array and its explanatory comment block. Append to the comment list, before the array:
  ```
  //  - `/v1/apps/{id}/inspector/policy`,
  //    `/v1/apps/{id}/inspector/mask-actions`,
  //    `/v1/apps/{id}/inspector/masked-keys`
  //    (`inspector::effective_policy` / `list_app_mask_actions` /
  //    `list_app_masked_keys`) — the inspector is APP-scoped. Findings carry
  //    their own environment dimension inside the payload (`env_scope` plus
  //    `environment_id`), and MASKING cannot be limited to one environment at
  //    all: the pipeline enforcer keys on `app_id` alone, and a policy that
  //    masks in prod but not staging is a footgun that produces exactly the
  //    leak the feature exists to prevent. Each calls
  //    `reject_environment_id_with_message` with that reason.
  //
  //    `POST /v1/apps/{id}/inspector/mask-preview` rejects it too, and is
  //    deliberately NOT listed: `http_env_scoping.rs`'s
  //    `app_scoped_get_route_templates()` only collects `.route(...)` calls
  //    containing `get(`, and `the_backend_rejection_set_matches_the_dashboard_exclusion_list`
  //    asserts the collected set EQUALS this array. A POST-only path here is
  //    in `expected` and can never be in `rejecting`, so the contract test
  //    fails on a correct handler. The entries that are listed all have a GET.
  ```
  and add to the array:
  ```ts
  /^\/v1\/apps\/[^/]+\/inspector\/policy(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/inspector\/mask-actions(?:[/?].*)?$/,
  /^\/v1\/apps\/[^/]+\/inspector\/masked-keys(?:[/?].*)?$/,
  ```
  Do **not** add a `mask-preview` regex. The handler keeps its own
  `reject_environment_id_with_message` call — the dashboard never appends
  `environment_id` to a POST body route anyway, and Step 6 below smoke-tests
  the rejection directly.

- [ ] **Step 4: Run the env-scoping test.** `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api --test http_env_scoping`. Expected: green. A failure naming one of the three new URLs means the backend route does not actually reject `environment_id` — fix the handler, not the array.

- [ ] **Step 5: Run the dashboard scope test and typecheck.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test -- scope` then `npm run check`. Both green.

- [ ] **Step 6: Smoke every route.** With the API running, `curl` each of the 15 mounted paths with an Owner bearer token and confirm none returns 404 for "no such route" (404 for a missing row is fine) and none returns 500. Then `GET /v1/apps/{app}/inspector/policy?environment_id=<a real enrollment id>` and confirm 400 with the message `the inspector is app-scoped; masking cannot be limited to one environment`. Then the same for `/inspector/mask-actions` and `/inspector/masked-keys`, and — because it is deliberately absent from the dashboard array and so has no test covering it — `POST /v1/apps/{app}/inspector/mask-preview?environment_id=<the same id>` with a valid body, which must also be 400 with that message.

- [ ] **Step 7: Format and lint.** `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check` then `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`.

---

## Task 30: Dashboard pure models — `inspector.ts`, `inspector-schedule.ts`, `inspector-findings.ts`

**Files:**
- Create `dashboard/src/lib/models/inspector.ts` + `inspector.test.ts`
- Create `dashboard/src/lib/models/inspector-schedule.ts` + `inspector-schedule.test.ts`
- Create `dashboard/src/lib/models/inspector-findings.ts` + `inspector-findings.test.ts`
- Create `dashboard/src/lib/constants/inspectorSchedules.ts`

**Interfaces:**
- Consumes: nothing (pure).
- Produces: `UNREACHABLE_COPY`, `describeTarget`, `expandCompanionTargets`, `maskConfirmReady`, `csvFilename`; `weekdayMaskToArray`, `weekdayArrayToMask`, `describeSchedule`, `nextRuns`; `groupFindings`, `formatMatchCount`, `findingBadges`.

> **vitest is node-only and there is NO DOM test environment**, so this is where all the dashboard coverage lives. Nothing in these three files may import a `.svelte` file or touch `document`.

- [ ] **Step 1: Write the failing `inspector.test.ts`.** Create `dashboard/src/lib/models/inspector.test.ts`:
  ```ts
  import { describe, it, expect } from 'vitest';
  import {
    UNREACHABLE_COPY,
    describeTarget,
    expandCompanionTargets,
    maskConfirmReady,
    csvFilename,
  } from './inspector';

  describe('UNREACHABLE_COPY', () => {
    // One source, rendered verbatim in the MaskDialog, in the Audit tab detail
    // and in the wiki — so support answers and the product cannot diverge.
    it('leads with the hot-Postgres headline', () => {
      expect(UNREACHABLE_COPY[0].headline).toBe(true);
      expect(UNREACHABLE_COPY[0].what).toMatch(/hot Postgres only/i);
    });

    it('carries all twelve enumerated rows beneath the headline', () => {
      // A dropped row is a promise the dialog stops making.
      expect(UNREACHABLE_COPY.filter((r) => !r.headline)).toHaveLength(12);
      const subjects = UNREACHABLE_COPY.map((r) => r.what.toLowerCase()).join(' | ');
      for (const must of [
        'cold parquet',
        'tier_hot_days',
        'redis ingest stream',
        'dlq',
        'breadcrumbs',
        'alert_events',
        'already-delivered',
        'event_users.properties',
        'devices',
        'symbolicated',
        'backups',
        'active-users',
      ]) {
        expect(subjects).toContain(must);
      }
    });

    it('never claims a mask is permanent or removed', () => {
      const all = UNREACHABLE_COPY.map((r) => `${r.what} ${r.why} ${r.bounded}`).join(' ');
      expect(all).not.toMatch(/permanently removed/i);
    });

    it('marks the active-users row as read-before-confirm', () => {
      const row = UNREACHABLE_COPY.find((r) => r.what.toLowerCase().includes('active-user'));
      expect(row?.readFirst).toBe(true);
    });
  });

  describe('describeTarget', () => {
    it('names a jsonb path', () => {
      expect(describeTarget({ table: 'error_events', column: 'extra', path: 'customer.email' })).toBe(
        'error_events.extra → customer.email',
      );
    });

    it('says whole value for a text column', () => {
      expect(describeTarget({ table: 'issues', column: 'title', path: '' })).toBe(
        'issues.title → the whole value',
      );
    });
  });

  describe('expandCompanionTargets', () => {
    // Mirrors the backend map so the dialog can describe the blast radius
    // BEFORE the server answers.
    it('expands error_events.title to the wire sources and issues.title', () => {
      const out = expandCompanionTargets({ table: 'error_events', column: 'title', path: '' });
      const pairs = out.map((t) => `${t.table}.${t.column}`);
      expect(pairs).toContain('error_events.title');
      expect(pairs).toContain('issues.title');
      expect(pairs).toContain('error_events.exception_value');
      expect(pairs).toContain('error_events.exception_type');
      expect(pairs).toContain('error_events.message');
    });

    // The path is relative to the COLUMN and `error_events.stacktrace` is an
    // array at its root, so the wildcard is bare — same convention as
    // `parse_mask_path` in Task 11 and `apply_mask_path` in Task 14.
    it('expands stacktrace to its symbolicated copy, keeping the path', () => {
      const out = expandCompanionTargets({
        table: 'error_events',
        column: 'stacktrace',
        path: '[*].abs_path',
      });
      expect(out).toContainEqual({
        table: 'error_events',
        column: 'stacktrace_symbolicated',
        path: '[*].abs_path',
      });
    });

    it('expands context to sessions.context for both event tables', () => {
      for (const table of ['error_events', 'analytics_events'] as const) {
        const out = expandCompanionTargets({ table, column: 'context', path: 'user.email' });
        expect(out).toContainEqual({ table: 'sessions', column: 'context', path: 'user.email' });
      }
    });

    it('expands everything else to itself', () => {
      const one = { table: 'error_events', column: 'extra', path: 'a.b' } as const;
      expect(expandCompanionTargets(one)).toEqual([one]);
    });
  });

  describe('maskConfirmReady', () => {
    const preview = { status: 'previewed', previewed_at: new Date().toISOString(), estimated_rows: 10 };

    it('is false for the wrong slug', () => {
      expect(maskConfirmReady('wrong', 'my-app-a1b2', preview, 900, 20000000)).toBe(false);
    });

    it('is true for the right slug on a fresh preview', () => {
      expect(maskConfirmReady('my-app-a1b2', 'my-app-a1b2', preview, 900, 20000000)).toBe(true);
    });

    it('trims whitespace but not case', () => {
      expect(maskConfirmReady('  my-app-a1b2 ', 'my-app-a1b2', preview, 900, 20000000)).toBe(true);
      expect(maskConfirmReady('MY-APP-A1B2', 'my-app-a1b2', preview, 900, 20000000)).toBe(false);
    });

    it('is false while the preview is still counting', () => {
      expect(
        maskConfirmReady('my-app-a1b2', 'my-app-a1b2', { status: 'preview', previewed_at: null, estimated_rows: 0 }, 900, 20000000),
      ).toBe(false);
    });

    it('is false once the preview is stale', () => {
      const old = { ...preview, previewed_at: new Date(Date.now() - 3600_000).toISOString() };
      expect(maskConfirmReady('my-app-a1b2', 'my-app-a1b2', old, 900, 20000000)).toBe(false);
    });

    it('is false above the row ceiling', () => {
      expect(
        maskConfirmReady('my-app-a1b2', 'my-app-a1b2', { ...preview, estimated_rows: 99 }, 900, 10),
      ).toBe(false);
    });
  });

  describe('csvFilename', () => {
    it('is stable and carries the scope and range', () => {
      expect(csvFilename('findings', 'my-app', '2026-07-01', '2026-08-01')).toBe(
        'sauron-inspector-findings_my-app_2026-07-01_2026-08-01.csv',
      );
    });
  });
  ```

- [ ] **Step 2: Run and see it fail.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test -- inspector`. Expected: `Failed to resolve import "./inspector"`.

- [ ] **Step 3: Implement `inspector.ts`.** Create `dashboard/src/lib/models/inspector.ts`:
  ```ts
  // Pure decision logic for the PII inspector. No Svelte, no DOM — vitest is
  // node-only in this repo, so anything that needs a test lives here.

  export interface MaskTargetView {
    table: string;
    column: string;
    path: string;
  }

  export interface UnreachableRow {
    /** The first entry is the headline, rendered above the enumerated rows. */
    headline?: boolean;
    /** Rendered in bold before confirm is enabled. */
    readFirst?: boolean;
    what: string;
    why: string;
    bounded: string;
  }

  /**
   * What "permanently masked" does NOT mean.
   *
   * ONE data array, rendered verbatim in the MaskDialog, in the Audit tab detail
   * and in the wiki, so support answers and the product cannot diverge. The
   * product must never claim a mask is permanent: in twelve named places the
   * promise does not hold — eleven where the bytes survive, and one where the
   * mask silently takes something else away with it.
   */
  export const UNREACHABLE_COPY: UnreachableRow[] = [
    {
      headline: true,
      what: 'Masking rewrites rows in hot Postgres only.',
      why: 'Everything below still holds the original bytes, or is outside this product’s reach.',
      bounded: 'Read the rows below before confirming.',
    },
    {
      what: 'Cold Parquet',
      why: 'The partition was exported before the mask ran. Parquet is immutable and, after the drop, the only copy.',
      bounded: 'Nothing. Permanent.',
    },
    {
      what: 'Postgres rows older than TIER_HOT_DAYS',
      why: 'The retro-mask deliberately stops at the hot boundary.',
      bounded: 'The tier drop, which destroys the row entirely.',
    },
    {
      what: 'The Redis ingest stream',
      why: 'sauron:ingest:stream holds the full serialized job.',
      bounded: 'XADD … MAXLEN ~ 1000000.',
    },
    {
      what: 'The Redis DLQ',
      why: 'sauron:ingest:dlq is XADD with no MAXLEN and no TTL, and no reaper exists. A payload that fails to deserialize still dead-letters raw.',
      bounded: 'Nothing. Permanent.',
    },
    {
      what: 'Per-person breadcrumbs in Redis',
      why: 'Up to 100 batches are buffered per person before an error arrives.',
      bounded: 'A 1800 s TTL.',
    },
    {
      what: 'alert_events.title / .body',
      why: 'They embed the issue title verbatim.',
      bounded: 'ALERT_EVENT_RETENTION_DAYS (90).',
    },
    {
      what: 'Already-delivered alerts',
      why: 'Email, Slack, Discord, Matrix, Telegram and webhook messages are gone from our control the moment they send.',
      bounded: 'Nothing.',
    },
    {
      what: 'event_users.properties',
      why: 'The identify() write merges with ||, which never removes keys. An at-rest mask is undone by the next identify().',
      bounded: 'Forward enforcement only, and only for keys in the mask set.',
    },
    {
      what: 'devices.*',
      why: 'Every column is COALESCE(EXCLUDED.x, devices.x) — a non-null incoming value always wins, and there is no wire field to enforce on.',
      bounded: 'Not offered: devices is not maskable at all.',
    },
    {
      what: 'Symbolicated source lines',
      why: 'Frames carry context_line / pre_context / post_context — verbatim customer source. Masking a JSON path never touches them.',
      bounded: 'Redacted from responses only, for callers without source:read.',
    },
    {
      what: 'Backups, WAL, replicas',
      why: 'Out of the product’s reach entirely.',
      bounded: 'Operator policy.',
    },
    {
      readFirst: true,
      what: 'The active-users report stops identifying anyone new through that key',
      why: 'The enforcer runs before the active-users pipeline stamps identified_at, so masking a key an app sends as context.user.id means the equality test never passes again. Nobody already stamped is un-identified, but everyone first seen afterwards arrives as a guest and never merges across apps, so the identified share decays with no discontinuity to notice.',
      bounded: 'Nothing. The bytes are gone, so it cannot be recomputed later.',
    },
  ];

  export function describeTarget(t: MaskTargetView): string {
    return `${t.table}.${t.column} → ${t.path === '' ? 'the whole value' : t.path}`;
  }

  /**
   * Mirrors the backend's `expand_targets` so the dialog can describe the blast
   * radius before the server answers. The backend map is authoritative;
   * `inspector.test.ts` and the Rust `targets.rs` tests assert the same pairs.
   */
  export function expandCompanionTargets(t: MaskTargetView): MaskTargetView[] {
    const out: MaskTargetView[] = [{ ...t }];
    const push = (m: MaskTargetView) => {
      if (!out.some((x) => x.table === m.table && x.column === m.column && x.path === m.path)) {
        out.push(m);
      }
    };
    if (t.table === 'error_events' && t.column === 'title') {
      // error_events.title is derived server-side and has NO wire field, so
      // forward enforcement reaches it only through its inputs.
      push({ table: 'issues', column: 'title', path: '' });
      push({ table: 'error_events', column: 'exception_value', path: '' });
      push({ table: 'error_events', column: 'exception_type', path: '' });
      push({ table: 'error_events', column: 'message', path: '' });
    } else if (t.table === 'error_events' && t.column === 'culprit') {
      push({ table: 'issues', column: 'culprit', path: '' });
    } else if (t.table === 'error_events' && t.column === 'stacktrace') {
      push({ table: 'error_events', column: 'stacktrace_symbolicated', path: t.path });
    } else if (
      (t.table === 'error_events' || t.table === 'analytics_events') &&
      t.column === 'context'
    ) {
      // bump_session snapshots the same enriched jsonb on every event.
      push({ table: 'sessions', column: 'context', path: t.path });
    }
    return out;
  }

  export interface PreviewState {
    status: string;
    previewed_at: string | null;
    estimated_rows: number;
  }

  /**
   * Whether the danger button may be enabled.
   *
   * Typing the SLUG is the only confirmation that forces attention onto the
   * thing that actually goes wrong: the realistic failure is masking the WRONG
   * APP, not a mis-click. Case-sensitive, whitespace-trimmed.
   */
  export function maskConfirmReady(
    typed: string,
    slug: string,
    preview: PreviewState,
    ttlSecs: number,
    maxRows: number,
  ): boolean {
    if (typed.trim() !== slug) return false;
    if (preview.status !== 'previewed' || !preview.previewed_at) return false;
    // The TTL runs from the preview COMPLETING, not from the request, or a
    // queued preview expires before it is readable.
    const ageSecs = (Date.now() - Date.parse(preview.previewed_at)) / 1000;
    if (!Number.isFinite(ageSecs) || ageSecs > ttlSecs) return false;
    return preview.estimated_rows <= maxRows;
  }

  export function csvFilename(
    kind: 'findings' | 'mask-actions',
    scope: string,
    from: string,
    to: string,
  ): string {
    return `sauron-inspector-${kind}_${scope}_${from}_${to}.csv`;
  }
  ```

- [ ] **Step 4: Run and see it pass.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test -- inspector`. All of `inspector.test.ts` green.

- [ ] **Step 5: Write the failing `inspector-schedule.test.ts`.** Create `dashboard/src/lib/models/inspector-schedule.test.ts`:
  ```ts
  import { describe, it, expect } from 'vitest';
  import {
    weekdayMaskToArray,
    weekdayArrayToMask,
    describeSchedule,
    nextRuns,
  } from './inspector-schedule';

  describe('weekday bitmask', () => {
    // Bit N = EXTRACT(DOW) = N, so SUNDAY IS BIT 0. Getting this backwards
    // shifts every schedule by a day and nobody notices for a week.
    it('maps bit 0 to Sunday', () => {
      expect(weekdayMaskToArray(1)).toEqual([true, false, false, false, false, false, false]);
    });

    it('round-trips every mask', () => {
      for (let m = 0; m <= 127; m += 1) {
        expect(weekdayArrayToMask(weekdayMaskToArray(m))).toBe(m);
      }
    });

    it('maps 127 to every day', () => {
      expect(weekdayMaskToArray(127).every(Boolean)).toBe(true);
    });
  });

  describe('describeSchedule', () => {
    it('names the days, the time and the zone', () => {
      expect(describeSchedule(0b0010100, '03:00', 'Europe/Paris')).toBe(
        'Every Tue, Thu at 03:00 (Europe/Paris)',
      );
    });

    it('says daily when every bit is set', () => {
      expect(describeSchedule(127, '03:00', 'UTC')).toBe('Every day at 03:00 (UTC)');
    });

    it('says never when no bit is set', () => {
      expect(describeSchedule(0, '03:00', 'UTC')).toBe('No scheduled runs');
    });
  });

  describe('nextRuns', () => {
    it('returns three future instants on set days only', () => {
      // Sunday only.
      const runs = nextRuns(1, '03:00', 'UTC', new Date('2026-08-01T00:00:00Z'));
      expect(runs).toHaveLength(3);
      for (const r of runs) {
        expect(r.getTime()).toBeGreaterThan(Date.parse('2026-08-01T00:00:00Z'));
        expect(r.getUTCDay()).toBe(0);
      }
      expect(runs[0].getTime()).toBeLessThan(runs[1].getTime());
    });

    it('returns nothing when no day is selected', () => {
      expect(nextRuns(0, '03:00', 'UTC', new Date())).toEqual([]);
    });
  });
  ```

- [ ] **Step 6: Implement the constants and the schedule model.** Create `dashboard/src/lib/constants/inspectorSchedules.ts`:
  ```ts
  // Single source of truth for the schedule vocabulary the Policy tab renders.
  // Mirrors the backend's `NEXT_RUN_SQL` in
  // backend/crates/sauron-db/src/repo.rs, which computes the due instant with
  // `(schedule_days >> EXTRACT(DOW FROM ts)) & 1` — so BIT 0 IS SUNDAY, exactly
  // as Postgres numbers DOW. Keep the two in sync.

  export const WEEKDAYS: string[] = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

  /**
   * A short list of IANA zones for the picker. Any zone Postgres accepts is
   * valid — the API validates with `SELECT now() AT TIME ZONE $1` and answers
   * 400 — so this is a convenience list, not a whitelist.
   */
  export const COMMON_TIMEZONES: string[] = [
    'UTC',
    'Europe/London',
    'Europe/Paris',
    'Europe/Berlin',
    'Africa/Algiers',
    'Africa/Cairo',
    'Asia/Dubai',
    'Asia/Kolkata',
    'Asia/Singapore',
    'Asia/Tokyo',
    'Australia/Sydney',
    'America/New_York',
    'America/Chicago',
    'America/Denver',
    'America/Los_Angeles',
    'America/Sao_Paulo',
  ];

  /**
   * Local times the UI warns about. On spring-forward a 02:30 schedule resolves
   * to a valid instant (effectively 03:30 local); on fall-back it resolves to
   * the first occurrence, so it runs once, not twice. Never zero runs, never
   * double runs — but an operator picking 02:30 should know that.
   */
  export const DST_RISK_HOURS: number[] = [0, 1, 2, 3];
  ```
  Then create `dashboard/src/lib/models/inspector-schedule.ts`:
  ```ts
  import { WEEKDAYS } from '../constants/inspectorSchedules';

  // The SERVER's `next_run_at` is authoritative. Everything here is DISPLAY
  // ONLY: the backend resolves DST with Postgres's `AT TIME ZONE`, and this
  // module cannot, so a preview that disagrees by an hour on a transition day is
  // expected and is not a bug to chase.

  /** Bit N = day N, Sunday first, matching Postgres's EXTRACT(DOW). */
  export function weekdayMaskToArray(mask: number): boolean[] {
    return WEEKDAYS.map((_, i) => ((mask >> i) & 1) === 1);
  }

  export function weekdayArrayToMask(days: boolean[]): number {
    return days.reduce((acc, on, i) => (on ? acc | (1 << i) : acc), 0);
  }

  export function describeSchedule(mask: number, time: string, tz: string): string {
    if (mask === 0) return 'No scheduled runs';
    if (mask === 127) return `Every day at ${time} (${tz})`;
    const names = weekdayMaskToArray(mask)
      .map((on, i) => (on ? WEEKDAYS[i] : null))
      .filter((n): n is string => n !== null);
    return `Every ${names.join(', ')} at ${time} (${tz})`;
  }

  /**
   * The next three instants, for a preview under the picker.
   *
   * Computed with `Intl.DateTimeFormat` rather than a tz library — the
   * dashboard has no date library and this is display only.
   */
  export function nextRuns(mask: number, time: string, tz: string, now: Date = new Date()): Date[] {
    if (mask === 0) return [];
    const [hh, mm] = time.split(':').map((n) => Number.parseInt(n, 10));
    const out: Date[] = [];
    // 21 days, not 14. A weekly (single-bit) schedule has to yield THREE
    // candidates, and three weekly runs span up to 21 days from an arbitrary
    // starting weekday — from a Saturday, the third Sunday is offset 15. A
    // 14-day bound silently returns two, and the preview under the picker
    // quietly shows one fewer run than it promises.
    for (let offset = 0; offset <= 21 && out.length < 3; offset += 1) {
      const day = new Date(now.getTime() + offset * 86400_000);
      const candidate = new Date(
        Date.UTC(day.getUTCFullYear(), day.getUTCMonth(), day.getUTCDate(), hh, mm, 0),
      );
      // Deliberately UTC day-of-week: the server resolves the real local
      // weekday with Postgres's AT TIME ZONE, and duplicating that here without
      // a tz library would produce a preview that is confidently wrong near
      // midnight. The Policy tab labels this list "approximate — the server
      // decides", and `tz` is carried only so that label can name the zone.
      const dow = candidate.getUTCDay();
      if (((mask >> dow) & 1) === 1 && candidate.getTime() > now.getTime()) {
        out.push(candidate);
      }
    }
    return out;
  }
  ```
  `tz` is now unused inside the function body; keep the parameter (every call site passes the policy's zone and the signature is mirrored in the Policy tab) and silence the lint with a leading underscore only if `npm run check` complains.

- [ ] **Step 7: Run and see the schedule tests pass.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test -- inspector-schedule`. All green.

- [ ] **Step 8: Write the failing `inspector-findings.test.ts`.** Create `dashboard/src/lib/models/inspector-findings.test.ts`:
  ```ts
  import { describe, it, expect } from 'vitest';
  import { groupFindings, formatMatchCount, findingBadges } from './inspector-findings';

  const base = {
    id: 'f1',
    app_id: 'a1',
    environment_id: null,
    env_scope: 'no_env_column',
    source_table: 'issues',
    source_column: 'title',
    key_path: '',
    matched_key: 'email',
    detector: '',
    value_type: 'string',
    match_count: 3,
    match_count_exact: true,
    sample_preview: 'j…m',
    partition_kind: 'rollup',
    last_seen_at: '2026-08-01T00:00:00Z',
  };

  describe('formatMatchCount', () => {
    it('is exact when the scan was not truncated', () => {
      expect(formatMatchCount(41200, true)).toBe('41,200');
    });

    // Hitting INSPECTOR_MAX_PHASE2_ROWS_PER_UNIT makes every count a LOWER
    // BOUND. Rendering it as an exact number would be a quiet lie.
    it('says at least when the unit was truncated', () => {
      expect(formatMatchCount(200000, false)).toBe('at least 200,000');
    });
  });

  describe('findingBadges', () => {
    it('marks a rollup as recurring', () => {
      const b = findingBadges(base);
      expect(b.map((x) => x.label)).toContain('recurring');
      expect(b.find((x) => x.label === 'recurring')?.title).toMatch(/undone by the next event/i);
    });

    it('marks a default-partition finding as never ageing out', () => {
      const b = findingBadges({ ...base, partition_kind: 'default' });
      expect(b.map((x) => x.label)).toContain('never ages out');
    });

    it('marks a non-maskable table', () => {
      for (const table of ['devices', 'identities', 'workflows']) {
        const b = findingBadges({ ...base, source_table: table });
        expect(b.map((x) => x.label)).toContain('not maskable');
      }
    });

    it('distinguishes unattributed from no environment column', () => {
      expect(findingBadges({ ...base, env_scope: 'unattributed' }).map((x) => x.label)).toContain(
        'no environment',
      );
      expect(findingBadges({ ...base, env_scope: 'no_env_column' }).map((x) => x.label)).toContain(
        'app-wide table',
      );
    });
  });

  describe('groupFindings', () => {
    it('groups by table then column and sorts by match count', () => {
      const rows = [
        { ...base, id: 'a', source_table: 'error_events', source_column: 'extra', match_count: 1 },
        { ...base, id: 'b', source_table: 'error_events', source_column: 'extra', match_count: 9 },
        { ...base, id: 'c', source_table: 'issues', source_column: 'title', match_count: 5 },
      ];
      const groups = groupFindings(rows);
      expect(groups.map((g) => g.key)).toEqual(['error_events.extra', 'issues.title']);
      expect(groups[0].findings.map((f) => f.id)).toEqual(['b', 'a']);
      expect(groups[0].total).toBe(10);
    });
  });
  ```

- [ ] **Step 9: Implement `inspector-findings.ts`.** Create `dashboard/src/lib/models/inspector-findings.ts`:
  ```ts
  // Grouping, count rendering and badge logic for the Findings tab. Pure.

  export interface FindingView {
    id: string;
    app_id: string;
    environment_id: string | null;
    env_scope: string;
    source_table: string;
    source_column: string;
    key_path: string;
    matched_key: string;
    detector: string;
    value_type: string;
    match_count: number;
    match_count_exact: boolean;
    sample_preview: string;
    partition_kind: string;
    last_seen_at: string | null;
  }

  export interface FindingBadge {
    label: string;
    /** The tooltip. Every badge explains a consequence, not a category. */
    title: string;
  }

  /** Tables a scan reaches but a mask is never offered for. */
  const SCAN_ONLY = new Set(['devices', 'identities', 'workflows']);

  export function formatMatchCount(n: number, exact: boolean): string {
    const s = n.toLocaleString();
    // A truncated unit makes every count a LOWER BOUND; rendering it as an
    // exact number would be a quiet lie on a privacy report.
    return exact ? s : `at least ${s}`;
  }

  export function findingBadges(f: FindingView): FindingBadge[] {
    const out: FindingBadge[] = [];
    if (f.partition_kind === 'rollup') {
      out.push({
        label: 'recurring',
        title: 'This row is rewritten by every matching event, so an at-rest mask will be undone by the next event. Forward enforcement is what covers it.',
      });
    }
    if (f.partition_kind === 'default') {
      out.push({
        label: 'never ages out',
        title: 'This row lives in the default partition, which is never exported to cold storage and never dropped. It is the longest-lived copy in the system.',
      });
    }
    if (SCAN_ONLY.has(f.source_table)) {
      out.push({
        label: 'not maskable',
        title:
          f.source_table === 'devices'
            ? 'Every devices column is COALESCE(EXCLUDED.x, devices.x), so a mask would report success and be overwritten by the next event from that device.'
            : f.source_table === 'identities'
              ? 'alias_id and distinct_id ARE the identity graph. Masking them merges every masked person into one rather than redacting anyone.'
              : 'cancel_reason is derived server-side from an analytics event; mask analytics_events.properties instead, which is where the bytes arrive.',
      });
    }
    if (f.env_scope === 'unattributed') {
      out.push({
        label: 'no environment',
        title: 'The platform could not attribute this row to an environment.',
      });
    }
    if (f.env_scope === 'no_env_column') {
      out.push({
        label: 'app-wide table',
        title: 'This table has no environment column at all, so the finding covers the whole app.',
      });
    }
    if (f.detector !== '') {
      out.push({ label: f.detector, title: 'Matched by value shape, not by key name.' });
    }
    return out;
  }

  export interface FindingGroup {
    key: string;
    table: string;
    column: string;
    total: number;
    findings: FindingView[];
  }

  export function groupFindings(rows: FindingView[]): FindingGroup[] {
    const byKey = new Map<string, FindingGroup>();
    for (const f of rows) {
      const key = `${f.source_table}.${f.source_column}`;
      let g = byKey.get(key);
      if (!g) {
        g = { key, table: f.source_table, column: f.source_column, total: 0, findings: [] };
        byKey.set(key, g);
      }
      g.findings.push(f);
      g.total += f.match_count;
    }
    const groups = [...byKey.values()];
    for (const g of groups) {
      g.findings.sort((a, b) => b.match_count - a.match_count || a.key_path.localeCompare(b.key_path));
    }
    groups.sort((a, b) => b.total - a.total || a.key.localeCompare(b.key));
    return groups;
  }
  ```

- [ ] **Step 10: Run the whole dashboard suite and typecheck.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test` then `npm run check`. Both green. Note `groupFindings`' sort is by total descending, so the `error_events.extra` group (10) precedes `issues.title` (5) as the test asserts.

---

## Task 31: Dashboard API client and response types

**Files:**
- Create `dashboard/src/lib/api/inspector.ts`
- Modify `dashboard/src/lib/models/index.ts` (append the inspector response types)

**Interfaces:**
- Consumes: `{ api }` from `./client`, `downloadBlob` from `./download.ts` (S4), the routes from Tasks 26–29.
- Produces: `listPolicies`, `createPolicy`, `getPolicy`, `patchPolicy`, `deletePolicy`, `effectivePolicy`, `listScans`, `startScan`, `getScan`, `cancelScan`, `listFindings`, `downloadFindingsCsv`, `revealFinding`, `maskPreview`, `getMaskAction`, `confirmMask`, `cancelMask`, `listAppMaskActions`, `listOrgMaskActions`, `downloadMaskActionsCsv`, `listMaskedKeys`; the types `InspectorPolicy`, `InspectorScan`, `InspectorFinding`, `InspectorMaskAction`, `InspectorMaskedKey`, `EffectivePolicy`, `FindingsPage`, `RevealResult`.

- [ ] **Step 1: Add the response types.** Append to `dashboard/src/lib/models/index.ts`:
  ```ts
  // ---------------------------------------------------------------------------
  // PII inspector
  // ---------------------------------------------------------------------------

  export interface InspectorTrackedKey {
    key: string;
    scope: 'any' | 'top';
  }

  export interface InspectorPolicy {
    id: string;
    org_id: string;
    target_type: 'project' | 'app' | 'app_env';
    target_id: string;
    enabled: boolean;
    tracked_keys: InspectorTrackedKey[];
    detectors: string[];
    scan_columns: string[] | null;
    rollups: string[];
    window_days: number;
    schedule_enabled: boolean;
    /** 7-bit weekday mask; bit 0 is Sunday, matching Postgres's EXTRACT(DOW). */
    schedule_days: number;
    /** `HH:MM` local wall clock. */
    schedule_time: string;
    schedule_tz: string;
    next_run_at: string | null;
    last_run_at: string | null;
    last_scan_id: string | null;
    last_skip_reason: string;
    created_at: string;
    updated_at: string;
  }

  export interface InspectorScan {
    id: string;
    policy_id: string;
    org_id: string;
    trigger_type: 'scheduled' | 'manual';
    status: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled';
    coverage: 'full' | 'partial';
    coverage_note: string;
    window_from: string;
    window_to: string;
    units_total: number;
    units_done: number;
    rows_scanned: number;
    findings_count: number;
    findings_reaped_at: string | null;
    attempts: number;
    cancel_requested_at: string | null;
    error: string;
    started_at: string | null;
    finished_at: string | null;
    created_at: string;
  }

  export interface InspectorFinding {
    id: string;
    scan_id: string;
    org_id: string;
    app_id: string;
    environment_id: string | null;
    env_scope: 'enrollment' | 'unattributed' | 'no_env_column';
    source_table: string;
    source_column: string;
    key_path: string;
    matched_key: string;
    detector: string;
    value_type: string;
    match_count: number;
    match_count_exact: boolean;
    /** Shape-only. NEVER the value — the findings table has no value column. */
    sample_preview: string;
    sample_row_id: string | null;
    sample_occurred_at: string | null;
    partition_kind: 'ranged' | 'default' | 'rollup';
    first_seen_at: string | null;
    last_seen_at: string | null;
    created_at: string;
  }

  export interface InspectorMaskAction {
    id: string;
    org_id: string;
    app_id: string;
    kind: 'preview' | 'mask';
    finding_id: string | null;
    scan_id: string | null;
    targets: { table: string; column: string; path: string }[];
    status:
      | 'preview'
      | 'previewed'
      | 'pending'
      | 'running'
      | 'cancelling'
      | 'done'
      | 'failed'
      | 'cancelled';
    requested_by_email: string;
    cancelled_by_email: string;
    cancelled_at: string | null;
    requested_at: string;
    previewed_at: string | null;
    confirmed_at: string | null;
    started_at: string | null;
    finished_at: string | null;
    confirm_source: string;
    estimated_rows: number;
    rows_scanned: number;
    rows_masked: number;
    cold_rows_skipped: number;
    cold_boundary_at: string | null;
    phase: string;
    vacuum_advised: boolean;
    error: string;
  }

  export interface InspectorMaskedKey {
    id: string;
    app_id: string;
    target_table: string;
    target_column: string;
    json_path: string;
    created_at: string;
    source_action_id: string | null;
  }

  export interface EffectivePolicy {
    policy: InspectorPolicy | null;
    masked_keys: InspectorMaskedKey[];
    /** Read from the server, never hardcoded — the UI states this number. */
    enforcement_latency_secs: number;
    hot_window_days: number;
  }

  export interface FindingsPage {
    findings: InspectorFinding[];
    coverage: 'full' | 'partial';
    coverage_note: string;
    detection_caveat: string;
  }

  export interface RevealResult {
    path: string;
    value: unknown;
    type: string;
  }

  export interface MaskPreviewStart {
    action: InspectorMaskAction;
    app_slug: string;
    preview_ttl_secs: number;
    mask_max_rows: number;
    enforcement_latency_secs: number;
  }
  ```

- [ ] **Step 2: Implement the client.** Create `dashboard/src/lib/api/inspector.ts`:
  ```ts
  // One exported async fn per inspector endpoint.
  //
  // Imports ONLY `{ api }` from ./client, so the bearer header and the
  // single-flight 401 refresh-and-replay apply. Request-body interfaces live
  // here; response types live in models/index.ts.

  import { api } from './client';
  import { downloadBlob } from './download';
  import type {
    EffectivePolicy,
    FindingsPage,
    InspectorMaskAction,
    InspectorMaskedKey,
    InspectorPolicy,
    InspectorScan,
    MaskPreviewStart,
    RevealResult,
  } from '../models';

  export interface CreatePolicyBody {
    target_type: 'project' | 'app' | 'app_env';
    target_id: string;
    tracked_keys?: { key: string; scope: 'any' | 'top' }[];
    detectors?: string[];
    scan_columns?: string[] | null;
    rollups?: string[];
    window_days?: number;
    schedule_enabled?: boolean;
    schedule_days?: number;
    schedule_time?: string;
    schedule_tz?: string;
  }

  export type PatchPolicyBody = Partial<CreatePolicyBody> & { enabled?: boolean };

  export async function listPolicies(orgId: string): Promise<InspectorPolicy[]> {
    const { data } = await api.get<InspectorPolicy[]>(`/v1/orgs/${orgId}/inspector/policies`);
    return data;
  }

  export async function createPolicy(orgId: string, body: CreatePolicyBody): Promise<InspectorPolicy> {
    const { data } = await api.post<InspectorPolicy>(`/v1/orgs/${orgId}/inspector/policies`, body);
    return data;
  }

  export async function getPolicy(policyId: string): Promise<InspectorPolicy> {
    const { data } = await api.get<InspectorPolicy>(`/v1/inspector/policies/${policyId}`);
    return data;
  }

  export async function patchPolicy(policyId: string, body: PatchPolicyBody): Promise<InspectorPolicy> {
    const { data } = await api.patch<InspectorPolicy>(`/v1/inspector/policies/${policyId}`, body);
    return data;
  }

  export async function deletePolicy(policyId: string): Promise<void> {
    await api.delete(`/v1/inspector/policies/${policyId}`);
  }

  export async function effectivePolicy(appId: string): Promise<EffectivePolicy> {
    const { data } = await api.get<EffectivePolicy>(`/v1/apps/${appId}/inspector/policy`);
    return data;
  }

  export async function listScans(policyId: string, limit = 20): Promise<InspectorScan[]> {
    const { data } = await api.get<InspectorScan[]>(`/v1/inspector/policies/${policyId}/scans`, {
      params: { limit },
    });
    return data;
  }

  export async function startScan(policyId: string): Promise<InspectorScan> {
    const { data } = await api.post<InspectorScan>(`/v1/inspector/policies/${policyId}/scans`);
    return data;
  }

  export async function getScan(scanId: string): Promise<InspectorScan> {
    const { data } = await api.get<InspectorScan>(`/v1/inspector/scans/${scanId}`);
    return data;
  }

  export async function cancelScan(scanId: string): Promise<void> {
    await api.post(`/v1/inspector/scans/${scanId}/cancel`);
  }

  export async function listFindings(
    scanId: string,
    opts: { limit?: number; afterCount?: number; afterId?: string } = {},
  ): Promise<FindingsPage> {
    const { data } = await api.get<FindingsPage>(`/v1/inspector/scans/${scanId}/findings`, {
      params: {
        limit: opts.limit ?? 100,
        after_count: opts.afterCount,
        after_id: opts.afterId,
      },
    });
    return data;
  }

  /**
   * Buffered CSV. Goes through `downloadBlob`, which uses the shared `api`
   * instance so refresh-and-replay still works and reads the blob back as text
   * on a non-2xx — `normalizeError` reads `error.response.data` as an
   * `{error:{code,message}}` envelope, and with `responseType: 'blob'` that data
   * IS a Blob and the message is lost.
   */
  export async function downloadFindingsCsv(scanId: string, filename: string): Promise<void> {
    await downloadBlob(`/v1/inspector/scans/${scanId}/findings`, { format: 'csv' }, filename);
  }

  export async function revealFinding(findingId: string): Promise<RevealResult> {
    const { data } = await api.post<RevealResult>(`/v1/inspector/findings/${findingId}/reveal`, {});
    return data;
  }

  export async function maskPreview(
    appId: string,
    body: { finding_id?: string; targets?: { table: string; column: string; path: string }[] },
  ): Promise<MaskPreviewStart> {
    const { data } = await api.post<MaskPreviewStart>(
      `/v1/apps/${appId}/inspector/mask-preview`,
      body,
    );
    return data;
  }

  export async function getMaskAction(actionId: string): Promise<InspectorMaskAction> {
    const { data } = await api.get<InspectorMaskAction>(`/v1/inspector/mask-actions/${actionId}`);
    return data;
  }

  export async function confirmMask(
    actionId: string,
    confirmText: string,
  ): Promise<{ action: InspectorMaskAction; enforcement_latency_secs: number }> {
    const { data } = await api.post(`/v1/inspector/mask-actions/${actionId}/confirm`, {
      confirm_text: confirmText,
    });
    return data;
  }

  export async function cancelMask(actionId: string): Promise<InspectorMaskAction> {
    const { data } = await api.post<InspectorMaskAction>(
      `/v1/inspector/mask-actions/${actionId}/cancel`,
    );
    return data;
  }

  export async function listAppMaskActions(appId: string, limit = 100): Promise<InspectorMaskAction[]> {
    const { data } = await api.get<InspectorMaskAction[]>(
      `/v1/apps/${appId}/inspector/mask-actions`,
      { params: { limit } },
    );
    return data;
  }

  export async function listOrgMaskActions(orgId: string, limit = 100): Promise<InspectorMaskAction[]> {
    const { data } = await api.get<InspectorMaskAction[]>(
      `/v1/orgs/${orgId}/inspector/mask-actions`,
      { params: { limit } },
    );
    return data;
  }

  export async function downloadMaskActionsCsv(appId: string, filename: string): Promise<void> {
    await downloadBlob(`/v1/apps/${appId}/inspector/mask-actions`, { format: 'csv' }, filename);
  }

  export async function listMaskedKeys(
    appId: string,
  ): Promise<{ masked_keys: InspectorMaskedKey[]; enforcement_latency_secs: number }> {
    const { data } = await api.get(`/v1/apps/${appId}/inspector/masked-keys`);
    return data;
  }
  ```

- [ ] **Step 3: Match `downloadBlob`'s real signature.** Run `cd /home/splimter/projects/freelance/sauron/dashboard && cat src/lib/api/download.ts`. Adjust the two `downloadBlob(...)` call sites to whatever it actually exports — **do not** write a second download helper, and do not bypass the `api` instance.

- [ ] **Step 4: Typecheck.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`. Clean.

- [ ] **Step 5: Run the whole dashboard suite.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`. Green — no test imports this module, but a syntax error here fails the run.

---

## Task 32: `Inspector.svelte` — the four-tab page, plus the three mandatory wiring edits

**Files:**
- Create `dashboard/src/pages/Inspector.svelte`
- Modify `dashboard/src/routes.ts`
- Modify `dashboard/src/lib/components/layout/Sidebar.svelte`
- Modify `dashboard/src/lib/components/ui/Icon.svelte`

**Interfaces:**
- Consumes: `lib/api/inspector.ts` (Task 31), `lib/models/{inspector,inspector-schedule,inspector-findings}.ts` (Task 30), `lib/constants/inspectorSchedules.ts`.
- Produces: the `#/inspector` route; the `shield-alert` and `eye-off` icon registry entries.

> House UI components only. There is **no Tabs primitive** and introducing one is out of scope, so the tabs are hand-rolled copying `Alerts.svelte` verbatim (`<nav class="tabs"><button class="tab" class:active={…}>`). There is **no Toggle**, so enabled state is a `Button` that flips a boolean plus a status `Badge`. There is **no Select**, so the timezone picker is a raw `<select class="sel">`.

- [ ] **Step 1: Register the two icons.** In `dashboard/src/lib/components/ui/Icon.svelte`, add to the import block (alphabetical position) `import EyeOff from '@lucide/svelte/icons/eye-off';` and `import ShieldAlert from '@lucide/svelte/icons/shield-alert';`, then add `'eye-off': EyeOff,` and `'shield-alert': ShieldAlert,` to `iconRegistry`. Nothing else in the dashboard imports from `@lucide/svelte` directly, so a component that wants an icon must go through this registry.

- [ ] **Step 2: Add the route.** In `dashboard/src/routes.ts`, add `import Inspector from './pages/Inspector.svelte';` beside the other page imports, and under the admin section of `routes` add:
  ```ts
  '/inspector': guarded(Inspector as Component<never>),
  ```

- [ ] **Step 3: Add the nav item.** In `dashboard/src/lib/components/layout/Sidebar.svelte`, in the `Manage` group's `items` array after the Storage entry:
  ```ts
  { href: '#/inspector', label: 'Privacy', icon: 'shield-alert', match: (p) => p.startsWith('/inspector'), show: () => sessionStore.can('pii:read') },
  ```
  Per the Storage precedent this `show` is **cosmetic** — the endpoint's 403 is the real gate, and `#/inspector` is reachable by typing it.

- [ ] **Step 4: Create the page shell with the four tabs.** Create `dashboard/src/pages/Inspector.svelte`:
  ```svelte
  <!--
    PII inspector. Four hand-rolled tabs (there is no Tabs primitive and adding
    one is out of scope): Findings / Policy / Scans / Audit.

    Every destructive control is gated on `pii:manage` AT THE CURRENT APP, and
    the sidebar entry's `show` is cosmetic — the endpoint's 403 is the real gate.
  -->
  <script lang="ts">
    import AppShell from '../lib/components/layout/AppShell.svelte';
    import Card from '../lib/components/ui/Card.svelte';
    import Button from '../lib/components/ui/Button.svelte';
    import Badge from '../lib/components/ui/Badge.svelte';
    import Input from '../lib/components/ui/Input.svelte';
    import Spinner from '../lib/components/ui/Spinner.svelte';
    import EmptyState from '../lib/components/ui/EmptyState.svelte';
    import Icon from '../lib/components/ui/Icon.svelte';
    import DataTable from '../lib/components/DataTable.svelte';
    import JsonTree from '../lib/components/JsonTree.svelte';
    import MaskDialog from '../lib/components/inspector/MaskDialog.svelte';
    import { sessionStore } from '../lib/stores/session.svelte';
    import { toastStore } from '../lib/stores/toast.svelte';
    import * as inspectorApi from '../lib/api/inspector';
    import { WEEKDAYS, COMMON_TIMEZONES, DST_RISK_HOURS } from '../lib/constants/inspectorSchedules';
    import {
      weekdayMaskToArray,
      weekdayArrayToMask,
      describeSchedule,
      nextRuns,
    } from '../lib/models/inspector-schedule';
    import { groupFindings, formatMatchCount, findingBadges } from '../lib/models/inspector-findings';
    import { csvFilename, UNREACHABLE_COPY } from '../lib/models/inspector';
    import type {
      EffectivePolicy,
      InspectorFinding,
      InspectorMaskAction,
      InspectorScan,
    } from '../lib/models';

    type Tab = 'findings' | 'policy' | 'scans' | 'audit';
    let tab = $state<Tab>('findings');

    let loading = $state(true);
    let error = $state('');
    // $state.raw, not $state: these are replaced wholesale on every reload and
    // deep-proxying them makes `===` never match a raw row, which breaks the
    // "is this the row I opened?" check in the expand map below.
    let effective = $state.raw<EffectivePolicy | null>(null);
    let scans = $state.raw<InspectorScan[]>([]);
    let findings = $state.raw<InspectorFinding[]>([]);
    let actions = $state.raw<InspectorMaskAction[]>([]);
    let coverageNote = $state('');
    let detectionCaveat = $state('');
    let expanded = $state<Record<string, boolean>>({});
    let revealed = $state<Record<string, unknown>>({});
    let maskTargetFinding = $state.raw<InspectorFinding | null>(null);

    const appId = $derived(sessionStore.currentAppId);
    const canManage = $derived(sessionStore.can('pii:manage', { app: sessionStore.currentAppId }));
    const policy = $derived(effective?.policy ?? null);
    const groups = $derived(groupFindings(findings));

    async function loadAll() {
      if (!appId) return;
      loading = true;
      error = '';
      try {
        effective = await inspectorApi.effectivePolicy(appId);
        actions = await inspectorApi.listAppMaskActions(appId);
        if (effective.policy) {
          scans = await inspectorApi.listScans(effective.policy.id);
          const latest = scans.find((s) => s.status === 'succeeded') ?? scans[0];
          if (latest) {
            const page = await inspectorApi.listFindings(latest.id);
            findings = page.findings;
            coverageNote = page.coverage === 'partial' ? page.coverage_note : '';
            detectionCaveat = page.detection_caveat;
          } else {
            findings = [];
          }
        } else {
          scans = [];
          findings = [];
        }
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      } finally {
        loading = false;
      }
    }

    $effect(() => {
      // Re-read on app switch. `appId` is the only dependency on purpose: a
      // reload triggered by anything else would wipe a half-typed policy edit.
      if (appId) void loadAll();
    });

    // Poll only while something is in flight, and clear the interval in the
    // teardown — an interval that outlives the page keeps a dead component
    // fetching for as long as the tab is open.
    $effect(() => {
      const busy =
        scans.some((s) => s.status === 'queued' || s.status === 'running') ||
        actions.some((a) => ['preview', 'pending', 'running', 'cancelling'].includes(a.status));
      if (!busy) return;
      const id = setInterval(() => void loadAll(), 3000);
      return () => clearInterval(id);
    });
  </script>

  <AppShell requireApp>
    <div class="head">
      <h1>Privacy inspector</h1>
      {#if effective}
        <span class="muted">
          New events are masked within about {effective.enforcement_latency_secs} seconds of a change.
        </span>
      {/if}
    </div>

    <nav class="tabs" aria-label="Privacy inspector sections">
      <button class="tab" class:active={tab === 'findings'} onclick={() => (tab = 'findings')}>
        Findings <span class="count">{findings.length}</span>
      </button>
      <button class="tab" class:active={tab === 'policy'} onclick={() => (tab = 'policy')}>Policy</button>
      <button class="tab" class:active={tab === 'scans'} onclick={() => (tab = 'scans')}>
        Scans <span class="count">{scans.length}</span>
      </button>
      <button class="tab" class:active={tab === 'audit'} onclick={() => (tab = 'audit')}>
        Audit <span class="count">{actions.length}</span>
      </button>
    </nav>

    {#if error}
      <Card><p class="err">{error}</p></Card>
    {:else if loading}
      <Spinner />
    {:else if tab === 'findings'}
      <!-- Findings tab body: Step 5 -->
    {:else if tab === 'policy'}
      <!-- Policy tab body: Step 6 -->
    {:else if tab === 'scans'}
      <!-- Scans tab body: Step 7 -->
    {:else}
      <!-- Audit tab body: Step 8 -->
    {/if}
  </AppShell>

  {#if maskTargetFinding && appId}
    <MaskDialog
      appId={appId}
      finding={maskTargetFinding}
      onclose={() => (maskTargetFinding = null)}
      ondone={() => {
        maskTargetFinding = null;
        tab = 'audit';
        void loadAll();
      }}
    />
  {/if}

  <style>
    .head {
      display: flex;
      align-items: baseline;
      gap: 12px;
      margin-bottom: 12px;
    }
    .muted {
      color: var(--text-muted);
      font-size: 12.5px;
    }
    .err {
      color: var(--danger);
    }
    .tabs {
      display: flex;
      gap: 4px;
      border-bottom: 1px solid var(--border);
    }
    .tab {
      padding: 8px 14px;
      font-size: 13.5px;
      font-weight: 550;
      color: var(--text-muted);
      background: none;
      border: none;
      border-bottom: 2px solid transparent;
      cursor: pointer;
    }
    .tab:hover {
      color: var(--text);
    }
    .tab.active {
      color: var(--primary);
      border-bottom-color: var(--primary);
    }
    .count {
      display: inline-block;
      margin-left: 6px;
      padding: 1px 6px;
      border-radius: 999px;
      background: var(--surface-2);
      font-size: 11px;
    }
  </style>
  ```

- [ ] **Step 5: Fill in the Findings tab.** Replace `<!-- Findings tab body: Step 5 -->` with:
  ```svelte
      <Card>
        <!-- Non-dismissible, always. Detection is best-effort: the prefilter
             greps the JSON TEXT for the quoted key name, so a key hidden by a
             unicode escape, base64 or URL encoding is not found. -->
        <p class="caveat"><Icon name="info" size={14} /> {detectionCaveat}</p>
        {#if coverageNote}
          <p class="caveat">Coverage is partial: {coverageNote}</p>
        {/if}
      </Card>
      {#if findings.length === 0}
        <EmptyState title="No findings" description="Run a scan from the Scans tab." />
      {:else}
        {#each groups as g (g.key)}
          <Card>
            <h3>{g.table}.{g.column} <Badge>{formatMatchCount(g.total, true)} matches</Badge></h3>
            <DataTable
              head={['Path', 'Type', 'Matches', 'Last seen', '']}
              rows={g.findings}
            >
              {#snippet row(f: InspectorFinding)}
                <tr onclick={() => (expanded = { ...expanded, [f.id]: !expanded[f.id] })}>
                  <td>{f.key_path || '(whole value)'}</td>
                  <td>{f.value_type}</td>
                  <td class="num">{formatMatchCount(f.match_count, f.match_count_exact)}</td>
                  <td>{f.last_seen_at ?? '—'}</td>
                  <td>
                    {#each findingBadges(f) as b (b.label)}
                      <Badge title={b.title}>{b.label}</Badge>
                    {/each}
                    {#if canManage && !['devices', 'identities', 'workflows'].includes(f.source_table)}
                      <Button
                        variant="danger"
                        size="sm"
                        onclick={(e: MouseEvent) => {
                          e.stopPropagation();
                          maskTargetFinding = f;
                        }}
                      >
                        <Icon name="eye-off" size={14} /> Mask
                      </Button>
                    {/if}
                  </td>
                </tr>
                {#if expanded[f.id]}
                  <!-- A CSS grid with ARIA roles, NOT a nested <table>: a raw
                       table here sits inside DataTable's own tbody/td and picks
                       up its :global(tbody td) padding/white-space/alignment
                       rules by DOM descendance regardless of component
                       boundaries. Background, white-space and cursor are set
                       INLINE for the same reason. -->
                  <tr>
                    <td colspan="5" style="background: var(--surface-2); white-space: normal; cursor: default;">
                      <div class="detail" role="table" aria-label="Finding detail">
                        <div role="row">
                          <span role="cell">Redacted preview</span>
                          <span role="cell"><code>{f.sample_preview}</code></span>
                        </div>
                        <div role="row">
                          <span role="cell">Environment</span>
                          <span role="cell">{f.environment_id ?? f.env_scope}</span>
                        </div>
                        {#if revealed[f.id] !== undefined}
                          <JsonTree value={revealed[f.id]} expandTo={2} />
                        {:else}
                          <Button
                            size="sm"
                            onclick={async () => {
                              try {
                                const r = await inspectorApi.revealFinding(f.id);
                                revealed = { ...revealed, [f.id]: r.value };
                              } catch (e) {
                                toastStore.error(e instanceof Error ? e.message : String(e));
                              }
                            }}
                          >
                            Reveal one value (recorded in the audit trail)
                          </Button>
                        {/if}
                      </div>
                    </td>
                  </tr>
                {/if}
              {/snippet}
            </DataTable>
          </Card>
        {/each}
      {/if}
  ```
  and add to the `<style>` block:
  ```css
    .caveat {
      color: var(--text-muted);
      font-size: 12.5px;
      margin: 0 0 6px;
    }
    .detail {
      display: grid;
      gap: 6px;
      padding: 8px 0;
    }
    .detail [role='row'] {
      display: grid;
      grid-template-columns: 180px 1fr;
      gap: 12px;
    }
  ```
  If `DataTable`'s API is prop-based rather than snippet-based, read it with `cd /home/splimter/projects/freelance/sauron/dashboard && sed -n '1,60p' src/lib/components/DataTable.svelte` and follow `Storage.svelte:120-200` as the worked example.

- [ ] **Step 6: Fill in the Policy tab.** Replace `<!-- Policy tab body: Step 6 -->` with:
  ```svelte
      <Card>
        <h3>Inspection</h3>
        {#if !policy}
          <EmptyState
            title="No policy covers this app"
            description="Create one from the organization settings, scoped to the project, the app, or one environment."
          />
        {:else}
          <p>
            Scope: <Badge>{policy.target_type}</Badge>
            Status:
            <Badge variant={policy.enabled ? 'success' : 'muted'}>
              {policy.enabled ? 'enabled' : 'disabled'}
            </Badge>
          </p>
          {#if canManage}
            <!-- There is no Toggle primitive, so this is a Button plus a Badge. -->
            <Button
              onclick={async () => {
                await inspectorApi.patchPolicy(policy.id, { enabled: !policy.enabled });
                await loadAll();
              }}
            >
              {policy.enabled ? 'Disable' : 'Enable'}
            </Button>
          {/if}

          <h4>Tracked keys</h4>
          <p class="caveat">
            Matched case-insensitively and exactly against a key name at any depth.
            <code>Email</code> matches <code>email</code>; <code>user_email</code> does not.
          </p>
          <div class="chips">
            {#each policy.tracked_keys as k (k.key)}
              <Badge>
                {k.key}{k.scope === 'top' ? ' (top level)' : ''}
                {#if canManage}
                  <Button
                    size="sm"
                    onclick={async () => {
                      await inspectorApi.patchPolicy(policy.id, {
                        tracked_keys: policy.tracked_keys.filter((x) => x.key !== k.key),
                      });
                      await loadAll();
                    }}
                  >
                    <Icon name="x" size={12} />
                  </Button>
                {/if}
              </Badge>
            {/each}
          </div>
          {#if canManage}
            <Input
              placeholder="Add a key and press Enter"
              onkeydown={async (e: KeyboardEvent) => {
                if (e.key !== 'Enter') return;
                const el = e.currentTarget as HTMLInputElement;
                const key = el.value.trim().toLowerCase();
                if (!key) return;
                // Records in $state are REPLACED, never mutated in place.
                await inspectorApi.patchPolicy(policy.id, {
                  tracked_keys: [...policy.tracked_keys, { key, scope: 'any' }],
                });
                el.value = '';
                await loadAll();
              }}
            />
          {/if}

          <h4>Schedule</h4>
          <p>{describeSchedule(policy.schedule_days, policy.schedule_time, policy.schedule_tz)}</p>
          {#if DST_RISK_HOURS.includes(Number.parseInt(policy.schedule_time.slice(0, 2), 10))}
            <p class="caveat">
              On the spring-forward day this resolves to a valid instant; on the fall-back day it runs
              once, not twice. Times from 04:00 avoid the question entirely.
            </p>
          {/if}
          <div class="chips">
            {#each weekdayMaskToArray(policy.schedule_days) as on, i (i)}
              <Button
                size="sm"
                variant={on ? 'primary' : 'ghost'}
                disabled={!canManage}
                onclick={async () => {
                  const days = weekdayMaskToArray(policy.schedule_days);
                  days[i] = !days[i];
                  await inspectorApi.patchPolicy(policy.id, {
                    schedule_days: weekdayArrayToMask(days),
                  });
                  await loadAll();
                }}
              >
                {WEEKDAYS[i]}
              </Button>
            {/each}
          </div>
          <!-- No Select primitive; a raw <select> fed by the constants module. -->
          <select
            class="sel"
            disabled={!canManage}
            value={policy.schedule_tz}
            onchange={async (e: Event) => {
              await inspectorApi.patchPolicy(policy.id, {
                schedule_tz: (e.currentTarget as HTMLSelectElement).value,
              });
              await loadAll();
            }}
          >
            {#each COMMON_TIMEZONES as tz (tz)}
              <option value={tz}>{tz}</option>
            {/each}
          </select>
          <p class="caveat">
            Next runs (approximate — the server decides):
            {nextRuns(policy.schedule_days, policy.schedule_time, policy.schedule_tz)
              .map((d) => d.toISOString())
              .join(', ') || 'none'}
          </p>
          {#if policy.last_skip_reason}
            <p class="caveat">Last scheduled run: {policy.last_skip_reason}</p>
          {/if}
        {/if}
      </Card>

      <Card>
        <h3>Forward enforcement</h3>
        <p class="caveat">
          New events are masked within about {effective?.enforcement_latency_secs} seconds of a change.
        </p>
        {#if (effective?.masked_keys ?? []).length === 0}
          <EmptyState title="Nothing is masked yet" description="Mask a finding to start enforcing." />
        {:else}
          <ul>
            {#each effective?.masked_keys ?? [] as k (k.id)}
              <li>
                <code>{k.target_table}.{k.target_column}{k.json_path ? `.${k.json_path}` : ''}</code>
                <span class="caveat">since {k.created_at}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </Card>
  ```
  and add `.chips { display: flex; flex-wrap: wrap; gap: 6px; margin: 6px 0; } .sel { padding: 6px 8px; }` to the `<style>` block.

- [ ] **Step 7: Fill in the Scans tab.** Replace `<!-- Scans tab body: Step 7 -->` with:
  ```svelte
      <Card>
        <div class="head">
          <h3>Scans</h3>
          {#if canManage && policy}
            <Button
              onclick={async () => {
                try {
                  await inspectorApi.startScan(policy.id);
                  toastStore.success('Scan queued');
                  await loadAll();
                } catch (e) {
                  toastStore.error(e instanceof Error ? e.message : String(e));
                }
              }}
            >
              Run scan now
            </Button>
          {/if}
        </div>
        {#if scans.length === 0}
          <EmptyState title="No scans yet" description="Run one, or set a schedule on the Policy tab." />
        {:else}
          <DataTable
            head={['Started', 'Finished', 'Status', 'Rows scanned', 'Findings', 'Coverage', '']}
            rows={scans}
          >
            {#snippet row(s: InspectorScan)}
              <tr>
                <td>{s.started_at ?? '—'}</td>
                <td>{s.finished_at ?? '—'}</td>
                <td>
                  {#if s.status === 'running' || s.status === 'queued'}
                    <Spinner size={14} />
                  {/if}
                  <Badge>{s.status}</Badge>
                </td>
                <td class="num">{s.rows_scanned.toLocaleString()}</td>
                <td class="num">{s.findings_count.toLocaleString()}</td>
                <td>
                  <Badge variant={s.coverage === 'full' ? 'success' : 'warning'} title={s.coverage_note}>
                    {s.coverage}
                  </Badge>
                </td>
                <td>
                  {#if canManage && (s.status === 'queued' || s.status === 'running')}
                    <Button
                      size="sm"
                      onclick={async () => {
                        await inspectorApi.cancelScan(s.id);
                        await loadAll();
                      }}
                    >
                      Stop
                    </Button>
                  {/if}
                  <Button
                    size="sm"
                    onclick={() =>
                      inspectorApi.downloadFindingsCsv(
                        s.id,
                        csvFilename('findings', sessionStore.currentAppSlug ?? 'app', s.window_from.slice(0, 10), s.window_to.slice(0, 10)),
                      )}
                  >
                    CSV
                  </Button>
                </td>
              </tr>
            {/snippet}
          </DataTable>
        {/if}
      </Card>
  ```

- [ ] **Step 8: Fill in the Audit tab.** Replace `<!-- Audit tab body: Step 8 -->` with:
  ```svelte
      <Card>
        <h3>Mask audit trail</h3>
        <p class="caveat">
          Readable by anyone with <code>pii:read</code> — deliberately, and affordable precisely
          because these rows store paths and counts and never a value.
        </p>
        {#if actions.length === 0}
          <EmptyState title="Nothing masked yet" description="Mask a finding to start the trail." />
        {:else}
          <DataTable
            head={['When', 'Who', 'Targets', 'Status', 'Rows masked', 'Cold skipped', 'Cancelled by']}
            rows={actions}
          >
            {#snippet row(a: InspectorMaskAction)}
              <tr onclick={() => (expanded = { ...expanded, [a.id]: !expanded[a.id] })}>
                <td>{a.requested_at}</td>
                <td>{a.requested_by_email || '—'}</td>
                <td>{a.targets.length}</td>
                <td><Badge>{a.status}</Badge></td>
                <!-- rows_masked > estimated_rows is NORMAL on an actively
                     ingesting app, because preview and execution are separated
                     in time. Never render it as an error. -->
                <td class="num">{a.rows_masked.toLocaleString()}</td>
                <td class="num">{a.cold_rows_skipped.toLocaleString()}</td>
                <td>{a.cancelled_by_email || '—'}</td>
              </tr>
              {#if expanded[a.id]}
                <tr>
                  <td colspan="7" style="background: var(--surface-2); white-space: normal; cursor: default;">
                    <div class="detail" role="table" aria-label="Mask action detail">
                      {#each a.targets as t, i (i)}
                        <div role="row">
                          <span role="cell">Target</span>
                          <span role="cell"><code>{t.table}.{t.column}{t.path ? `.${t.path}` : ''}</code></span>
                        </div>
                      {/each}
                      {#if a.error}
                        <div role="row"><span role="cell">Error</span><span role="cell">{a.error}</span></div>
                      {/if}
                      {#if a.vacuum_advised}
                        <div role="row">
                          <span role="cell">Maintenance</span>
                          <span role="cell">This pass rewrote enough rows that a VACUUM is worth scheduling.</span>
                        </div>
                      {/if}
                      <h4>What this did not reach</h4>
                      {#each UNREACHABLE_COPY as r, i (i)}
                        <div role="row">
                          <span role="cell">{r.headline ? '' : r.what}</span>
                          <span role="cell" class:headline={r.headline}>
                            {r.headline ? r.what : `${r.why} — bounded by: ${r.bounded}`}
                          </span>
                        </div>
                      {/each}
                    </div>
                  </td>
                </tr>
              {/if}
            {/snippet}
          </DataTable>
          <Button
            size="sm"
            onclick={() =>
              appId &&
              inspectorApi.downloadMaskActionsCsv(
                appId,
                csvFilename('mask-actions', sessionStore.currentAppSlug ?? 'app', '', ''),
              )}
          >
            Export CSV
          </Button>
        {/if}
      </Card>
  ```
  and add `.headline { font-weight: 600; }` to the `<style>` block.

- [ ] **Step 9: Typecheck and lint.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`. Fix every reported prop mismatch against the real component signatures (`Button`'s `variant`/`size` set, `Badge`'s props, `DataTable`'s row API, `Spinner`'s `size`, `sessionStore.currentAppSlug`) — read each component rather than guessing. Then `npm run test` and confirm green.

- [ ] **Step 10: Drive the page in a browser.** Start the dashboard and API, open `#/inspector`, and confirm: all four tabs render; the Findings tab shows the non-dismissible detection caveat; a rollup finding carries the "recurring" badge and a `devices` finding carries "not maskable" with no Mask button; the Policy tab's weekday buttons round-trip; the Scans tab's "Run scan now" queues one and the row polls to `succeeded`; the Audit tab's expanded detail lists all thirteen `UNREACHABLE_COPY` entries.

---

## Task 33: `MaskDialog.svelte`

**Files:**
- Create `dashboard/src/lib/components/inspector/MaskDialog.svelte`

**Interfaces:**
- Consumes: `inspectorApi.{maskPreview, getMaskAction, confirmMask}` (Task 31), `maskConfirmReady`, `describeTarget`, `expandCompanionTargets`, `UNREACHABLE_COPY` (Task 30).
- Produces: the component, with props `appId: string`, `finding: InspectorFinding`, `onclose: () => void`, `ondone: () => void`.

> `ConfirmDialog` is insufficient — **it has no text input at all**, and a typed literal like `MASK` proves intent and proves nothing about scope. The realistic failure is not a mis-click; it is masking the **wrong app**, because the operator saw a finding and forgot which app was selected.

- [ ] **Step 1: Create the component.** Create `dashboard/src/lib/components/inspector/MaskDialog.svelte`:
  ```svelte
  <!--
    Mask confirmation. Modal size="md" — ConfirmDialog has no text input, and
    typing the APP SLUG is the only confirmation that forces attention onto the
    thing that actually goes wrong.

    The §1 "what this does not reach" panel is PERMANENTLY VISIBLE and
    non-collapsible. The product must never say "permanently removed": masking
    rewrites rows in hot Postgres only.
  -->
  <script lang="ts">
    import { untrack } from 'svelte';
    import Modal from '../ui/Modal.svelte';
    import Button from '../ui/Button.svelte';
    import Input from '../ui/Input.svelte';
    import Badge from '../ui/Badge.svelte';
    import Spinner from '../ui/Spinner.svelte';
    import * as inspectorApi from '../../api/inspector';
    import { toastStore } from '../../stores/toast.svelte';
    import {
      UNREACHABLE_COPY,
      describeTarget,
      expandCompanionTargets,
      maskConfirmReady,
    } from '../../models/inspector';
    import type { InspectorFinding, InspectorMaskAction } from '../../models';

    interface Props {
      appId: string;
      finding: InspectorFinding;
      onclose: () => void;
      ondone: () => void;
    }
    const { appId, finding, onclose, ondone }: Props = $props();

    // $state.raw: the action is replaced wholesale on every poll, and deep
    // proxying it means `action === previous` never matches, which would restart
    // the poll effect on every tick.
    let action = $state.raw<InspectorMaskAction | null>(null);
    let slug = $state('');
    let ttlSecs = $state(900);
    let maxRows = $state(20_000_000);
    let latencySecs = $state(30);
    let typed = $state('');
    let starting = $state(true);
    let submitting = $state(false);
    let error = $state('');

    // Computed locally so the blast radius is described BEFORE the server
    // answers. Mirrors the backend's expand_targets.
    const previewTargets = $derived(
      expandCompanionTargets({
        table: finding.source_table,
        column: finding.source_column,
        path: finding.key_path,
      }),
    );
    const touchesEventUser = $derived(previewTargets.some((t) => t.column === 'event_user'));
    const ready = $derived(
      action
        ? maskConfirmReady(
            typed,
            slug,
            { status: action.status, previewed_at: action.previewed_at, estimated_rows: action.estimated_rows },
            ttlSecs,
            maxRows,
          )
        : false,
    );

    $effect(() => {
      // Prop-seeding read, wrapped in untrack() so a parent reload cannot wipe a
      // half-typed confirmation by re-running this effect.
      const id = untrack(() => appId);
      const f = untrack(() => finding);
      void (async () => {
        try {
          const started = await inspectorApi.maskPreview(id, { finding_id: f.id });
          action = started.action;
          slug = started.app_slug;
          ttlSecs = started.preview_ttl_secs;
          maxRows = started.mask_max_rows;
          latencySecs = started.enforcement_latency_secs;
        } catch (e) {
          error = e instanceof Error ? e.message : String(e);
        } finally {
          starting = false;
        }
      })();
    });

    $effect(() => {
      const a = action;
      if (!a || a.status !== 'preview') return;
      const id = setInterval(async () => {
        try {
          action = await inspectorApi.getMaskAction(a.id);
        } catch {
          // A transient poll failure must not close the dialog; the next tick
          // retries and the confirm button stays disabled meanwhile.
        }
      }, 2000);
      return () => clearInterval(id);
    });

    async function confirm() {
      if (!action) return;
      submitting = true;
      try {
        await inspectorApi.confirmMask(action.id, typed.trim());
        toastStore.success(
          `Mask queued. New events are masked within about ${latencySecs} seconds.`,
        );
        ondone();
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      } finally {
        submitting = false;
      }
    }
  </script>

  <Modal size="md" title="Mask this value" {onclose}>
    {#if error}
      <p class="err">{error}</p>
    {/if}

    <h4>What will be rewritten</h4>
    <ul>
      {#each previewTargets as t, i (i)}
        <li><code>{describeTarget(t)}</code></li>
      {/each}
    </ul>
    <p class="note">
      The value becomes the JSON string <code>"****"</code> and the key is kept. The TYPE changes, so
      arithmetic, containment filters and range comparisons stop working for masked rows.
    </p>
    {#if touchesEventUser}
      <p class="warn">
        This masks <code>event_user</code>, which backs the <code>user.email:</code> search dimension.
        Masked rows will silently stop matching those queries.
      </p>
    {/if}

    <h4>Affected rows</h4>
    {#if starting || !action || action.status === 'preview'}
      <p><Spinner size={14} /> Counting affected rows…</p>
    {:else if action.status === 'previewed'}
      <p>
        <Badge>{action.estimated_rows.toLocaleString()} rows</Badge>
        <Badge variant="muted">{action.cold_rows_skipped.toLocaleString()} row(s) already in cold storage, skipped</Badge>
      </p>
      <p class="note">
        The count was taken a moment ago. On an actively ingesting app more rows will match by the
        time the mask runs, so a larger "rows masked" figure afterwards is normal, not an error.
      </p>
    {:else}
      <p class="err">The preview did not complete: {action.error || action.status}</p>
    {/if}

    <h4>What this does not reach</h4>
    <div class="unreachable">
      {#each UNREACHABLE_COPY as r, i (i)}
        <p class:headline={r.headline} class:readFirst={r.readFirst}>
          {#if !r.headline}<strong>{r.what}</strong> — {/if}{r.why}
          {#if !r.headline}<span class="bounded">Bounded by: {r.bounded}</span>{/if}
        </p>
      {/each}
    </div>
    <p class="note">
      A running mask can be stopped, but it cannot be undone. There is no shadow copy.
    </p>

    <label for="mask-confirm">Type the app slug ({slug}) to confirm</label>
    <Input id="mask-confirm" bind:value={typed} placeholder={slug} autocomplete="off" />

    {#snippet footer()}
      <Button onclick={onclose}>Cancel</Button>
      <Button variant="danger" disabled={!ready || submitting} onclick={confirm}>
        {submitting ? 'Queuing…' : 'Mask permanently in hot Postgres'}
      </Button>
    {/snippet}
  </Modal>

  <style>
    .err {
      color: var(--danger);
    }
    .warn {
      color: var(--warning);
      font-size: 12.5px;
    }
    .note {
      color: var(--text-muted);
      font-size: 12.5px;
    }
    .unreachable {
      max-height: 260px;
      overflow-y: auto;
      border: 1px solid var(--border);
      border-radius: 6px;
      padding: 8px 10px;
      font-size: 12.5px;
    }
    .unreachable .headline {
      font-weight: 600;
      color: var(--text);
    }
    .unreachable .readFirst {
      border-left: 3px solid var(--warning);
      padding-left: 8px;
    }
    .bounded {
      display: block;
      color: var(--text-muted);
    }
    label {
      display: block;
      margin-top: 12px;
      font-size: 13px;
      font-weight: 550;
    }
  </style>
  ```
  Note the panel scrolls **inside itself** rather than being collapsible — collapsing it would let an operator confirm without ever rendering the active-users row, which is the one that must be read before the fact because afterwards there is nothing left to recompute from.

- [ ] **Step 2: Typecheck.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`. Fix prop mismatches against the real `Modal`, `Button`, `Input`, `Badge` and `Spinner` signatures — read each rather than guessing, especially whether `Modal` takes a `footer` snippet or a `footer` slot.

- [ ] **Step 3: Run the suite.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`. Green.

- [ ] **Step 4: Drive the dialog manually.** Open a finding's Mask button and confirm, in order: the dialog opens showing "Counting affected rows…"; the confirm button is disabled; the row count appears within a few seconds of the inspector worker's tick; typing the wrong slug leaves the button disabled; typing the right slug enables it; typing the right slug, then waiting past `INSPECTOR_PREVIEW_TTL_SECS`, disables it again; a parent reload while half-typed does **not** clear the input; confirming toasts and switches to the Audit tab with the new action visible.

---

## Task 34: Packaging, config documentation, and the `TIER_HOT_DAYS` relocation

**Files:**
- Create `packaging/rpm/config/inspector.env`
- Create `packaging/rpm/systemd/sauron-inspector.service`
- Modify `packaging/rpm/config/sauron.env`
- Modify `packaging/rpm/config/tier.env`
- Modify `packaging/rpm/binaries.txt`
- Modify `packaging/rpm/sauron.spec`
- Modify `packaging/rpm/build-rpm.sh`
- Modify `packaging/rpm/SETUP.md`
- Modify `docker-compose.yml`
- Modify `.env.example`
- Modify `README.md`

**Interfaces:**
- Consumes: the 25 config keys (Task 4) and the `sauron-inspector` binary (Task 21).
- Produces: nothing code-level; this task makes the binary shippable and the keys documented.

> **All seven packaging touchpoints move in lockstep.** `binaries.txt` drives CI's prebuilt assemble, `build-rpm.sh --prebuilt`'s preflight, and the spec's `%install` loop — but **not** `%files`, which is manual, and rpmbuild fails on installed-but-unpackaged files. `build-rpm.sh`'s two `install -m0644` lines for the unit and the env file are **not** driven by `binaries.txt` either; missing them yields a cryptic rpmbuild SOURCE-not-found failure.

- [ ] **Step 1: Relocate `TIER_HOT_DAYS` and add the two shared keys.** Delete the `TIER_HOT_DAYS=30` line from `packaging/rpm/config/tier.env`, and append to `packaging/rpm/config/sauron.env`:
  ```
  # Hot/cold boundary, in days. Read by sauron-tier, sauron-inspector AND
  # sauron-api, which is why it is declared HERE and not in tier.env.
  #
  # Until the PII inspector shipped this was one worker's private tuning knob.
  # It is now the boundary three binaries derive independently, and a divergence
  # means the masker rewriting rows in a partition sauron-tier has already
  # exported to Parquet — Postgres masked, Parquet raw, and the drop destroys
  # the only masked copy.
  #
  # UPGRADE HAZARD: tier.env is %config(noreplace), so on any host whose
  # operator ever edited it, rpm keeps their file verbatim and ships the new one
  # beside it as .rpmnew. Their stale TIER_HOT_DAYS= line then wins for
  # sauron-tier alone. DELETE that line by hand after upgrading, and do not
  # enable the inspector before you have.
  TIER_HOT_DAYS=30

  # How long a mask takes to reach every ingest replica, in seconds. Read by
  # sauron-ingest (the enforcer's cache TTL) and sauron-api (the number the
  # dashboard states literally). Declared here because neither of those units
  # loads inspector.env, so the promise and the behaviour cannot diverge.
  # Raising it delays enforcement; lowering it adds one indexed query per app
  # per interval on the ingest pool.
  INSPECTOR_POLICY_CACHE_SECS=30

  # How far back the retro-mask's tail sweep re-checks, in seconds. Read by
  # sauron-inspector and sauron-api. Clamped at load to at least 4 x
  # INSPECTOR_POLICY_CACHE_SECS: a sweep shorter than the cache TTL closes
  # nothing, and the rows written in that window stay raw forever because the
  # retro-mask is a one-shot job.
  INSPECTOR_TAIL_SWEEP_SECS=120
  ```

- [ ] **Step 2: Write `inspector.env`.** Create `packaging/rpm/config/inspector.env`:
  ```
  # sauron-inspector — PII scanning, retro-masking and the audit reaper.
  #
  # OFF by default. The scanner reads the same partitions the ingest path
  # writes, so a deployment opts in deliberately. Nothing here is read by
  # sauron-api or sauron-ingest; the two keys they share live in sauron.env.

  # Master switch. Set to 1 to enable.
  INSPECTOR_ENABLED=false

  # Scheduler cadence, seconds (clamped 5..3600). This loop only claims due
  # policies; it is never blocked by a running scan.
  INSPECTOR_TICK_SECS=30

  # Rows read per phase-1 batch. The LIMIT sits on an index-bounded inner
  # window, so this bounds SCANNED rows, not matches. Raising it lengthens the
  # gap between heartbeats and between inter-batch pauses.
  INSPECTOR_BATCH_ROWS=5000

  # Sleep between batches, milliseconds. This plus the batch size IS the duty
  # cycle that keeps the ingest working set resident in the buffer cache.
  INSPECTOR_BATCH_PAUSE_MS=200

  # A scan whose heartbeat is older than this is re-claimable by another
  # worker. Shorter than a scan's slowest unit and you get needless re-claims.
  INSPECTOR_LEASE_SECS=120

  # After this many claims a scan finalizes as failed, so one poison unit
  # cannot loop forever.
  INSPECTOR_MAX_ATTEMPTS=3

  # Per-connection statement_timeout, milliseconds. Set at checkout and RESET
  # before the connection returns to the pool.
  INSPECTOR_STATEMENT_TIMEOUT_MS=30000

  # Scan window ceiling, days. Defaults to SEARCH_SCAN_CLAMP_DAYS, which itself
  # defaults to TIER_HOT_DAYS — nothing older is in Postgres anyway.
  INSPECTOR_WINDOW_DAYS=30

  # Detector-mode window, days. Detector mode drops the SQL prefilter entirely
  # and walks every string leaf of every row: roughly 20x the CPU and 20x the
  # bytes shipped out of Postgres. On a 30M-row app that is the difference
  # between a scan that finishes overnight and one still running at noon.
  INSPECTOR_DETECTOR_WINDOW_DAYS=7

  # Phase-2 rows per unit before counts become lower bounds and the scan is
  # reported as partial rather than full.
  INSPECTOR_MAX_PHASE2_ROWS_PER_UNIT=200000

  # Truncation point for the default-partition sweep. Those rows are never
  # tiered and never dropped, so on a deployment that had data before the
  # partitioning migration this child can be very large.
  INSPECTOR_DEFAULT_SWEEP_ROWS=50000

  # A missed scheduled run older than this is SKIPPED, not replayed. A 03:00
  # scan firing at 09:00 on a Monday is precisely the production load spike the
  # schedule existed to avoid.
  INSPECTOR_CATCHUP_GRACE_HOURS=6

  # Scans retained per policy. Their findings are deleted in bounded batches
  # before the parent row goes.
  INSPECTOR_SCAN_KEEP=20

  # Finding retention, days. A nightly scan producing 33k findings is 12M rows
  # a year without this.
  INSPECTOR_FINDING_RETENTION_DAYS=90

  # Rows rewritten per mask batch. Halved automatically when any target carries
  # a wildcard, because the array rebuild re-serializes the whole array per row.
  INSPECTOR_MASK_BATCH=2000

  # Sleep between mask batches, milliseconds. A 2000-row batch is ~0.37 s of
  # write on error_events (13 index updates per row), so 200 ms is a ~65% duty
  # cycle. Raise it if ingest latency moves during a mask.
  INSPECTOR_MASK_PAUSE_MS=200

  # A mask action claimed longer ago than this is re-claimable. This is the
  # crash-resume mechanism; the cursor is durable, so a re-claim never
  # double-counts.
  INSPECTOR_CLAIM_STALE_SECS=300

  # Abandoned preview retention, days. Previews are not audit-relevant.
  INSPECTOR_PREVIEW_GC_DAYS=7

  # Mask-audit retention, days. 0 = NEVER PRUNE, which is the default: this
  # table grows per human action, not per rule evaluation, and it is the record
  # a compliance question is answered from.
  INSPECTOR_AUDIT_RETENTION_DAYS=0

  # Age at which staff emails and confirm_source are nulled on audit rows,
  # keeping counts and targets. Without this the privacy feature is the only
  # un-erasable store of staff PII in the schema, because deleting a user is
  # this product's de-facto erasure mechanism everywhere else.
  INSPECTOR_AUDIT_PII_DAYS=730
  ```
  Then append the three API-read keys to `packaging/rpm/config/api.env`:
  ```
  # PII inspector, API side. INSPECTOR_MASK_MAX_ROWS is the ceiling a confirm
  # refuses above — raise it explicitly rather than by accident.
  INSPECTOR_MASK_MAX_ROWS=20000000
  # Preview freshness, seconds, measured from the preview COMPLETING.
  INSPECTOR_PREVIEW_TTL_SECS=900
  # Buffered CSV ceiling. A buffered export cannot be truncated honestly, so
  # over this the route answers 400 rather than shipping a prefix.
  INSPECTOR_EXPORT_MAX_ROWS=50000
  ```

- [ ] **Step 3: Write the systemd unit.** Create `packaging/rpm/systemd/sauron-inspector.service` by copying `packaging/rpm/systemd/sauron-alerts.service` verbatim and then: change every `sauron-alerts` to `sauron-inspector`; **remove** `EnvironmentFile=/etc/sauron/secret.env` (it decrypts nothing); **remove** any `ReadWritePaths=` line (it writes no files); keep `Type=exec`, `After=network-online.target sauron-migrate.service`, `Restart=on-failure`, `RestartSec=2`, the full hardening block and `StateDirectory=sauron`; and set `EnvironmentFile=/etc/sauron/sauron.env` plus `EnvironmentFile=/etc/sauron/inspector.env` in that order. Verify with `cd /home/splimter/projects/freelance/sauron && diff packaging/rpm/systemd/sauron-alerts.service packaging/rpm/systemd/sauron-inspector.service` and confirm the only differences are the ones listed.

- [ ] **Step 4: Add the binary to the manifest.** In `packaging/rpm/binaries.txt`, add `sauron-inspector` to the `# --- sauron-server ---` group, after `sauron-tier`.

- [ ] **Step 5: Edit the spec.** In `packaging/rpm/sauron.spec`: add `Source16: systemd/sauron-inspector.service` and `Source37: config/inspector.env` (use the next free numbers if 16 or 37 are taken — check with `grep -n '^Source' packaging/rpm/sauron.spec`); add the matching `%install` lines beside the other unit/config installs; add explicit `%files server` entries for `/usr/bin/sauron-inspector`, `%{_unitdir}/sauron-inspector.service` and `%attr(0640,root,sauron) %config(noreplace) /etc/sauron/inspector.env`; and add `sauron-inspector.service` to the `%post`, `%preun` and `%postun_with_restart` service lists.

- [ ] **Step 6: Edit `build-rpm.sh`.** Add the two hardcoded `install -m0644` lines for `systemd/sauron-inspector.service` and `config/inspector.env` alongside the existing ones. This list is **not** driven by `binaries.txt`.

- [ ] **Step 7: Verify the spec parses and the SRPM builds.** `cd /home/splimter/projects/freelance/sauron/packaging/rpm && rpmspec -P sauron.spec > /dev/null && ./build-rpm.sh --srpm`. Both succeed. An `installed but unpackaged files found` error means the `%files` entry from Step 5 is missing; a `SOURCE... not found` error means Step 6 is.

- [ ] **Step 8: Add the compose service and raise `max_connections`.** In `docker-compose.yml`, copy the `alerts` service block to a new `inspector` service: same `build` context `./backend` with `args: BIN: sauron-inspector`, `depends_on` migrate `service_completed_successfully` and postgres `service_healthy`, **no** `JWT_SECRET`, **no** `NOTIFY_SECRET_KEY`, **no** Redis. Give it `${VAR:-default}` interpolation for every key in `inspector.env` plus:
  ```yaml
        TIER_HOT_DAYS: ${TIER_HOT_DAYS:-30}
        INSPECTOR_POLICY_CACHE_SECS: ${INSPECTOR_POLICY_CACHE_SECS:-30}
        INSPECTOR_TAIL_SWEEP_SECS: ${INSPECTOR_TAIL_SWEEP_SECS:-120}
  ```
  Compose has no shared env file, so repeat those three on the `api` service and `TIER_HOT_DAYS` on `tier` as well — all reading the same variable so they cannot drift. Then add to the `postgres` service:
  ```yaml
      # Peak pooled demand is api 16 + ingest 8 + alerts 8 + tier 4 + monitor
      # (50 + 8) + inspector 4 = 98, against a stock max_connections of 100 with
      # 3 reserved for superusers. Exhaustion surfaces as API 500s and ingest
      # 202-then-drop, not as an inspector error, so give it real headroom.
      command: postgres -c max_connections=200
  ```

- [ ] **Step 9: Document every key in `.env.example` and the README.** Append to `.env.example` under a `# --- inspector (sauron-inspector) ---` header every key from Steps 1 and 2, each with the one-line operational consequence from those files. Add a row per key to the README's configuration table. Then run the CI gate S0 added: `cd /home/splimter/projects/freelance/sauron && grep -oE '(var|parse)\("[A-Z_]+"' backend/crates/sauron-core/src/config.rs | grep -oE '[A-Z_]+' | sort -u | while read k; do grep -q "^$k=" .env.example || echo "UNDOCUMENTED: $k"; done`. Expected: no output.

- [ ] **Step 10: Append the upgrade rows and the connection-budget prerequisite to SETUP.md §11.** In `packaging/rpm/SETUP.md`'s "Upgrading" section table, add:
  | Migration | What breaks if it is skipped |
  |---|---|
  | `000041` | Custom roles holding `org:manage` never receive `pii:read`/`pii:manage`; the Owner and Admin presets keep working, so it looks like a role bug rather than a missed migration. |
  | `000042` | Every `/v1/inspector/*` route 500s. |
  | `000043` | Worse: the ingest pipeline's `masked_keys_for_app` query fails on **every cache miss**, so forward masking is off deployment-wide with only a rate-limited log line. The enforcer fails stale rather than open, so ingest keeps flowing — which is exactly why nobody notices. |

  and add, immediately below the table:
  ```
  After upgrading to the release that ships the PII inspector, **remove the
  `TIER_HOT_DAYS=` line from `/etc/sauron/tier.env` by hand.** That file is
  `%config(noreplace)`, so if you ever edited it rpm keeps your version and
  ships the new one as `.rpmnew`. Your stale line then wins for `sauron-tier`
  alone, while `sauron-inspector` and `sauron-api` use the shared declaration in
  `sauron.env` — and that divergence means the masker rewrites rows in a
  partition the tier worker has already exported to Parquet. Do not set
  `INSPECTOR_ENABLED=1` before you have done this.

  A meaningful `confirm_source` in the mask audit trail requires
  `API_TRUST_FORWARDED_HEADERS=true` behind a proxy that **overwrites**
  `X-Forwarded-For`. With the shipped nginx and the default `false`, every audit
  row records the proxy's address.

  **Postgres `max_connections` must be at least 150 before you enable the
  inspector.** `sauron-inspector` opens one 4-connection pool, taking peak
  pooled demand from 94 (`sauron-api` 16 + `sauron-ingest` 8 + `sauron-alerts`
  8 + `sauron-tier` 4 + `sauron-monitor` 50 + 8) to 98 — against a stock
  `max_connections` of 100 with 3 reserved for superusers. Exhaustion does not
  surface as an inspector error: it surfaces as `sauron-api` 500s and
  `sauron-ingest` accepting a 202 and then dropping the event. Check with
  `sudo -u postgres psql -c 'SHOW max_connections'` and raise it in
  `postgresql.conf` (the compose stack does this with
  `command: postgres -c max_connections=200`; an RPM host has no such
  override and defaults to 100).
  ```

- [ ] **Step 11: Verify the packaged unit runs.** Build the RPMs, install them in a container or VM, then `systemctl start sauron-inspector` and `journalctl -u sauron-inspector -n 20`. Expected with the default config: one line `INSPECTOR_ENABLED is false; sauron-inspector is idle`, unit `active (running)`.

---

## Task 35: Wiki, in-product docs, and the final green sweep

**Files:**
- Create `wiki/Privacy-Inspector.md`
- Modify `wiki/_Sidebar.md`, `wiki/Home.md`, `wiki/Best-Practices.md`, `wiki/Search.md`
- Modify `dashboard/src/pages/Docs.svelte`

**Interfaces:**
- Consumes: `UNREACHABLE_COPY` (Task 30) — the wiki page carries the same thirteen entries verbatim, so support answers and the product cannot diverge.
- Produces: documentation only.

- [ ] **Step 1: Write the wiki page.** Create `wiki/Privacy-Inspector.md` covering, in this order:
  1. **What it is.** Find developer-supplied PII in telemetry jsonb columns, prove what was found without storing a second copy, mask it in hot Postgres, and enforce the mask on all future ingest.
  2. **What masking does not mean.** The `UNREACHABLE_COPY` headline plus all twelve rows, **verbatim** from `dashboard/src/lib/models/inspector.ts`. State plainly: the product never says "permanently removed"; it says "masked in hot Postgres and in all future ingest".
  3. **Detection is best-effort, not a compliance guarantee.** The phase-1 prefilter greps the JSON *text* for the quoted key name, so a key serialized with a unicode escape evades it, as does anything inside a base64 or URL-encoded blob. It is the right tool for accidental PII and useless against an adversary.
  4. **Policies.** Precedence is most specific wins, whole row, no merging: `app_env` > `app` > `project`, one policy per node. A narrower row **subtracts** its pairs from the parent's scan, enabled or not — that is how an admin excludes one noisy environment.
  5. **Keys vs detectors.** A tracked key is a literal name, case-insensitive, exact, at any depth. Detectors are opt-in, get their own much shorter window, and change the cost model by an order of magnitude.
  6. **Masking semantics and the three visible regressions.** The value becomes `"****"` and the key is kept, so the *type* changes; masking `event_user` breaks the `user.email:` search dimension; and **`issues.title` sticks forever once masked** — a fingerprint is a stable error identity, so the Issues page shows `****` for that fingerprint even if every subsequent occurrence is benign. Support will be asked about this one.
  7. **What is scanned but never maskable, and why.** `devices` (every column is `COALESCE(EXCLUDED.x, devices.x)`), `identities` (`alias_id`/`distinct_id` *are* the identity graph, so masking merges people rather than redacting them), `workflows` (`cancel_reason` is derived server-side — mask `analytics_events.properties` instead).
  8. **`event_users.properties` is forward-enforcement only.** The identify() write merges with `||`, which never removes keys.
  9. **The audit trail and its trade.** The org-wide audit CSV exports `requested_by_email` for every action, which makes a downloadable staff-email roster available to any org-scoped `pii:read` holder. That is deliberate, and it is bounded by `INSPECTOR_AUDIT_PII_DAYS`.
  10. **Operating it.** `INSPECTOR_ENABLED=false` by default; schedule broad masks off-peak and `VACUUM` after; the mask job never runs `VACUUM` itself; a running mask can be stopped but not undone.
  11. **Permissions.** `pii:read` and `pii:manage`, Owner and Admin only. Developer stays at 18 permissions and Viewer at 7.

- [ ] **Step 2: Link it.** Add `Privacy Inspector` to `wiki/_Sidebar.md` in the same group as `Best-Practices`, and a one-line entry with a link to `wiki/Home.md`'s index.

- [ ] **Step 3: Extend `Best-Practices.md` §2.** Add a subsection "Keep PII out of telemetry in the first place" covering: prefer stable opaque ids over emails in `distinct_id` and `context.user.id`; put customer data in `extra` rather than in the exception message, because `error_events.title` is derived from the message and `exception_value` and is the most-read string in the product; and run a scan before the first production release rather than after an incident.

- [ ] **Step 4: Add the note to `Search.md`.** Append a short section: masked rows carry the JSON string `"****"`, so a masked row silently stops matching `user.email:*@acme.com` and every other predicate over that column, and any `@>` containment or range comparison against the old value stops matching too. Name `event_user` explicitly as the column that backs the documented `user.` dimension.

- [ ] **Step 5: Document the flow in-product.** In `dashboard/src/pages/Docs.svelte`, add a "Privacy inspector" section describing the five-step flow — create a policy → run a scan → review findings → preview a mask → confirm with the app slug — and stating the two things a reader must not get wrong: masking rewrites rows in hot Postgres only, and it cannot be undone.

- [ ] **Step 6: Full backend green sweep.** Run, in order:
  - `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check`
  - `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  - `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test --workspace`
  - `cd /home/splimter/projects/freelance/sauron/backend && TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test --workspace`

  All four clean. The third run must be green **without** a database — CI has none, and the harness skips.

- [ ] **Step 7: Full dashboard green sweep.** `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test && npm run check`. Both clean.

- [ ] **Step 8: Assert the schema delta one last time.** `cd /home/splimter/projects/freelance/sauron/backend && grep -c '^diesel::table!' crates/sauron-db/src/schema.rs` — exactly 6 more than the pre-slice count — and `grep -n 'file' diesel.toml` — still no `file =` key.

- [ ] **Step 9: Run the manual end-to-end gate.** There is no HTTP harness assertion for the UI or for CSV bodies, so this is the real gate. In one sitting: create a policy → run a scan → see findings → reveal one and see the `inspector_reveal_audit` row → preview a mask → confirm is disabled until the slug matches → let a preview expire and see the confirm rejected → confirm → cancel mid-run and see terminal `cancelled` with a durable cursor → re-queue and see it complete → send a new event and confirm it lands masked → open both CSVs in a spreadsheet and confirm a leading-`=` value renders as text, not a formula.

- [ ] **Step 10: Read back the promise.** Grep the whole tree for the phrase the product must never make: `cd /home/splimter/projects/freelance/sauron && grep -rn "permanently removed" dashboard/src wiki backend/bins backend/crates`. Expected: no output. Then `grep -rn "hot Postgres" dashboard/src/lib/models/inspector.ts wiki/Privacy-Inspector.md` and confirm the headline appears in both.

