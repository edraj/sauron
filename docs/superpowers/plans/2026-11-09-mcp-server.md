# Sauron MCP Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a read-only MCP endpoint in `sauron-api` that developers connect to Claude Code with a personal API key. It has 8 debugging tools, an org-level off switch, and a new per-issue hourly rollup so that "trending" counts are real.

**Architecture:**
- Personal keys (`api_keys` table) authenticate only `/v1/mcp`. An axum middleware validates the key and inserts an `McpCaller` into the request.
- The endpoint is the official `rmcp` Streamable HTTP service, running stateless with JSON responses.
- Every tool resolves the caller's `user_id` and reuses the dashboard's own authorization (`sauron_auth::authorize_env_read_with_perms`, `gate_event_body`) and read functions. Handler bodies are factored into `pub(crate)` functions where needed.
- A new `issue_stats_hourly` rollup is folded by the existing rollup loop, with its own watermark and an operator-run backfill. It ranks issues by in-window events.

**Tech Stack:** Rust (axum 0.8, diesel-async, Postgres 16, Redis), `rmcp` 3.9, Svelte 5 + TS dashboard, vitest.

**Spec:** `docs/superpowers/specs/2026-11-09-mcp-server-design.md`. Read it before starting any task.

## Global Constraints

- **Git:** NEVER commit, never create branches, never `git stash`. Another session may edit this checkout live. Every task's last step is "leave changes in the working tree".
- **Read-only:** no MCP tool may change user-visible state. The internal write-backs the dashboard read paths already do (`symbolicate_event` caching, `repair_blank_culprits`) are allowed.
- **Tool names**, exactly: `list_apps`, `trending_issues`, `get_issue`, `list_issue_events`, `describe_search`, `get_session_timeline`, `list_releases`, `slow_operations`. Every tool is annotated `read_only_hint = true, open_world_hint = false`.
- **Keys:**
  - Token = `sauron_pat_` + `sauron_core::ids::opaque_token()` (64 hex characters). Stored as `sauron_auth::hash_token(token)` (hex SHA-256). Display prefix = the first 17 characters of the token.
  - Expiry choices, exactly: `"30d"`, `"90d"` (UI default), `"1y"`, `"never"`.
  - At most 25 live (unrevoked) keys per user. Name is 1–64 characters, trimmed.
  - A key authenticates `/v1/mcp` only.
- **Org switch:** `organizations.mcp_enabled boolean NOT NULL DEFAULT true`. Only `org:manage` holders may change it.
- **Limits:**
  - trending: default 20 rows, max 50
  - `list_issue_events`: default 10 rows, max 50
  - stack frames: cap 50; breadcrumbs: last 20
  - `context_line` clip: 200 characters; tag and context values: clip 200 characters, max 30 entries
  - users-count statement timeout: 5000 ms; users-sort candidate pool: 200
  - `query` candidate cap: 10,000
  - rate limit: 120 tool calls per minute per key (config `MCP_CALLS_PER_MIN`)
- **Windows:**
  - `trending_issues`: `1h | 24h (default) | 7d | 14d | 30d`
  - `list_issue_events`, `list_releases`: `24h | 7d (default) | 14d | 30d`
  - `slow_operations`: `24h | 7d (default) | 30d | 90d`
- **Dependencies:** `rmcp = { version = "3.9", default-features = false, features = ["server", "macros", "transport-streamable-http-server"] }` and `schemars = "1"`. Bump workspace `rust-version` from `"1.82"` to `"1.88"` (rmcp's MSRV).
- **Migrations:** `2026-11-09-000083_api_keys_mcp_switch` and `2026-11-09-000084_issue_stats_hourly`, each with a `down.sql`. `crates/sauron-db/src/schema.rs` is hand-edited: table block, `joinable!`, and `allow_tables_to_appear_in_same_query!`.
- **Audit:** `changes` field names must not contain any `audit::FORBIDDEN_FIELDS` substring (`token`, `api_key`, `secret`, …). Use `name`, `prefix`, `expires_at`, `revoked_keys`, `mcp_enabled`.
- **Dashboard:** every user-visible string goes through the i18n catalog with both `en` and `ar`. Commands shown in `<code>` / `CodeBlock` are exempt.
- **Backend verification:**
  - Tests need both `TEST_DATABASE_URL` and `TEST_REDIS_URL` exported. A test binary reporting `finished in 0.00s` **skipped and proves nothing**; see memory `backend-tests-silently-skip`.
  - Clippy runs from `backend/` as `cargo +1.98.0 clippy --workspace --all-targets -- -D warnings`.
- **Backend test-file convention:** HTTP tests copy the `TestServer` from `bins/sauron-api/tests/http_sessions.rs` (there is no shared module).

## Review Focus

These five inputs the spec implies are the most likely to bite a user. Each has a pinned test in the task that owns it.

1. **One app slug in two orgs.** A user belongs to two orgs that both have `checkout/web`. `app: "checkout/web"` must return a tool error listing both `org/project/app` refs, and never silently pick one. Test: Task 9 `ambiguous_app_slug_lists_candidates`.
2. **Env-scoped member calls `trending_issues` with no `environment`.** They must see only their own environments' counts (`EnvFilter::Subset`), never app-wide numbers. Test: Task 9 `trending_env_scoped_grant_sees_only_its_env`.
3. **Issue with no hot event rows** (all archived to Parquet, or purged). `get_issue` must still return the issue summary, plus a note that the event body isn't in the hot store. It must not return a tool error. Test: Task 10 `get_issue_with_no_hot_events_still_returns_summary`.
4. **Key owner deactivated, then reactivated.** Calls are refused while deactivated and work again after reactivation, with no revocation. Test: Task 5 `deactivated_owner_is_refused_then_works_after_reactivation`.
5. **Pathological stack.** A 500-frame trace with in-app frames deep inside, plus a 50 KB minified `context_line`. Output stays bounded: in-app frames are kept, there is an "N frames omitted" line, and lines are clipped to 200 characters. Tests: Task 10 `stacktrace_truncates_to_50_keeping_in_app_frames` and `context_line_is_clipped`.

---

## File Structure

**Backend: new**
- `backend/migrations/2026-11-09-000083_api_keys_mcp_switch/{up,down}.sql`: the `api_keys` table and `organizations.mcp_enabled`.
- `backend/migrations/2026-11-09-000084_issue_stats_hourly/{up,down}.sql`: the rollup table, its epoch, the backfill marker, and the watermark seed.
- `backend/crates/sauron-auth/src/api_key.rs`: token minting and recognition (pure).
- `backend/crates/sauron-db/src/api_keys.rs`: key CRUD, authentication lookup, last-used throttling.
- `backend/crates/sauron-db/src/org_settings.rs`: `mcp_enabled` get and set.
- `backend/crates/sauron-db/src/rollups/issue_stats.rs`: fold rows, upsert, day replace, readiness, backfill, reads.
- `backend/bins/sauron-api/src/mcp/mod.rs`: builds the rmcp service and returns the `/v1/mcp` router.
- `backend/bins/sauron-api/src/mcp/auth.rs`: the `require_api_key` middleware and `McpCaller`.
- `backend/bins/sauron-api/src/mcp/server.rs`: `SauronMcp` (`ServerHandler`), instructions, and tool registration.
- `backend/bins/sauron-api/src/mcp/access.rs`: app-ref resolution, org-switch check, env parsing, read-scope authorization, `ToolError`, deep links.
- `backend/bins/sauron-api/src/mcp/format.rs`: text rendering for frames, breadcrumbs, key/value blocks and window parsing.
- `backend/bins/sauron-api/src/mcp/tools/{apps,issues,search,sessions,releases,performance}.rs`: one file per tool group.
- Tests:
  - `backend/crates/sauron-db/tests/api_keys.rs`
  - `backend/crates/sauron-db/tests/issue_stats.rs`
  - `backend/bins/sauron-api/tests/http_api_keys.rs`
  - `backend/bins/sauron-api/tests/http_mcp.rs`

**Backend: modified**
- `backend/Cargo.toml`: deps, MSRV.
- `bins/sauron-api/Cargo.toml`
- `crates/sauron-db/src/{schema.rs, models.rs, lib.rs, repo.rs, rollups/mod.rs, rollups/fold.rs}`
- `crates/sauron-auth/src/lib.rs`
- `crates/sauron-core/src/config.rs`: `MCP_CALLS_PER_MIN`.
- `crates/sauron-pipeline/src/rollup_task.rs`
- `bins/sauron-tier/src/main.rs`
- `bins/sauron-migrate/src/main.rs`
- `bins/sauron-api/src/{main.rs, openapi.rs, audit.rs}`
- `bins/sauron-api/src/routes/{account.rs, orgs.rs, projects.rs, environments.rs, issues.rs, sessions.rs, search.rs}`
- `bins/sauron-api/tests/audit_coverage.rs`
- `bins/sauron-api/tests/http_orgs.rs`

**Dashboard**
- New: `src/lib/components/account/ApiKeys.svelte`, `src/lib/components/members/OrgAiAccessCard.svelte`, `src/lib/mcp-command.ts`, `src/lib/mcp-command.test.ts`.
- Modified:
  - `src/lib/api/{account.ts, orgs.ts}`
  - `src/lib/models/index.ts`
  - `src/pages/Account.svelte`
  - `src/pages/admin/Members.svelte`
  - `src/lib/i18n/catalog/{account.ts, admin.ts}`
  - `vite.config.i18n-harness.mjs`

**Docs**
- New: `wiki/AI-Assistants-MCP.md`.
- Modified: `wiki/_Sidebar.md`, `wiki/Home.md`, `INSTALL.md`, `packaging/rpm/SETUP.md`.

---

### Task 1: Key storage and org switch column

**Files:**
- Create: `backend/migrations/2026-11-09-000083_api_keys_mcp_switch/up.sql`, `down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs`
  - add an `api_keys` block
  - add `mcp_enabled -> Bool` to `organizations` (:330-338)
  - add `joinable!(api_keys -> users (user_id))`
  - add `api_keys` to `allow_tables_to_appear_in_same_query!`
- Modify: `backend/crates/sauron-db/src/models.rs`
  - `Organization` (:93) gains `pub mcp_enabled: bool`
  - add `ApiKey` (Queryable + Selectable, **no `Serialize`**)
  - add `NewApiKey<'a>` (Insertable)
- Create: `backend/crates/sauron-db/src/api_keys.rs`, `backend/crates/sauron-db/src/org_settings.rs`. Register both in `lib.rs`.
- Create: `backend/crates/sauron-auth/src/api_key.rs`. Re-export from `lib.rs`.
- Test: `backend/crates/sauron-db/tests/api_keys.rs` (uses `mod common; common::TestDb`), plus unit tests in `api_key.rs`.

**Interfaces:**
- Produces, in `sauron_auth::api_key`:
  - `pub const API_KEY_PREFIX: &str = "sauron_pat_";`
  - `pub struct MintedKey { pub token: String, pub prefix: String, pub hash: String }`
  - `pub fn mint_api_key() -> MintedKey`
  - `pub fn looks_like_api_key(s: &str) -> bool`
- Produces, in `sauron_db::api_keys`:
  - `pub async fn insert(conn: &mut AsyncPgConnection, user_id: Uuid, name: &str, prefix: &str, hash: &str, expires_at: Option<DateTime<Utc>>) -> QueryResult<ApiKey>`
  - `pub async fn list_for_user(conn, user_id: Uuid) -> QueryResult<Vec<ApiKey>>`: unrevoked keys, newest first, expired ones included.
  - `pub async fn count_live_for_user(conn, user_id: Uuid) -> QueryResult<i64>`
  - `pub async fn revoke(conn, user_id: Uuid, key_id: Uuid) -> QueryResult<Option<ApiKey>>`: only the caller's own unrevoked key; returns the row as revoked.
  - `pub async fn revoke_all_for_user(conn, user_id: Uuid) -> QueryResult<usize>`
  - `pub struct KeyOwner { pub key_id: Uuid, pub user_id: Uuid, pub is_active: bool, pub must_change_password: bool, pub credentials_invalidated: bool }`
  - `pub async fn authenticate(conn, hash: &str) -> QueryResult<Option<KeyOwner>>`: unrevoked, unexpired keys only, joined to `users`.
  - `pub async fn touch_last_used(conn, key_id: Uuid) -> QueryResult<()>`: writes only when `last_used_at IS NULL OR last_used_at < now() - interval '60 seconds'`.
- Produces, in `sauron_db::org_settings`:
  - `pub async fn mcp_enabled(conn, org_id: Uuid) -> QueryResult<bool>`
  - `pub async fn set_mcp_enabled(conn, org_id: Uuid, enabled: bool) -> QueryResult<bool>`: returns the previous value.
- `ApiKey` fields: `id, user_id, name, token_prefix, token_hash, created_at, expires_at: Option<_>, last_used_at: Option<_>, revoked_at: Option<_>`.

- [ ] **Step 1: Write the migration.**
  - `up.sql`:
    - Create `api_keys` per the spec sketch. `token_hash text NOT NULL UNIQUE`; `user_id ... REFERENCES users(id) ON DELETE CASCADE`.
    - Add the partial index `api_keys_user_live_idx ON api_keys (user_id) WHERE revoked_at IS NULL`.
    - Add `ALTER TABLE organizations ADD COLUMN mcp_enabled boolean NOT NULL DEFAULT true;`.
  - `down.sql` reverses both.
- [ ] **Step 2: Write the failing tests.**
  - Unit tests in `api_key.rs`:

    ```rust
    #[test] fn minted_key_has_prefix_and_hash_matches() {
        let k = mint_api_key();
        assert!(k.token.starts_with("sauron_pat_") && k.token.len() == 11 + 64);
        assert_eq!(k.prefix, &k.token[..17]);
        assert_eq!(k.hash, crate::hash_token(&k.token));
    }
    #[test] fn looks_like_api_key_rejects_jwt() {
        assert!(!looks_like_api_key("eyJhbGciOiJIUzI1NiJ9.e30.x"));
        assert!(looks_like_api_key(&mint_api_key().token));
    }
    ```

  - DB tests in `tests/api_keys.rs`:
    - `authenticate_finds_live_key_and_rejects_revoked_and_expired`: `authenticate` returns `Some` for a fresh key, and `None` after `revoke` and for a key inserted with `expires_at = now - 1s`.
    - `revoke_is_scoped_to_owner`: `revoke(other_user, key.id)` returns `None`, and the key still authenticates.
    - `revoke_all_for_user_counts_only_live_keys`: 3 keys, one already revoked, gives `2`.
    - `authenticate_reports_owner_state`: after `repo::set_user_active(user, false)`, `owner.is_active == false`. After `repo::set_user_credentials_invalidated(user, Some(now))`, `credentials_invalidated == true`.
    - `touch_last_used_is_throttled`: first touch sets `last_used_at`; an immediate second touch leaves it equal.
    - `mcp_enabled_defaults_true_and_round_trips`: a new org gives `true`; `set_mcp_enabled(false)` returns `true`, and the next read gives `false`.
- [ ] **Step 3: Run and confirm failure.** `cd backend && cargo test -p sauron-auth api_key && cargo test -p sauron-db --test api_keys`. Expected: compile errors for the missing items.
- [ ] **Step 4: Implement** the items in the Interfaces block, plus the schema and model edits.
  - `Organization`'s `as_select()` call sites pick up the new column automatically, and `GET /v1/orgs` will then include `mcp_enabled`. That's intended.
- [ ] **Step 5: Run and confirm pass.**
  - Same commands as step 3, plus `cargo test -p sauron-db` to confirm existing suites still compile and pass.
  - Check durations are not `0.00s`.
- [ ] **Step 6: Leave changes in the working tree** (no commit).

---

### Task 2: Key management API (`/v1/me/api-keys`)

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/account.rs`: three handlers plus view types.
- Modify: `backend/bins/sauron-api/src/main.rs`: mount next to `/v1/me/sessions` (:552).
  - `.route("/v1/me/api-keys", get(routes::account::list_api_keys).post(routes::account::create_api_key))`
  - `.route("/v1/me/api-keys/{key_id}", delete(routes::account::delete_api_key))`
- Modify: `backend/bins/sauron-api/src/openapi.rs`: add the three paths to `paths(...)`, tag `"Account"`, `security(("bearerAuth" = []))`.
- Modify: `backend/bins/sauron-api/src/audit.rs`:
  - `entity::ACCESS_KEY = "access_key"`; add it to `entity::ALL` and bump the array length.
  - `action::ACCESS_KEY_CREATE = "access_key.create"`, `action::ACCESS_KEY_REVOKE = "access_key.revoke"`.
  - `allowlist(ACCESS_KEY) = &["name", "prefix", "expires_at"]`.
  - Add the family to the tests at :606 and :658.
- Modify: `backend/bins/sauron-api/tests/audit_coverage.rs`:
  - `AUDITED` += `"routes::account::create_api_key"`, `"routes::account::delete_api_key"`
  - `SOURCES` += `("routes::account", include_str!("../src/routes/account.rs"))`
- Test: `backend/bins/sauron-api/tests/http_api_keys.rs`: a new file; copy `TestServer` and `register_owner` from `http_sessions.rs`.

**Interfaces:**
- Consumes: Task 1's `sauron_auth::api_key::*` and `sauron_db::api_keys::*`.
- Produces (all `#[derive(Serialize, utoipa::ToSchema)]`):
  - `pub struct ApiKeyView { id: Uuid, name: String, prefix: String, created_at, expires_at: Option<_>, last_used_at: Option<_>, expired: bool }`
  - `pub struct CreateApiKeyReq { name: String, expiry: String }` (Deserialize + ToSchema)
  - `pub struct CreatedApiKey { key: ApiKeyView, token: String }`
- Handlers:
  - `pub async fn list_api_keys(auth: AuthUser, State(state): State<AppState>) -> Result<Json<Vec<ApiKeyView>>, ApiError>`
  - `pub async fn create_api_key(auth: AuthUser, State(state): State<AppState>, Json(req): Json<CreateApiKeyReq>) -> Result<(StatusCode, Json<CreatedApiKey>), ApiError>` returns 201, or:
    - 400 for a bad name or expiry
    - 409 `"you already have 25 active keys; revoke one first"`
    - 429 from `rate_limit(&state, &format!("sauron:auth:apikeys:{}", auth.user_id), SESSION_ACTIONS_PER_MIN, 60)`
  - `pub async fn delete_api_key(auth: AuthUser, State(state): State<AppState>, Path(key_id): Path<Uuid>) -> Result<Json<serde_json::Value>, ApiError>` returns `{"ok":true}`; a missing, foreign or already-revoked key gives 404. Same rate limit.
- Audit: create and revoke call `crate::audit::record_for_user_orgs(&mut conn, auth.user_id, Entry::new(Uuid::nil(), action, entity::ACCESS_KEY).target(key.id, &key.name).changes(created(entity::ACCESS_KEY, &[("name", ..), ("prefix", ..), ("expires_at", ..)])))`, after the write.

- [ ] **Step 1: Write the failing tests** in `http_api_keys.rs`. Each test opens with `let Some(mut server) = TestServer::start().await else { return; };`.
  - `create_returns_token_once_and_list_never_does`: POST `{"name":"laptop","expiry":"90d"}` gives 201.

    ```rust
    let token = body["token"].as_str().unwrap();
    assert!(token.starts_with("sauron_pat_"));
    let list = server.get_json("/v1/me/api-keys", Some(&jwt)).await;
    assert_eq!(list[0]["prefix"], &token[..17]);
    assert!(!list.to_string().contains(token));
    ```

  - `expiry_choices_validated`:
    - `"2d"` gives 400.
    - `"never"` gives `expires_at == null`.
    - `"30d"` gives `expires_at` within ±60 s of now + 30 days.
  - `blank_name_is_400`: `"   "` gives 400.
  - `delete_revokes_and_second_delete_is_404`.
  - `cannot_delete_another_users_key`: 404, and the key still lists for its owner.
  - `key_cap_is_25`: the 26th create gives 409.
  - `create_and_revoke_are_audited`: rows with `action IN ('access_key.create','access_key.revoke')` exist for the owner's org, and `changes` has no `token` key.
  - `api_key_is_rejected_on_rest_routes`: `GET /v1/me` with `Bearer <api key>` gives 401.
- [ ] **Step 2: Run and confirm failure.** `cargo test -p sauron-api --test http_api_keys`. Expected: 404s or compile errors.
- [ ] **Step 3: Implement** the handlers, routes, OpenAPI, audit vocabulary and coverage entries.
- [ ] **Step 4: Run and confirm pass.**
  - `cargo test -p sauron-api --test http_api_keys --test audit_coverage`
  - `cargo test -p sauron-api --bin sauron-api openapi` (`router_parity`, `only_the_allowlisted_operations_are_public`, `every_schema_reference_resolves`)
  - `cargo test -p sauron-api --bin sauron-api audit`
  - The http binary must not report `finished in 0.00s`.
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 3: Admin actions revoke a member's keys

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs`:
  - In `revoke_member_sessions` (:1374), after `revoke_sessions_for_user`.
  - In `reset_member_password` (:1493), in the **reset** branch only, after `revoke_sessions_for_user`.
- Modify: `backend/bins/sauron-api/src/audit.rs`: add `"revoked_keys"` to the `MEMBER` allowlist.
- Test: `backend/bins/sauron-api/tests/http_api_keys.rs` (extend; copy `seed_member` from `http_sessions.rs:325`).

**Interfaces:**
- Consumes: `sauron_db::api_keys::revoke_all_for_user`.
- Both handlers add `("revoked_keys", json!(n))` to their existing `audit::created(entity::MEMBER, ...)` changes. `revoke_member_sessions`'s JSON response gains `"revoked_keys": n`.
- `set_member_active` is **unchanged**: deactivation refuses keys at authentication time (Task 5) and does not revoke them.

- [ ] **Step 1: Write the failing tests.**
  - `admin_end_all_sessions_revokes_member_keys`: the member creates a key; the owner POSTs `/v1/orgs/{org}/members/{member}/revoke-sessions`; then `api_keys::authenticate(hash_token(&token))` is `None` and the response has `revoked_keys == 1`.
  - `admin_forced_reset_revokes_member_keys`: same, via the reset endpoint with `{"action":"reset"}`.
    - The test server needs `DASHBOARD_URL` and a mail config; follow `http_password_reset.rs`'s `TestServer` env.
  - `own_password_change_keeps_keys`: the member changes their own password via `/v1/auth/password`, and the key still authenticates.
- [ ] **Step 2: Run and confirm failure.** `cargo test -p sauron-api --test http_api_keys`.
- [ ] **Step 3: Implement** both call sites and the allowlist entry.
- [ ] **Step 4: Run and confirm pass.** `cargo test -p sauron-api --test http_api_keys --test http_password_reset --test http_sessions`. These must not report `0.00s`.
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 4: Org settings API (`/v1/orgs/{org_id}/settings`)

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs`: two handlers plus types.
- Modify: `backend/bins/sauron-api/src/main.rs`: `.route("/v1/orgs/{org_id}/settings", get(routes::orgs::get_org_settings).patch(routes::orgs::update_org_settings))` next to `/access` (:566).
- Modify: `openapi.rs` (tag `"Organizations"`).
- Modify: `audit.rs`: `action::ORG_SETTINGS_UPDATE = "org.settings_update"`; `ORG` allowlist becomes `&["name", "mcp_enabled"]`.
- Modify: `tests/audit_coverage.rs`: `AUDITED` += `"routes::orgs::update_org_settings"`.
- Test: `backend/bins/sauron-api/tests/http_orgs.rs` (extend; it has `seed_role` and `seed_member`).

**Interfaces:**
- Consumes: `sauron_db::org_settings::{mcp_enabled, set_mcp_enabled}`.
- Types:
  - `#[derive(Serialize, ToSchema)] pub struct OrgSettings { pub mcp_enabled: bool }`
  - `#[derive(Deserialize, ToSchema)] pub struct UpdateOrgSettings { pub mcp_enabled: Option<bool> }`
- Handlers:
  - `pub async fn get_org_settings(auth: AuthUser, State(state): State<AppState>, Path(org_id): Path<Uuid>) -> Result<Json<OrgSettings>, ApiError>`: any member (non-empty `repo::user_grants_in_org`), otherwise 403.
  - `pub async fn update_org_settings(auth: AuthUser, State(state): State<AppState>, Path(org_id): Path<Uuid>, Json(req): Json<UpdateOrgSettings>) -> Result<Json<OrgSettings>, ApiError>`:
    - Gated by `authorize_org(.., perm::ORG_MANAGE)`.
    - All fields `None` gives 400.
    - Audits `audit::diff(entity::ORG, &[("mcp_enabled", old, new)])` only when the value changed.

- [ ] **Step 1: Write the failing tests.**
  - `org_settings_default_mcp_enabled_true`
  - `org_settings_patch_requires_org_manage`: a member whose role lacks `org:manage` gets 403.
  - `org_settings_patch_is_audited_with_diff`: an `org.settings_update` row whose `changes` mentions `mcp_enabled`.
  - `org_settings_patch_same_value_writes_no_audit_row`
  - `org_settings_get_by_non_member_is_403`
  - `org_settings_empty_patch_is_400`
- [ ] **Step 2: Run and confirm failure.** `cargo test -p sauron-api --test http_orgs org_settings`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run and confirm pass.** Same command, plus `--test audit_coverage` and `--bin sauron-api openapi`.
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 5: The `/v1/mcp` endpoint and key authentication

**Files:**
- Modify: `backend/Cargo.toml`: `rust-version = "1.88"`; workspace deps `rmcp` and `schemars` per Global Constraints.
- Modify: `backend/bins/sauron-api/Cargo.toml`: add both.
- Modify: `backend/crates/sauron-core/src/config.rs`: `mcp_calls_per_min: u32` from `MCP_CALLS_PER_MIN`, default 120, minimum 1.
- Create: `backend/bins/sauron-api/src/mcp/{mod.rs, auth.rs, server.rs}`.
- Modify: `backend/bins/sauron-api/src/main.rs`:
  - add `mod mcp;`
  - add `.merge(mcp::router(state.clone()))` **before** `.layer(DefaultBodyLimit…)` (:1182), so the timeout, concurrency limit, security headers and trace layers apply.
- Test: `backend/bins/sauron-api/tests/http_mcp.rs`:
  - a new file; copy `TestServer` from `http_sessions.rs`
  - add `MCP_CALLS_PER_MIN` to its env (default 1000 in tests, 3 in the rate-limit test)
  - add an `mcp(&self, token: Option<&str>, method: &str, params: Value) -> (u16, Value)` helper

**Interfaces:**
- Consumes: Task 1's `api_keys::{authenticate, touch_last_used}` and `api_key::looks_like_api_key`; `crate::routes::auth::within_budget`.
- Produces:
  - `pub(crate) fn router(state: AppState) -> Router<AppState>`: `Router::new().nest_service("/v1/mcp", service).layer(axum::middleware::from_fn_with_state(state, auth::require_api_key))`.
    - Mounting with `nest_service` keeps `/v1/mcp` out of the `.route(` scanners used by `router_parity` and `audit_coverage`, which is intended. Add a `//` comment at the mount saying so.
  - `#[derive(Clone, Debug)] pub(crate) struct McpCaller { pub user_id: Uuid, pub key_id: Uuid }`
  - `pub(crate) async fn require_api_key(State(state): State<AppState>, mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response`
  - `#[derive(Clone)] pub(crate) struct SauronMcp { pub state: AppState, tool_router: ToolRouter<SauronMcp> }` with `impl ServerHandler` via `#[tool_handler]`. Server name `"sauron"`, version `env!("CARGO_PKG_VERSION")`.
  - `pub(crate) const INSTRUCTIONS: &str` must say:
    - Sauron is the team's error and analytics platform.
    - Call `list_apps` first.
    - Apps are referenced as `project/app` or `org/project/app` slugs, or by id.
    - `environment` takes a name.
    - Stack-frame paths are as the SDK reported them and may need mapping to the local repo.
    - Everything is read-only.
  - `pub(crate) fn caller(parts: &http::request::Parts) -> Result<McpCaller, rmcp::ErrorData>`: reads the extension that rmcp forwards (tool parameter `Extension(parts): Extension<http::request::Parts>`).
- Service config: `StreamableHttpServerConfig::default().with_legacy_session_mode(false).with_json_response(true).with_sse_keep_alive(None).disable_allowed_hosts().with_max_request_body_bytes(1024 * 1024)`.
  - Why `disable_allowed_hosts`: installs run behind arbitrary hostnames. Host checks guard unauthenticated local servers against DNS rebinding, and every request here needs a bearer key. Put this rationale in a comment.
- `require_api_key` responses:
  - 401 `invalid_token` for: a missing header, a non-`sauron_pat_` bearer (including a dashboard JWT), or an unknown, revoked or expired key. Every 401 carries `WWW-Authenticate: Bearer realm="sauron"`.
  - 403 `account_deactivated`, `password_change_required` or `password_reset_required` from `KeyOwner`.
  - 429 `rate_limited` when `within_budget(&state, &format!("sauron:mcp:calls:{key_id}"), cfg.mcp_calls_per_min, 60)` is false.
  - Otherwise `touch_last_used` runs (errors logged, never fatal), `McpCaller` is inserted, and the request passes on.
  - Bodies use `AuthError`'s `{"error":{code,message}}` shape.

- [ ] **Step 1: Write the failing tests** in `http_mcp.rs`.
  - The JSON-RPC helper sends `Accept: application/json, text/event-stream` and `Content-Type: application/json`. Mirror rmcp's `tests/test_streamable_http_json_response.rs` for the exact headers and the protocol-version header.
  - Tests:
    - `initialize_with_valid_key_returns_server_info_and_instructions`: `result.serverInfo.name == "sauron"`, and `result.instructions` contains `"list_apps"`.
    - `tools_list_responds`: 200 with `result.tools` an array.
    - `no_key_is_401_with_www_authenticate`
    - `dashboard_jwt_is_401`
    - `revoked_key_is_401`
    - `expired_key_is_401`: insert via `api_keys::insert` with `expires_at = now - 1s`.
    - `deactivated_owner_is_refused_then_works_after_reactivation`: `repo::set_user_active(false)` gives 403 `account_deactivated`; `set_user_active(true)` gives 200.
    - `pending_forced_reset_is_refused`: `repo::set_user_credentials_invalidated(Some(now))` gives 403.
    - `get_is_405`
    - `rate_limit_returns_429`: a server started with `MCP_CALLS_PER_MIN=3`; the 4th call gives 429.
    - `last_used_at_is_set_after_a_call`
- [ ] **Step 2: Run and confirm failure.** `cargo test -p sauron-api --test http_mcp`.
- [ ] **Step 3: Implement** the deps, config, `mcp/` module and mount. Confirm `cargo tree -p sauron-api -i axum` still shows a single axum 0.8.
- [ ] **Step 4: Run and confirm pass.**
  - `cargo test -p sauron-api --test http_mcp --test audit_coverage`
  - `cargo test -p sauron-api --bin sauron-api openapi`
  - `cargo +1.98.0 clippy --workspace --all-targets -- -D warnings`
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 6: Tool access layer and `list_apps`

**Files:**
- Create: `backend/bins/sauron-api/src/mcp/access.rs`, `mcp/format.rs`, `mcp/tools/mod.rs`, `mcp/tools/apps.rs`.
- Modify: `backend/bins/sauron-api/src/routes/projects.rs`: extract the bodies of `list_projects` (:39) and `list_apps` (:294). The handlers become `db()` + call + `Json`:
  - `pub(crate) async fn visible_projects(conn: &mut AsyncPgConnection, user_id: Uuid, org_id: Uuid) -> Result<Vec<Project>, ApiError>`
  - `pub(crate) async fn visible_apps(conn: &mut AsyncPgConnection, user_id: Uuid, project_id: Uuid) -> Result<Vec<App>, ApiError>`
- Modify: `backend/bins/sauron-api/src/routes/environments.rs`: extract from `list_app_environments` (:515): `pub(crate) async fn visible_app_environments(conn, user_id: Uuid, app_id: Uuid) -> Result<Vec<AppEnvironmentView>, ApiError>` (live environments only).
- Modify: `mcp/server.rs`: register `list_apps`.
- Test: `backend/bins/sauron-api/tests/http_mcp.rs` (extend).

**Interfaces:**
- Produces (`mcp::access`):
  - `pub(crate) struct AppTarget { pub app: App, pub project: Project, pub org: Organization }`
  - `pub(crate) enum ToolError { NotFound(String), Ambiguous(Vec<String>), NoAccess { perm: &'static str }, McpDisabled { org: String }, BadArgument(String), TimedOut(String), Internal(String) }`, with `impl From<ToolError> for CallToolResult` (`is_error: true`, one text block) and `From<ApiError>` / `From<AuthError>` / `From<diesel::result::Error>`.
  - `pub(crate) async fn resolve_app(conn, user_id: Uuid, app_ref: &str) -> Result<AppTarget, ToolError>`:
    - A UUID goes through `repo::app_ancestry`.
    - Otherwise `[org/]project/app` slugs are matched among `repo::list_orgs_for_user` orgs.
    - 0 matches gives `NotFound("… call list_apps")`; more than 1 gives `Ambiguous(refs)`.
    - Then `org_settings::mcp_enabled == false` gives `McpDisabled`.
    - An app in an org with no grant for the user gives `NotFound` (no existence oracle).
  - `pub(crate) async fn read_scope(conn, user_id: Uuid, target: &AppTarget, perm: &'static str, environment: Option<&str>) -> Result<(ReadScope, HashSet<String>), ToolError>`:
    - Environment `None` → `EnvFilter::All`.
    - `"none"` → `Unattributed`.
    - A UUID → `One` (an enrollment id).
    - Otherwise a name → `repo::enrollment_ids_for_env_name(conn, &[app_id], name)`. Zero hits gives `NotFound` listing the environment names the caller can see.
    - Then `sauron_auth::authorize_env_read_with_perms(conn, user_id, app_id, perm, filter)`. `Forbidden` maps to `NoAccess { perm }`, and a forbidden named environment maps to the same `NotFound` as an unknown name.
  - `pub(crate) fn release_arg(raw: Option<&str>) -> Result<ReleaseFilter, ToolError>`: `None` gives `All`, `"none"` gives `Unknown`, empty gives `BadArgument`.
  - `pub(crate) fn release_key_opt(r: &ReleaseFilter) -> Option<String>`: `All` gives `None`, `Unknown` gives `Some("")`, `One(r)` gives `Some(r)`. This is the shape the rollup reads take, the same mapping as `analytics.rs:1563 release_key`.
  - `pub(crate) async fn within_deadline<T>(state: &AppState, fut: impl Future<Output = Result<T, ToolError>>) -> Result<T, ToolError>`:
    - Wraps the tool body in `tokio::time::timeout(Duration::from_secs(cfg.api_request_timeout_secs.saturating_sub(5).max(1)), fut)`.
    - On elapse it returns `TimedOut("query timed out; narrow the window or add an environment")`, so the outer `TimeoutLayer` 503 is never what Claude sees.
    - Every tool body runs inside it.
  - `resolve_app` check order is fixed: (1) find the app (by ancestry or slug), (2) confirm the caller holds a grant in its org, else `NotFound`, (3) check `mcp_enabled`, else `McpDisabled`. That way `McpDisabled` is only ever shown to members of that org.
  - `pub(crate) fn deep_link(cfg: &Config, t: &AppTarget, hash_path: &str, env: &EnvFilter, release: &ReleaseFilter) -> Option<String>`:
    - `None` when `cfg.require_dashboard_url()` errors.
    - Format: `{base}/?org={org}&project={project}&app={app}[&env={uuid|none}][&release={urlencoded|none}]#{hash_path}`.
    - `env` is only set for `One` / `Unattributed`.
- Produces (`mcp::format`):
  - `pub(crate) fn parse_window(raw: Option<&str>, allowed: &[&str], default: &str) -> Result<chrono::Duration, ToolError>`: the error lists `allowed`.
  - `pub(crate) fn clip(s: &str, max: usize) -> String`: appends `…` when clipped, char-boundary safe.
- Produces (tool): `list_apps` with no arguments.
  - Output: one block per org, then per project, then per app.
  - Per app: its ref `org/project/app`, `app_type`, environment names (default marked) when `visible_app_environments` succeeds, the 3 most recent releases when the caller has `event:read`, and a link.
  - Never includes `public_key`.
  - Orgs with `mcp_enabled = false` are omitted, and a final line says `N organization(s) hidden: MCP access is turned off by an admin`.

- [ ] **Step 1: Write the failing tests.**
  - Unit tests in `format.rs`:
    - `parse_window_accepts_allowed_and_lists_them_on_error`
    - `clip_is_char_boundary_safe` (multi-byte input)
  - HTTP tests:
    - `list_apps_shows_only_reachable_apps`: an app-scoped grant on 1 of 2 apps lists only that one.
    - `list_apps_hides_orgs_with_mcp_disabled_and_says_so`
    - `list_apps_omits_ingest_public_key`: the output does not contain any `app_environments.public_key` value.
    - `deep_links_present_only_when_dashboard_url_set`: two servers, with and without `DASHBOARD_URL`.
    - `tools_list_includes_list_apps_read_only`: `annotations.readOnlyHint == true` and `openWorldHint == false`.
- [ ] **Step 2: Run and confirm failure.** `cargo test -p sauron-api --test http_mcp list_apps` and `cargo test -p sauron-api --bin sauron-api mcp::format`.
- [ ] **Step 3: Implement.** Existing handler tests must keep passing after the extraction.
- [ ] **Step 4: Run and confirm pass.** The same commands, plus `--test http_env_scoping --test http_orgs` (both exercise the extracted handlers).
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 7: `issue_stats_hourly` rollup (fold, rebuild, consistency, tier interlock)

**Files:**
- Create: `backend/migrations/2026-11-09-000084_issue_stats_hourly/{up,down}.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs`: add `issue_stats_hourly`, `issue_stats_epoch` and `issue_stats_backfill`, plus `allow_tables` entries.
- Create: `backend/crates/sauron-db/src/rollups/issue_stats.rs`.
- Modify: `backend/crates/sauron-db/src/rollups/mod.rs`:
  - `pub mod issue_stats;`
  - `pub const SRC_ISSUE_STATS: &str = "issue_stats";` next to `SRC_ERRORS` (:52). **Not** added to `EVENT_SOURCES`.
  - add the table to `analyze_rollup_tables` (:135).
- Modify: `backend/crates/sauron-db/src/rollups/fold.rs`:
  - add the pull instance and `fold_issue_stats`
  - add the rebuild call in `fold_day_body`'s `delete_first` block (:1213-1233)
  - add a 4th `consistency_check_day` entry (:1423-1439; array `3 → 4`)
- Modify: `backend/crates/sauron-pipeline/src/rollup_task.rs`: a drain loop for `fold_issue_stats` in `run_cycle`, after the `fold_errors` loop (:146-155).
- Modify: `backend/bins/sauron-tier/src/main.rs`: the export interlock at :597-612 also requires `range.end <= rollups::as_of(conn, &[SRC_ISSUE_STATS])`. Extract `fn export_allowed(range_end: DateTime<Utc>, events_as_of: Option<DateTime<Utc>>, issue_stats_as_of: Option<DateTime<Utc>>) -> bool` and unit-test it.
- Test: `backend/crates/sauron-db/tests/issue_stats.rs` (`mod common;`).

**Migration `up.sql` contents:**
- `issue_stats_hourly`, exactly as the spec sketch, with:
  - unique index `issue_stats_hourly_key ON issue_stats_hourly (app_id, hour, issue_id, release, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid))`
  - index `issue_stats_hourly_issue_idx ON (app_id, issue_id, hour)`
- `issue_stats_epoch (only_row boolean PRIMARY KEY DEFAULT true CHECK (only_row), started_at timestamptz NOT NULL DEFAULT now())`, followed by `INSERT INTO issue_stats_epoch DEFAULT VALUES`.
- `issue_stats_backfill (app_id uuid PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE, covered_from timestamptz NOT NULL, completed_at timestamptz NOT NULL DEFAULT now())`.
- `INSERT INTO rollup_watermarks (source, watermark) SELECT 'issue_stats', started_at FROM issue_stats_epoch`.
- `down.sql` drops all three tables and deletes the watermark row.

**Interfaces** (`sauron_db::rollups::issue_stats`):
- `pub struct IRow { pub app_id: Uuid, pub environment_id: Option<Uuid>, pub issue_id: Uuid, pub release: Option<String>, pub occurred_at: DateTime<Utc>, pub received_at: DateTime<Utc> }` (QueryableByName).
- `pub type IssueHourKey = (Uuid, Option<Uuid>, Uuid, String, DateTime<Utc>);` meaning (app, env, issue, release key, hour).
- `pub fn fold_issue_rows(rows: &[IRow]) -> HashMap<IssueHourKey, i64>`: uses `fold::hour_of` and `batch::release_key`.
- `pub async fn add_issue_stats(conn, deltas: &HashMap<IssueHourKey, i64>) -> QueryResult<()>`: an additive `INSERT … SELECT FROM unnest(...) ON CONFLICT (<key expr>) DO UPDATE SET events = issue_stats_hourly.events + EXCLUDED.events, updated_at = now()`, in chunks of `CHUNK` (1000).
- `pub async fn replace_day(conn, day: NaiveDate, app_id: Option<Uuid>, received_upto: DateTime<Utc>) -> QueryResult<u64>`:
  1. `DELETE` rows with `hour` inside the day (optionally for one app).
  2. `INSERT … SELECT app_id, issue_id, environment_id, COALESCE(release,''), date_trunc('hour', occurred_at, 'UTC'), count(*) FROM error_events WHERE occurred_at >= day AND occurred_at < day+1 AND received_at <= $received_upto [AND app_id = $app] GROUP BY 1,2,3,4,5`.
  3. Returns the number of rows inserted.
- In `fold.rs`:
  - `pull_source!(pull_issue_stats, IRow, "<SELECT app_id, environment_id, issue_id, release, occurred_at, received_at FROM error_events WHERE received_at > $1 AND received_at <= $2 AND occurred_at >= $3 ORDER BY received_at, id LIMIT $4>", "<eq variant on received_at = $1>")`
  - `pub async fn fold_issue_stats(conn, upto: DateTime<Utc>) -> QueryResult<Option<FoldOutcome>>`: the shape of `fold_errors`, with watermark `SRC_ISSUE_STATS`.
- Rebuild rule, in `fold_day_body`: call `issue_stats::replace_day(conn, day, None, watermark(conn, SRC_ISSUE_STATS))` **only when `delete_first && received_upto.is_none()`**. The base additive backfill path (`received_upto = Some(..)`, `delete_first = false`) never touches this table.
- Consistency entry, label `SRC_ISSUE_STATS`:
  - raw: `count(*)` from `error_events` in `[$1,$2)`
  - rolled: `COALESCE(sum(events),0)::bigint` from `issue_stats_hourly` with `hour >= $1 AND hour < $2`
  - Both are restricted to issue-stats-ready apps: `app_id IN (SELECT app_id FROM issue_stats_backfill UNION SELECT a.id FROM apps a, issue_stats_epoch e WHERE a.created_at >= e.started_at)`.

- [ ] **Step 1: Write the failing tests** in `tests/issue_stats.rs`.
  - Helper `open_issue_stats_gate(conn)`: `UPDATE issue_stats_epoch SET started_at = now() - interval '1 day'; UPDATE rollup_watermarks SET watermark = (SELECT started_at FROM issue_stats_epoch) WHERE source = 'issue_stats'`.
  - Use `db.seed_two_envs()` and an `error(conn, app, env, issue, release, at)` helper modeled on `tests/release_users.rs:96`.
  - Tests:
    - `fold_counts_match_raw_per_hour_env_release`:

      ```rust
      // A: 3× (env_a, "1.0", H), 2× (env_b, NULL, H); B: 1× (env_a, "1.0", H+1)
      assert_eq!(rows, vec![(a, Some(env_a), "1.0", h, 3), (a, Some(env_b), "", h, 2), (b, Some(env_a), "1.0", h1, 1)]);
      ```

    - `second_fold_reads_nothing`: the second `fold_issue_stats` returns `rows_read == 0`.
    - `rebuild_day_equals_fold`: `fold_day_from_raw(day, None, true, 2000)` leaves identical rows.
    - `base_additive_backfill_does_not_touch_issue_stats`: `fold_day_from_raw(day, Some(epoch), false, 2000)` leaves `issue_stats_hourly` unchanged.
    - `late_row_heals_via_consistency_rebuild`: a row with `occurred_at = now - 3d` and `received_at = now` is not pulled (occurred floor). `consistency_check_trailing` lists that day with label `issue_stats`, and after `fold_day_from_raw(day, None, true, …)` it is counted once.
    - `rebuild_is_bounded_by_issue_stats_watermark`: insert a row received after the watermark, rebuild (not counted), then fold (counted once).
  - Unit test in `sauron-tier`: `export_waits_for_issue_stats_watermark`.
- [ ] **Step 2: Run and confirm failure.** `cargo test -p sauron-db --test issue_stats`.
- [ ] **Step 3: Implement** the migration, schema, module, fold wiring, rollup task loop and tier interlock.
- [ ] **Step 4: Run and confirm pass.**
  - `cargo test -p sauron-db --test issue_stats --test rollup_equivalence --test rollup_tier_guard --test release_users`
  - `cargo test -p sauron-pipeline`
  - `cargo test -p sauron-tier`
  - No `0.00s` binaries.
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 8: Readiness and the `backfill-issue-stats` command

**Files:**
- Modify: `backend/crates/sauron-db/src/rollups/issue_stats.rs`.
- Modify: `backend/bins/sauron-migrate/src/main.rs`:
  - `COMMANDS` goes from `[Command; 6]` to `[Command; 7]` with `Command { name: "backfill-issue-stats", summary: "Build the per-issue hourly counts behind MCP trending for apps created before the upgrade (hot history only)" }`.
  - Add a dispatch block next to `backfill-release-users` (:277-334) that prints per-day progress.
- Modify: `INSTALL.md` and `packaging/rpm/SETUP.md`: an upgrade note next to the existing backfill commands. Mirror their exact invocation style.
- Test: `backend/crates/sauron-db/tests/issue_stats.rs` (extend).

**Interfaces:**
- `pub async fn epoch(conn) -> QueryResult<DateTime<Utc>>`
- `pub async fn is_ready(conn, app_id: Uuid) -> QueryResult<bool>`: true when a marker exists, or `apps.created_at >= epoch`.
- `pub async fn coverage_start(conn, app_id: Uuid) -> QueryResult<Option<DateTime<Utc>>>`: `marker.covered_from`, else `apps.created_at` if ready, else `None`.
- `pub async fn backfill_pending(conn) -> QueryResult<bool>`
- `pub async fn backfill_all(pool: &PgPool, progress: impl FnMut(Uuid, NaiveDate)) -> anyhow::Result<usize>`. For each app with `created_at < epoch` and no marker:
  1. `from_day` = the date of `min(occurred_at)` over the app's `error_events` (hot Postgres only). With no rows, `covered_from = epoch`.
  2. For each day from `from_day` to `today_utc`: `begin_locked` → `wm = watermark(SRC_ISSUE_STATS)` → `replace_day(conn, day, Some(app), wm)` → `finish`. That's one transaction per (app, day), so the rollup advisory lock is held only briefly.
  3. Insert the marker with `covered_from = from_day 00:00 UTC`.
  4. Return the count of apps done.
  - It is idempotent: a crashed run re-replaces days. Never call it from `auto_backfill.rs`, systemd units, RPM/deb scripts or compose.

- [ ] **Step 1: Write the failing tests.**
  - `apps_created_after_epoch_are_ready_without_backfill`
  - `old_app_not_ready_until_backfill_then_ready_with_coverage`: `coverage_start == from_day 00:00 UTC`.
  - `backfill_then_live_fold_counts_each_row_once`: rows received before and after the epoch, then backfill and fold. The sum of `events` equals the raw `count(*)`.
  - `backfill_is_idempotent`: run, delete the marker, run again; the counts are unchanged.
  - `backfill_ignores_rows_received_after_watermark_until_fold`
  - Existing `sauron-migrate` test `help_lists_every_command_exactly_once` still passes.
- [ ] **Step 2: Run and confirm failure.** `cargo test -p sauron-db --test issue_stats backfill`.
- [ ] **Step 3: Implement** the functions, command and docs.
- [ ] **Step 4: Run and confirm pass.** `cargo test -p sauron-db --test issue_stats` and `cargo test -p sauron-migrate`.
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 9: `trending_issues`

**Files:**
- Modify: `backend/crates/sauron-db/src/rollups/issue_stats.rs` (reads).
- Modify: `backend/crates/sauron-db/src/repo.rs`:
  - `issue_users_in_window`
  - `issues_by_ids`, unless an equivalent exists (grep `fn .*issues.*ids` first).
- Modify: `backend/bins/sauron-api/src/routes/issues.rs`: `issue_ids_matching`.
- Create: `backend/bins/sauron-api/src/mcp/tools/issues.rs`.
- Modify: `mcp/server.rs`.
- Test: `backend/bins/sauron-api/tests/http_mcp.rs` (extend). Tests open the issue-stats gate the way Task 7's helper does, and call `sauron_db::rollups::fold::fold_issue_stats(&mut conn, Utc::now() + 1s)` directly, because the fold runs in `sauron-ingest`.

**Interfaces:**
- `issue_stats`:
  - `pub struct RankedIssue { pub issue_id: Uuid, pub events: i64 }`
  - `pub async fn ranked(conn, scope: &ReadScope, release: Option<&str>, range: Range, status: Option<&str>, candidates: Option<&[Uuid]>, limit: i64) -> QueryResult<Vec<RankedIssue>>`:
    - `sum(events)` grouped by `issue_id`, joined to `issues` for the status filter.
    - Env via `scope.env.sql_fragment_for("h", …)`, release via `release = $n`.
    - `hour >= range.from.duration_trunc(1h)` and `hour < range.to.unwrap_or(now + 1h)`.
    - Ordered by `events DESC, issue_id`.
  - `pub async fn events_for(conn, scope: &ReadScope, release: Option<&str>, range: Range, ids: &[Uuid]) -> QueryResult<HashMap<Uuid, i64>>`
- `repo`:
  - `pub async fn issue_users_in_window(conn, scope: &ReadScope, release: Option<&str>, ids: &[Uuid], range: Range, timeout_ms: u32) -> QueryResult<Option<HashMap<Uuid, i64>>>`:
    - In a transaction with `SET LOCAL statement_timeout = {timeout_ms}`, run `count(DISTINCT distinct_id)` per issue over `error_events`, served by `error_events_issue_env_time_idx`.
    - On a query-canceled error, roll back and return `Ok(None)`.
  - `pub async fn issues_by_ids(conn, app_id: Uuid, ids: &[Uuid]) -> QueryResult<Vec<Issue>>`
- `routes::issues`:
  - `pub(crate) async fn issue_ids_matching(conn: &mut AsyncPgConnection, scope: &ReadScope, perms: &HashSet<String>, query: &str, release: &ReleaseFilter, since: DateTime<Utc>) -> Result<(Vec<Uuid>, bool), ApiError>`
  - It reuses `list`'s pipeline: `resolve_query` → `with_release` → `reject_withheld_dimensions` → `prepare` → `search_issues`, with `IssueSort::LastSeen` descending and limit 10,000. The `bool` is "capped".
- Tool `trending_issues` args (`schemars::JsonSchema`, doc comments become descriptions):
  - `TrendingArgs { app: String, window: Option<String>, sort: Option<String> /* events|users */, environment: Option<String>, release: Option<String>, status: Option<String> /* unresolved (default)|resolved|ignored|all */, query: Option<String>, limit: Option<u32> }`
- Tool behavior, in order:
  1. `resolve_app`, then `read_scope(ISSUE_READ, environment)`.
  2. If `!issue_stats::is_ready`, use the **fallback**: `repo::search_issues` sorted by `TimesSeen` / `UsersSeen` over the window. The first output line starts `Fallback ranking:` and names `sauron-migrate backfill-issue-stats`.
  3. `candidates = query.map(issue_ids_matching)`.
  4. `ranked(…, limit or 200 for sort=users)`.
  5. Previous window `[from - len, from)`: use `events_for`, but only if `coverage_start <= prev.from`. Otherwise show the note `previous window predates issue-stats coverage`.
  6. Users come from `issue_users_in_window(…, 5000)`.
     - `sort=users`: re-rank by users, truncate to `limit`, and note `ranked by users among the 200 issues with the most events`.
     - `sort=users` with a users timeout gives `TimedOut("counting users took too long; sort by events or narrow the window")`.
     - `sort=events` with a users timeout shows users as `?`.
  7. Render:
     - Header: `Trending issues in {org/project/app}, last {window}[, env {name}][, release {r}], by {events|users}`.
     - Per row: `#{n} {title}: {culprit}`, then a second line `id {issue_id} · {level} · {events} events ({trend}) · {users} users · first seen {YYYY-MM-DD}[ · NEW] · {link}`.
     - Trend:
       - previous count 0 → `new in window`
       - ratio ≥ 1.2 → `↑{r:.1}×`
       - ratio ≤ 0.8 → `↓{r:.1}×`
       - otherwise `≈`
     - NEW means `issues.first_seen >= window start`.
     - Footer notes: candidate cap, coverage.

- [ ] **Step 1: Write the failing tests.**
  - `trending_ranks_by_in_window_events_not_lifetime`: issue OLD (`times_seen = 1_000_000`, 1 event today) and issue NEW (5 events today). `NEW` appears before `OLD`.
  - `trending_flags_new_and_trend_vs_previous_window`: 2 events in the previous 24 h and 8 now gives `↑4.0×`. An issue first seen today shows `NEW`.
  - `trending_env_scoped_grant_sees_only_its_env`: a member granted only `env_a`. Events: 5 in `env_a`, 50 in `env_b`. The output shows `5 events` and never `55` or `50`.
  - `trending_falls_back_with_label_when_not_backfilled`: an app created before the epoch, with no marker. The output starts `Fallback ranking:`.
  - `trending_query_filters_candidates`: `query: "level:fatal"` lists only fatal issues.
  - `trending_users_sort_reranks_top_by_users`
  - `ambiguous_app_slug_lists_candidates`: two orgs, both with `checkout/web`. The tool result has `isError == true`, and its text contains both `org-a/checkout/web` and `org-b/checkout/web`.
  - `trending_refused_when_org_mcp_disabled`: `isError`, with text containing `turned off`.
  - `trending_bad_window_is_tool_error`: `"2w"` gives `isError`, with text listing `1h, 24h, 7d, 14d, 30d`.
  - `trending_without_issue_read_is_no_access`
- [ ] **Step 2: Run and confirm failure.** `cargo test -p sauron-api --test http_mcp trending`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run and confirm pass.** The same command, plus `--test http_list_filters --test http_search` (the shared issue pipeline).
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 10: `get_issue` and `list_issue_events`

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/issues.rs`:
  - Extract `pub(crate) async fn load_issue_scoped(state: &AppState, user_id: Uuid, app_id: Uuid, issue_id: Uuid, env: EnvFilter) -> Result<(PgConn, ReadScope, HashSet<String>, Issue), ApiError>`. The existing `load_issue` (:564) becomes `parse_env` + call.
  - Make `finish_latest_event` (:519) `pub(crate)`.
  - Extract `pub(crate) struct EventsParams { pub query: Option<String>, pub since: DateTime<Utc>, pub limit: i64, pub cursor: Option<String> }` and `pub(crate) async fn search_issue_events(state: &AppState, user_id: Uuid, app_id: Uuid, issue_id: Uuid, env: EnvFilter, release: ReleaseFilter, p: EventsParams) -> Result<SearchEnvelope<ErrorEvent>, ApiError>`. The `events` handler (:786) becomes parse + call. Symbolication, `strip_source_context` and `gate_event_body` stay inside.
- Modify: `backend/crates/sauron-db/src/repo.rs`:
  - `pub async fn error_event_by_id(conn, scope: &ReadScope, issue_id: Uuid, event_id: Uuid) -> QueryResult<Option<ErrorEvent>>`: must call `crate::stack_pool::hydrate`. Reuse an existing equivalent if grep finds one.
  - `pub struct OccurrenceBreakdown { pub by_release: Vec<(String, i64)>, pub by_environment: Vec<(Option<Uuid>, i64)>, pub by_os: Vec<(String, i64)> }`
  - `pub async fn occurrence_breakdown(conn, scope: &ReadScope, issue_id: Uuid, range: Range, release: Option<&str>) -> QueryResult<OccurrenceBreakdown>`: top 10 of each. Release is `COALESCE(release,'')`; OS is `COALESCE(context->'os'->>'name','')`. Confirm that path against one row inserted by the ingest pipeline test fixture before relying on it.
- Modify: `backend/bins/sauron-api/src/mcp/format.rs`:
  - `pub(crate) fn render_stacktrace(ev: &ErrorEvent, max_frames: usize) -> String`
  - `pub(crate) fn render_breadcrumbs(v: &serde_json::Value, last_n: usize) -> String`
  - `pub(crate) fn render_kv(v: &serde_json::Value, max_entries: usize, max_len: usize) -> String`
- Modify: `backend/bins/sauron-api/src/mcp/tools/issues.rs`, `mcp/server.rs`.
- Test: unit tests in `format.rs`; HTTP tests in `http_mcp.rs`.

**Interfaces and rules:**
- `render_stacktrace`:
  - Uses `stacktrace_symbolicated` when non-null, else `stacktrace`.
  - Frame order and the header label (`most recent call first` / `last`) match what `dashboard/src/lib/components/` stack-trace rendering shows; read it first.
  - Line format: `  at {function} ({filename}:{lineno}:{colno})[ [app]]`, then, when `context_line` is present, `      > {clip(context_line, 200)}`.
  - Above 50 frames: keep every `in_app` frame (up to 50), fill the rest with the frames nearest the crash, and emit `  … {n} frames omitted` where frames were dropped.
- `get_issue` args: `GetIssueArgs { app: String, issue_id: String, event_id: Option<String>, environment: Option<String> }`. Output:
  - Issue summary: title, culprit, level, status, first and last seen, `times_seen` and `users_seen` (env-derived by `get_issue`).
  - The event (latest via `latest_error_event`, or `error_event_by_id`), passed through `finish_latest_event`: exception type and value, release, environment name, `occurred_at`, device/OS/runtime from `context`, `session_id` (with "use get_session_timeline"), stack, breadcrumbs (last 20), tags (30 entries), and a link `#/issues/{id}`.
  - Withheld notices, verbatim:
    - `Event details withheld: your role lacks event:read (stack trace, breadcrumbs, tags, user).` when `!may_read_event_body`.
    - `Source lines withheld: your role lacks source:read.` when there is a body but no `source:read`.
  - No event row gives the summary plus `No event body in the hot store (older events are archived or were purged).`
- `list_issue_events` args: `ListIssueEventsArgs { app: String, issue_id: String, window: Option<String>, environment: Option<String>, release: Option<String>, query: Option<String>, limit: Option<u32>, cursor: Option<String> }`. Output:
  - The breakdown block first.
  - Then one line per event: `{event_id} · {occurred_at} · release {r} · env {name} · {os} [· user {distinct_id} when body allowed]`.
  - A final `more results: pass cursor="{next_cursor}"` when there is one.

- [ ] **Step 1: Write the failing tests.**
  - Unit tests:
    - `stacktrace_truncates_to_50_keeping_in_app_frames`: 500 frames with `in_app` at indexes 400, 401 and 402. The output contains all 3 filenames, `frames omitted`, and at most 51 `at ` lines.
    - `context_line_is_clipped`: a 50,000-character `context_line` gives a rendered line of at most 200 + 9 characters, ending with `…`.
    - `symbolicated_frames_preferred`
    - `render_kv_caps_entries_and_value_length`
  - HTTP tests:
    - `get_issue_returns_frames_with_file_and_line`
    - `get_issue_withholds_body_without_event_read`: role `["issue:read","app:read"]`. The output contains `Event details withheld` and no frame filename.
    - `get_issue_by_event_id`
    - `get_issue_with_no_hot_events_still_returns_summary`: an issue row with zero `error_events`. Not `isError`, and the text contains `No event body in the hot store`.
    - `list_issue_events_breakdown_by_release_env_os`
    - `list_issue_events_paginates_with_cursor`: `limit 2` over 3 events. The second page holds the 3rd event.
- [ ] **Step 2: Run and confirm failure.** `cargo test -p sauron-api --bin sauron-api mcp::format` and `cargo test -p sauron-api --test http_mcp issue`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run and confirm pass.** The same commands, plus `--test http_source_context --test http_env_scoping --test http_release_scoping` (the refactored handlers).
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 11: `describe_search`, `get_session_timeline`, `list_releases`, `slow_operations`

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/sessions.rs`:
  - Extract `pub(crate) async fn load_session_scoped(state: &AppState, user_id: Uuid, app_id: Uuid, session_id: &str, env: EnvFilter) -> Result<(PgConn, ReadScope, HashSet<String>, Session), ApiError>`. `load_session` (:351) becomes parse + call.
  - Make `session_timeline` (:393) `pub(crate)`.
- Modify: `backend/bins/sauron-api/src/routes/search.rs`: make `load_tag_keys` (:1109) `pub(crate)`.
- Modify: `backend/crates/sauron-db/src/rollups/issue_stats.rs`:
  - `pub struct ReleaseErrors { pub release: String, pub events: i64, pub issues: i64 }`
  - `pub async fn release_error_counts(conn, scope: &ReadScope, range: Range) -> QueryResult<Vec<ReleaseErrors>>`
  - `pub async fn new_issues_by_release(conn, scope: &ReadScope, coverage_from: DateTime<Utc>) -> QueryResult<HashMap<String, i64>>`: for each issue with `issues.first_seen >= coverage_from`, take the release of its earliest `issue_stats_hourly` row, then count per release.
- Create: `backend/bins/sauron-api/src/mcp/tools/{search.rs, sessions.rs, releases.rs, performance.rs}`.
- Modify: `mcp/server.rs`.
- Test: `http_mcp.rs`.

**Interfaces:**
- `describe_search { app: String, context: Option<String> /* issues (default) | occurrences */ }`:
  - `read_scope(EVENT_READ)` (same as the dashboard handler), then `build_schema_response(ctx, resource, load_tag_keys(..))`.
  - Output: dimensions (`name · type · ops · options`), tags with up to 5 sample values, and 3 example queries built from real dimension names.
- `get_session_timeline { app: String, session_id: String, environment: Option<String> }`:
  - `read_scope(EVENT_READ)`, then `load_session_scoped` + `session_timeline` (which gates bodies).
  - Output: a header (started, duration, release, `errors_count`, device from `context`), then up to 200 lines:
    - `{HH:MM:SS.mmm} event {name} {clip(props,200)}`
    - `{…} error {exception_type}: {clip(value,200)} (issue {issue_id})`
    - `{…} txn {op} {name} {duration_ms}ms`
  - Plus `older rows archived; timeline may be incomplete` when `archive_incomplete`.
- `list_releases { app: String, window: Option<String>, environment: Option<String>, limit: Option<u32> /* 20 */ }`:
  - `read_scope(EVENT_READ)` and `sauron_db::releases::list_for_app`.
  - Error columns (events, issues, new issues) appear only when perms include `issue:read` **and** `issue_stats::is_ready`. Otherwise one note line names which condition failed.
  - Sorted by `last_seen_at` descending.
- `slow_operations { app: String, window: Option<String>, environment: Option<String>, op: Option<String>, limit: Option<u32> /* 20 */ }`:
  - `read_scope(EVENT_READ)`, then `repo::performance_summary(conn, scope, Range::since(now - window), op, None)`.
  - Rows are sorted by `p95` descending, keyed by the `(name, op)` pair: `{op} {name} · p50 {ms} · p95 {ms} · {count} calls · {error_rate:.1}% errors`.

- [ ] **Step 1: Write the failing tests.**
  - `describe_search_lists_dimensions_and_tags`
  - `session_timeline_orders_rows_and_gates_bodies`: a role without `event:read` gets `NoAccess`. With it, the rows are in time order.
  - `list_releases_shows_error_counts_when_ready_and_omits_when_not`
  - `slow_operations_sorted_by_p95_and_filters_op`
  - `tools_list_has_all_eight_read_only_tools`:

    ```rust
    let names: BTreeSet<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, BTreeSet::from(["describe_search","get_issue","get_session_timeline","list_apps","list_issue_events","list_releases","slow_operations","trending_issues"]));
    assert!(tools.iter().all(|t| t["annotations"]["readOnlyHint"] == true && t["annotations"]["openWorldHint"] == false));
    ```

- [ ] **Step 2: Run and confirm failure.** `cargo test -p sauron-api --test http_mcp`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run and confirm pass.**
  - `cargo test -p sauron-api --test http_mcp --test http_sessions --test http_sessions_search --test http_search_schema`
  - the full `cargo test --workspace`
  - clippy (`+1.98.0`)
  - No `0.00s` binaries.
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 12: Dashboard: API keys on the Account page

**Files:**
- Create: `dashboard/src/lib/mcp-command.ts`, `dashboard/src/lib/mcp-command.test.ts`.
- Create: `dashboard/src/lib/components/account/ApiKeys.svelte`. Model it on `components/account/NotificationSubscriptions.svelte`.
- Modify:
  - `dashboard/src/lib/api/account.ts`: `listApiKeys`, `createApiKey`, `revokeApiKey`.
  - `dashboard/src/lib/models/index.ts`: `ApiKey`, `CreatedApiKey`, `ApiKeyExpiry`.
  - `dashboard/src/pages/Account.svelte`: mount `<ApiKeys />` after the sessions card (:406).
  - `dashboard/src/lib/i18n/catalog/account.ts`: `account.apiKeys.*` keys, en + ar.
  - `dashboard/vite.config.i18n-harness.mjs`: stub `GET/POST /v1/me/api-keys` and `DELETE /v1/me/api-keys/:id`.

**Interfaces:**
- `export function mcpAddCommand(apiBaseUrl: string, token: string, origin: string): string` returns `claude mcp add --transport http sauron ${url} --header "Authorization: Bearer ${token}"`, where `url = new URL('v1/mcp', new URL(withTrailingSlash(apiBaseUrl), origin)).href`.
- `export type ApiKeyExpiry = '30d' | '90d' | '1y' | 'never'`
- `export interface ApiKey { id: string; name: string; prefix: string; created_at: string; expires_at: string | null; last_used_at: string | null; expired: boolean }`
- `export interface CreatedApiKey { key: ApiKey; token: string }`
- API functions:
  - `listApiKeys(): Promise<ApiKey[]>`
  - `createApiKey(name: string, expiry: ApiKeyExpiry): Promise<CreatedApiKey>`
  - `revokeApiKey(id: string): Promise<void>`
- `ApiKeys.svelte`:
  - A `Card` titled `t('account.apiKeys.title')`, whose `actions` snippet holds a "Create key" `Button`.
  - One line of explanation: read-only, uses your permissions, your admin can turn it off; changing your password does not revoke keys, but an admin ending your sessions does.
  - Data: `CachedView` + `viewKey('account.apiKeys', …)`. Keep the page's `RefreshButton` parity.
  - States: `Skeleton` / `ErrorState size="section"` / `EmptyState size="section"`.
  - `DataTable` columns: name, prefix (`<code>`), created (`TimeValue`), last used (`TimeValue` or "never"), expires ("never", a date, or an "expired" `Badge`), and a Revoke button (`ConfirmDialog danger`).
  - Create flow: a `Modal` with `Input` name and a `Select` expiry (default `90d`). On success the same modal switches to reveal mode, following the `CreateMemberDialog.svelte:110-136` pattern:
    - `dismissible={false}` until "Done"
    - `InlineNotice tone="warning"` one-time-secret text
    - `CodeBlock code={mcpAddCommand(apiBaseUrl, token, location.origin)} language="bash"`
    - `CopyButton value={token}`
  - On close: `viewCache.invalidate` + reload.

- [ ] **Step 1: Write the failing test** `mcp-command.test.ts`:

  ```ts
  expect(mcpAddCommand('https://s.example.com/api', 'sauron_pat_x', 'https://d')).toBe('claude mcp add --transport http sauron https://s.example.com/api/v1/mcp --header "Authorization: Bearer sauron_pat_x"');
  expect(mcpAddCommand('https://s.example.com/api/', 't', 'https://d')).toContain('https://s.example.com/api/v1/mcp ');
  expect(mcpAddCommand('/sauron-api', 't', 'https://h.example')).toContain('https://h.example/sauron-api/v1/mcp ');
  ```

- [ ] **Step 2: Run and confirm failure.** `cd dashboard && npx vitest run src/lib/mcp-command.test.ts`.
- [ ] **Step 3: Implement** the helper, API functions, types, component, catalog keys and harness stubs.
- [ ] **Step 4: Verify.**
  - `npm run check` (0 errors) and `npm test` (`leak.test.ts`, `catalog.test.ts`, `refresh-parity.test.ts`, `css-guards.test.ts` all green).
  - Start the i18n harness (`npx vite --config vite.config.i18n-harness.mjs`). In the Browser pane, check `#/account` in English and with `?lang=ar`: create a key, see the reveal modal and command, revoke it. Take screenshots.
  - The pane must be visible; synthetic clicks do nothing in a hidden pane (memory `dashboard-browser-drive`).
- [ ] **Step 5: Leave changes in the working tree.**

---

### Task 13: Dashboard: org AI-access switch

**Files:**
- Create: `dashboard/src/lib/components/members/OrgAiAccessCard.svelte`.
- Modify:
  - `dashboard/src/lib/api/orgs.ts`: `getOrgSettings`, `updateOrgSettings`.
  - `dashboard/src/lib/models/index.ts`: `OrgSettings`; `Organization` gains `mcp_enabled: boolean`.
  - `dashboard/src/pages/admin/Members.svelte`: mount the card first inside `.stack` (:519).
  - `dashboard/src/lib/i18n/catalog/admin.ts`: `members.aiAccess.*`.
  - `dashboard/vite.config.i18n-harness.mjs` and `vite.config.perm-harness.mjs`: stub `GET/PATCH /v1/orgs/:id/settings`.

**Interfaces:**
- `getOrgSettings(orgId: string): Promise<OrgSettings>`
- `updateOrgSettings(orgId: string, patch: Partial<OrgSettings>): Promise<OrgSettings>`
- `export interface OrgSettings { mcp_enabled: boolean }`
- The card:
  - `Card` titled `t('members.aiAccess.title')`.
  - A native `<input type="checkbox">` labeled `t('members.aiAccess.toggle')`, meaning "Allow members to connect AI assistants (MCP)".
  - Description `t('members.aiAccess.body')`: when on, members can use personal API keys to let Claude read this organization's data with their own permissions, and error data sent to the assistant can include end-user details such as emails and IP addresses.
  - Locked with `lockedBy('org:manage', { level: 'org' })`: disabled, plus `use:lockTip`.
  - A change saves immediately with `toastStore.success`. On error it reverts the checkbox and shows `toastStore.error(errorMessage(err))`.
  - It reads `sessionStore.currentOrgId`.

- [ ] **Step 1: Implement** (no unit-testable pure logic; covered by the checks below).
- [ ] **Step 2: Verify.**
  - `npm run check` and `npm test` are green.
  - In the perm-harness, a user without `org:manage` sees the checkbox disabled with a lock tip.
  - In the i18n harness, toggling persists through the stub, in both `en` and `ar`. Take screenshots.
- [ ] **Step 3: Leave changes in the working tree.**

---

### Task 14: Docs and live acceptance

**Files:**
- Create: `wiki/AI-Assistants-MCP.md`. Style: `wiki/Privacy-Inspector.md`, a bold lead, then a numbered flow, then `---` and `##` sections. Sections:
  - What it is
  - Create a key
  - Connect Claude Code (the command)
  - Tools (a table of the 8 tools, one line each)
  - Organization switch
  - What data leaves your org
  - Revoking keys, including which admin actions revoke them
  - Trending and the backfill (`sauron-migrate backfill-issue-stats`; the meaning of `Fallback ranking:`)
  - Troubleshooting:
    - 401, 403 and 429 meanings
    - the proxy must forward the `Authorization` header and allow `POST /v1/mcp`
    - the claude.ai web and desktop chat connectors are not supported, because they need OAuth
- Modify: `wiki/_Sidebar.md` (under **Guides**, after Privacy Inspector), `wiki/Home.md` (Guides list), `INSTALL.md` (a link to the wiki page).

- [ ] **Step 1: Write the docs.**
- [ ] **Step 2: Live acceptance.**
  - Run the local stack. Check memory `local-e2e-api-drive` and `dashboard-points-at-stale-api` first.
  - Seed or reuse an app with errors whose frames point into `examples/svelte-web`.
  - In the dashboard, create a key and copy the command. Run it (`claude mcp add …`) in a Claude Code session opened in `examples/svelte-web`. Ask: "What are the top errors in <app> this week, and why does the top one happen?"
  - Pass when Claude:
    - calls `list_apps`, `trending_issues` and `get_issue` without errors
    - opens the local file named by a frame
    - names the cause
  - Record the transcript excerpt and screenshots in the hand-off summary.
- [ ] **Step 3: Final gate.**
  - `cd backend && cargo test --workspace` (no `0.00s` http binaries)
  - `cargo +1.98.0 clippy --workspace --all-targets -- -D warnings`
  - `cd dashboard && npm run build && npm test`
- [ ] **Step 4: Leave changes in the working tree.** Report the follow-ups listed in the spec's non-goals.
