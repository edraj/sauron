# Member Lifecycle & Role Editing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user with `member:manage` create and deactivate members directly, and let a user with `role:manage` edit a member's grant and a custom role's permissions — all from the Members page.

**Architecture:** Two new `users` columns (`is_active`, `must_change_password`) plus four new API endpoints. All authorization logic is extracted into pure functions in a new `sauron-auth::guard` module so it is unit-testable without a database; handlers stay thin (fetch → guard → persist). A temp password grants no capability except replacing itself, enforced by a JWT claim checked in the auth extractor rather than by UI routing.

**Tech Stack:** Rust (axum 0.8, diesel-async, Argon2, jsonwebtoken), PostgreSQL, Svelte 5 (runes), TypeScript, Vite, vitest.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-26-member-lifecycle-design.md`. Read it before Task 1.
- **No DB-backed tests.** CI runs `cargo test --workspace` (`.github/workflows/ci.yml:59`) with no Postgres service. Every test in this plan is a pure unit test. DB-dependent behavior is verified manually in Task 16.
- **Never create a git branch.** Work stays on the current branch, `soheyb`. The user has explicitly authorized **one commit per task** on that branch for this plan (2026-07-26). Steps labelled "Stage" mean: `git add` the listed paths, then `git commit` with a `feat:`/`refactor:`/`test:` message naming the task. Do not push, do not merge, do not touch `main`.
- **Scope validation is a shared helper, not copy-paste.** Per the user's 2026-07-26 pre-flight decision, `create_grant`, `create_member` (Task 5), and `update_grant_handler` (Task 8) all call `validate_scope_in_org` (added in Task 4 Step 3a). The org-containment check is a cross-tenant boundary and must have exactly one implementation.
- Admins may never set a durable password for another user. Only system-generated temp passwords, always paired with `must_change_password = true`.
- Permission strings must come from `perm::ALL` (`backend/crates/sauron-auth/src/rbac.rs:56`), exactly 23 entries, canonical order.
- System preset roles (`is_system = true`) are read-only. They are re-synced at every API boot by `ensure_preset_roles` (`rbac.rs:347`), so edits would not survive a restart.
- Migration timestamp prefix: `2026-07-26-000023`. The previous migration is `2026-07-25-000022_refresh_revoke_reason`.
- Dashboard: use house UI components (`Button`, `DataTable`, `Modal`, `Badge`), never raw `<button>` / `<table>`. Svelte 5 runes (`$state`, `$derived`, `$props`), not Svelte 4 syntax.
- Run `cargo fmt` and `cargo clippy --workspace -- -D warnings` before each backend staging point; `npm run check` before each frontend one.

## File Structure

**New files**

| File | Responsibility |
|---|---|
| `backend/migrations/2026-07-26-000023_member_lifecycle/up.sql` | Add the two `users` columns |
| `backend/migrations/2026-07-26-000023_member_lifecycle/down.sql` | Drop them |
| `backend/crates/sauron-auth/src/guard.rs` | Pure guard functions + their unit tests. No DB, no axum state |
| `dashboard/src/lib/models/permissions.ts` | The 23-permission catalog, grouped by resource |
| `dashboard/src/lib/models/permissions.test.ts` | Parity test against the backend catalog |
| `dashboard/src/lib/models/group-members.test.ts` | Tests for `groupMembers` |
| `dashboard/src/lib/components/members/CreateMemberDialog.svelte` | Create form + one-time temp-password reveal |
| `dashboard/src/lib/components/members/EditMemberDialog.svelte` | Change a member's role/scope; add a grant |
| `dashboard/src/lib/components/members/RoleEditorDialog.svelte` | Create **and** edit a role; read-only for presets |
| `dashboard/src/lib/components/members/PermissionPicker.svelte` | Checkbox grid grouped by resource |
| `dashboard/src/pages/ChangePassword.svelte` | Forced change screen |

**Modified files**

| File | Change |
|---|---|
| `backend/crates/sauron-db/src/schema.rs:228` | Two columns on `users` |
| `backend/crates/sauron-db/src/models.rs:74` | Two fields on `User` |
| `backend/crates/sauron-db/src/repo.rs` | New queries + `REVOKE_DEACTIVATED` |
| `backend/crates/sauron-auth/src/lib.rs` | `pub mod guard;` + re-exports |
| `backend/crates/sauron-auth/src/jwt.rs:14` | `must_change_password` claim |
| `backend/crates/sauron-auth/src/extractors.rs` | Two new `AuthError` variants + the gate |
| `backend/bins/sauron-api/src/routes/orgs.rs` | Refactor to `guard`, then 4 handlers |
| `backend/bins/sauron-api/src/routes/auth.rs` | `is_active` checks, change-password |
| `backend/bins/sauron-api/src/main.rs:149-167` | Route registrations |
| `dashboard/src/lib/models/index.ts` | `Member` type, `MemberGrant.is_active` |
| `dashboard/src/lib/api/orgs.ts` | 4 client functions |
| `dashboard/src/lib/api/auth.ts` | `changePassword` |
| `dashboard/src/pages/Members.svelte` | Split out dialogs, regroup table |
| `dashboard/src/routes.ts` | `/change-password` route |
| `dashboard/src/pages/Docs.svelte:1127` | Document the flow |

**Task dependency order:** 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 (backend, sequential — 4-9 all touch `orgs.rs`), then 10 → 11 → (12, 13, 14 parallel) → 15 → 16.

---

### Task 1: Migration + model columns

Adds `is_active` and `must_change_password` to `users`. Nothing reads them yet — this task only has to compile and leave existing behavior unchanged.

**Files:**
- Create: `backend/migrations/2026-07-26-000023_member_lifecycle/up.sql`
- Create: `backend/migrations/2026-07-26-000023_member_lifecycle/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs:228-237`
- Modify: `backend/crates/sauron-db/src/models.rs:74-91`

**Interfaces:**
- Consumes: nothing.
- Produces: `User.is_active: bool`, `User.must_change_password: bool`; `users::is_active` and `users::must_change_password` diesel columns. Task 2 and Task 3 depend on both.

- [ ] **Step 1: Write the migration up**

Create `backend/migrations/2026-07-26-000023_member_lifecycle/up.sql`:

```sql
-- Account-level member lifecycle.
--
-- is_active is a login kill switch that deliberately leaves role_grants intact.
-- Deactivating by deleting grants would make the row vanish from every members
-- list while the account kept holding its slot in users_email_lower_key, with
-- no UI path back. Keeping the grants keeps the member visible, badged, and
-- reversible.
--
-- must_change_password marks an account whose password was generated by an
-- admin rather than chosen by its owner. The API refuses every request from
-- such an account except the password change itself, so a temp credential can
-- do nothing but replace itself.
ALTER TABLE users ADD COLUMN is_active BOOLEAN NOT NULL DEFAULT true;
ALTER TABLE users ADD COLUMN must_change_password BOOLEAN NOT NULL DEFAULT false;
```

- [ ] **Step 2: Write the migration down**

Create `backend/migrations/2026-07-26-000023_member_lifecycle/down.sql`:

```sql
ALTER TABLE users DROP COLUMN must_change_password;
ALTER TABLE users DROP COLUMN is_active;
```

- [ ] **Step 3: Update the diesel schema**

In `backend/crates/sauron-db/src/schema.rs`, the `users` table block (line 228) becomes:

```rust
diesel::table! {
    users (id) {
        id -> Uuid,
        email -> Text,
        password_hash -> Text,
        name -> Text,
        last_login_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        is_active -> Bool,
        must_change_password -> Bool,
    }
}
```

Column order must match the physical table (appended columns go last), or diesel's `Queryable` derive will bind the wrong fields.

- [ ] **Step 4: Update the User model**

In `backend/crates/sauron-db/src/models.rs`, the `User` struct (line 74):

```rust
pub struct User {
    pub id: Uuid,
    pub email: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub name: String,
    pub last_login_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_active: bool,
    pub must_change_password: bool,
}
```

Both new fields serialize — unlike `password_hash` — because the dashboard reads `must_change_password` from the login response to trigger the forced-change redirect, and `is_active` to render the badge.

`NewUser` is unchanged: both columns have defaults, and Task 4 sets `must_change_password` with an explicit update rather than widening the insert struct.

- [ ] **Step 5: Verify it compiles**

Run: `cd backend && cargo check --workspace`
Expected: success. Field-order mismatches surface here as a `Queryable` trait error on `User`.

- [ ] **Step 6: Apply the migration locally**

Run: `cd backend && cargo run --bin sauron-migrate`
Expected: applies `2026-07-26-000023_member_lifecycle`, exits 0.

Confirm: `psql "$DATABASE_URL" -c '\d users'`
Expected: `is_active | boolean | not null default true` and `must_change_password | boolean | not null default false`.

- [ ] **Step 7: Stage**

```bash
git add backend/migrations/2026-07-26-000023_member_lifecycle backend/crates/sauron-db/src/schema.rs backend/crates/sauron-db/src/models.rs
```

Report staged; do not commit unless asked.

---

### Task 2: The `guard` module

Pure authorization logic, extracted so it can be tested without a database. This task is test-first and adds no behavior change — Task 3 rewires `orgs.rs` to call it.

**Files:**
- Create: `backend/crates/sauron-auth/src/guard.rs`
- Modify: `backend/crates/sauron-auth/src/lib.rs`
- Modify: `backend/crates/sauron-auth/Cargo.toml`

**Interfaces:**
- Consumes: `AuthError` from `crate::extractors`.
- Produces, all `pub` from `sauron_auth::guard`:
  - `role_permissions(perms: &serde_json::Value) -> Vec<String>`
  - `check_no_escalation(caller: &HashSet<String>, required: &[String]) -> Result<(), AuthError>`
  - `check_role_edit(caller: &HashSet<String>, old: &[String], new: &[String]) -> Result<(), AuthError>`
  - `scope_parts(scope_type: &str, scope_id: Uuid, project_of_app: Option<Uuid>) -> (Option<Uuid>, Option<Uuid>)`
  - `temp_password_from_bytes(bytes: &[u8]) -> String` — pure, deterministic
  - `generate_temp_password() -> String` — draws OS randomness, calls the above
  - `pub const TEMP_PASSWORD_ALPHABET: &str`
  - `pub const TEMP_PASSWORD_LEN: usize = 16`

  Tasks 3–6 consume all of these.

- [ ] **Step 1: Use `getrandom`, not `rand`**

The workspace has **no `rand` dependency** — `backend/Cargo.toml:74` declares `getrandom = "0.3"`, and `sauron-core/src/ids.rs` generates all existing tokens with `getrandom::fill`. `sauron-auth/Cargo.toml:13` already has `getrandom.workspace = true`.

So **add no new dependency.** Splitting the generator in two gets determinism without one:

- `temp_password_from_bytes(bytes)` is pure — tests feed it fixed bytes, so it is deterministic with no seeded RNG.
- `generate_temp_password()` draws from the OS and delegates.

This is better than a seedable `Rng` parameter on both counts: no new crate, and no exposure to the `rand` 0.8-vs-0.9 `gen_range`/`random_range` split.

Confirm before writing: `grep -n 'getrandom' backend/crates/sauron-auth/Cargo.toml`
Expected: `getrandom.workspace = true`. If absent, add it — do NOT add `rand`.

- [ ] **Step 2: Write the failing tests**

Create `backend/crates/sauron-auth/src/guard.rs` containing **only** the test module for now, so the run fails on missing functions rather than on assertions:

```rust
//! Pure authorization guards.
//!
//! These take already-fetched data and return a decision. Keeping them free of
//! DB and axum-state dependencies is what makes them testable: CI runs
//! `cargo test --workspace` with no Postgres, so any guard reachable only
//! through a handler is a guard that never gets tested.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;
    use uuid::Uuid;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn role_permissions_parses_a_string_array() {
        let v = json!(["issue:read", "app:read"]);
        assert_eq!(role_permissions(&v), strings(&["issue:read", "app:read"]));
    }

    #[test]
    fn role_permissions_tolerates_malformed_jsonb() {
        // A non-array must yield empty rather than panic: this value comes from
        // a JSONB column, not from validated input.
        assert!(role_permissions(&json!({})).is_empty());
        assert!(role_permissions(&json!("issue:read")).is_empty());
        assert!(role_permissions(&json!(null)).is_empty());
        assert!(role_permissions(&json!([])).is_empty());
        // Non-string members are skipped, not fatal.
        assert_eq!(role_permissions(&json!(["issue:read", 7])), strings(&["issue:read"]));
    }

    #[test]
    fn no_escalation_allows_a_superset_caller() {
        let caller = set(&["issue:read", "app:read", "org:manage"]);
        assert!(check_no_escalation(&caller, &strings(&["issue:read"])).is_ok());
    }

    #[test]
    fn no_escalation_allows_an_empty_requirement() {
        assert!(check_no_escalation(&set(&[]), &[]).is_ok());
    }

    #[test]
    fn no_escalation_rejects_a_missing_permission() {
        let caller = set(&["issue:read"]);
        let err = check_no_escalation(&caller, &strings(&["issue:read", "org:manage"]));
        assert!(matches!(err, Err(AuthError::Forbidden)));
    }

    #[test]
    fn role_edit_rejects_adding_a_permission_the_caller_lacks() {
        let caller = set(&["issue:read", "role:manage"]);
        let err = check_role_edit(&caller, &strings(&["issue:read"]), &strings(&["issue:read", "org:manage"]));
        assert!(matches!(err, Err(AuthError::Forbidden)));
    }

    #[test]
    fn role_edit_rejects_removing_a_permission_the_caller_lacks() {
        // A Developer with role:manage must not be able to defang the Admin
        // role by stripping permissions that outrank them.
        let caller = set(&["issue:read", "role:manage"]);
        let err = check_role_edit(&caller, &strings(&["issue:read", "org:manage"]), &strings(&["issue:read"]));
        assert!(matches!(err, Err(AuthError::Forbidden)));
    }

    #[test]
    fn role_edit_allows_changes_within_the_callers_own_grant() {
        let caller = set(&["issue:read", "issue:write", "app:read"]);
        assert!(check_role_edit(&caller, &strings(&["issue:read"]), &strings(&["issue:write", "app:read"])).is_ok());
    }

    #[test]
    fn role_edit_allows_a_noop() {
        let caller = set(&["issue:read"]);
        let same = strings(&["issue:read"]);
        assert!(check_role_edit(&caller, &same, &same).is_ok());
    }

    #[test]
    fn role_edit_ignores_ordering_differences() {
        let caller = set(&["issue:read", "app:read"]);
        assert!(check_role_edit(
            &caller,
            &strings(&["issue:read", "app:read"]),
            &strings(&["app:read", "issue:read"]),
        )
        .is_ok());
    }

    #[test]
    fn scope_parts_maps_each_scope_type() {
        let id = Uuid::new_v4();
        let project = Uuid::new_v4();
        assert_eq!(scope_parts("org", id, None), (None, None));
        assert_eq!(scope_parts("project", id, None), (Some(id), None));
        assert_eq!(scope_parts("app", id, Some(project)), (Some(project), Some(id)));
        // An app whose ancestry lookup failed still scopes to the app itself.
        assert_eq!(scope_parts("app", id, None), (None, Some(id)));
        // Unknown scope types degrade to org scope, the narrowest grant of
        // authority, so a bad value cannot widen anyone's effective permissions.
        assert_eq!(scope_parts("nonsense", id, None), (None, None));
    }

    #[test]
    fn temp_password_has_the_documented_shape() {
        let pw = generate_temp_password();
        assert_eq!(pw.chars().count(), TEMP_PASSWORD_LEN);
        assert!(pw.chars().all(|c| TEMP_PASSWORD_ALPHABET.contains(c)));
    }

    #[test]
    fn temp_password_excludes_visually_ambiguous_characters() {
        // These get read off a screen and retyped by hand.
        for c in ['0', 'O', '1', 'l', 'I'] {
            assert!(!TEMP_PASSWORD_ALPHABET.contains(c), "alphabet contains {c}");
        }
    }

    #[test]
    fn temp_password_from_bytes_is_deterministic() {
        let bytes: Vec<u8> = (0u8..64).collect();
        assert_eq!(temp_password_from_bytes(&bytes), temp_password_from_bytes(&bytes));
    }

    #[test]
    fn temp_password_from_bytes_varies_with_input() {
        let a: Vec<u8> = (0u8..64).collect();
        let b: Vec<u8> = (64u8..128).collect();
        assert_ne!(temp_password_from_bytes(&a), temp_password_from_bytes(&b));
    }

    #[test]
    fn temp_password_rejects_biased_bytes() {
        // 256 is not a multiple of the 57-char alphabet, so bytes >= 228 must
        // be discarded rather than folded in with modulo — otherwise the first
        // 256 % 57 = 28 characters are ~2x as likely as the rest.
        let alphabet: Vec<char> = TEMP_PASSWORD_ALPHABET.chars().collect();
        assert_eq!(alphabet.len(), 57);
        // Bytes in [228, 255] are all rejected, so an input of only those
        // yields nothing at all.
        let all_rejected: Vec<u8> = (228u8..=255).collect();
        assert_eq!(temp_password_from_bytes(&all_rejected), "");
    }

    #[test]
    fn temp_password_from_bytes_stops_at_the_documented_length() {
        // Plenty of input must still yield exactly TEMP_PASSWORD_LEN.
        let plenty = vec![7u8; 512];
        assert_eq!(
            temp_password_from_bytes(&plenty).chars().count(),
            TEMP_PASSWORD_LEN
        );
    }

    #[test]
    fn generated_passwords_differ() {
        assert_ne!(generate_temp_password(), generate_temp_password());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd backend && cargo test -p sauron-auth guard`
Expected: FAIL to compile — `cannot find function 'role_permissions' in this scope` and similar for each function.

- [ ] **Step 4: Write the implementation**

Insert above the `#[cfg(test)]` block in `guard.rs`:

```rust
use std::collections::HashSet;

use serde_json::Value;
use uuid::Uuid;

use crate::extractors::AuthError;

/// Length of a generated temp password.
pub const TEMP_PASSWORD_LEN: usize = 16;

/// Alphabet for generated temp passwords. Excludes `0 O 1 l I`: these are
/// dictated aloud and retyped by hand, and a password nobody can transcribe
/// gets replaced by one the admin invents.
pub const TEMP_PASSWORD_ALPHABET: &str =
    "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";

/// Read a role's `permissions` JSONB column into a list.
///
/// Malformed JSONB yields an empty list rather than an error. An unreadable
/// permission set must fail closed (no permissions), and the caller's
/// escalation check then denies anything that depends on it.
pub fn role_permissions(perms: &Value) -> Vec<String> {
    match perms {
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

/// Refuse to hand out a permission the caller does not hold at that scope.
pub fn check_no_escalation(
    caller: &HashSet<String>,
    required: &[String],
) -> Result<(), AuthError> {
    for p in required {
        if !caller.contains(p) {
            return Err(AuthError::Forbidden);
        }
    }
    Ok(())
}

/// Guard a role edit in both directions.
///
/// Adding a permission the caller lacks is escalation. Removing one they lack
/// is sabotage: a Developer holding `role:manage` could otherwise strip
/// `org:manage` from the Admin role and disable everyone above them. Only the
/// symmetric difference is checked, so a reordered no-op edit is free.
pub fn check_role_edit(
    caller: &HashSet<String>,
    old: &[String],
    new: &[String],
) -> Result<(), AuthError> {
    let old_set: HashSet<&String> = old.iter().collect();
    let new_set: HashSet<&String> = new.iter().collect();
    for p in new_set.symmetric_difference(&old_set) {
        if !caller.contains(*p) {
            return Err(AuthError::Forbidden);
        }
    }
    Ok(())
}

/// Split a grant's scope into the `(project, app)` pair `effective_at` expects.
///
/// `project_of_app` is the app's parent project, when the scope is an app and
/// the ancestry lookup succeeded. Unknown scope types fall back to org scope —
/// the narrowest authority — so a bad column value cannot widen permissions.
pub fn scope_parts(
    scope_type: &str,
    scope_id: Uuid,
    project_of_app: Option<Uuid>,
) -> (Option<Uuid>, Option<Uuid>) {
    match scope_type {
        "project" => (Some(scope_id), None),
        "app" => (project_of_app, Some(scope_id)),
        _ => (None, None),
    }
}

/// Map random bytes onto the alphabet, up to [`TEMP_PASSWORD_LEN`] characters.
///
/// Pure and deterministic, which is the whole point: the caller supplies the
/// randomness, so this can be tested with fixed input and no seedable RNG (the
/// workspace has no `rand` dependency and does not need one).
///
/// Uses rejection sampling. 256 is not a multiple of the 57-character
/// alphabet, so a plain `% 57` would make the first 28 characters roughly
/// twice as likely as the rest. Bytes at or above the largest multiple of 57
/// are discarded instead. May return fewer than `TEMP_PASSWORD_LEN` characters
/// if `bytes` is short or heavily rejected — `generate_temp_password` loops
/// until it has enough.
pub fn temp_password_from_bytes(bytes: &[u8]) -> String {
    let alphabet: Vec<char> = TEMP_PASSWORD_ALPHABET.chars().collect();
    let n = alphabet.len();
    let limit = (256 / n) * n; // 228 for a 57-char alphabet
    let mut out = String::with_capacity(TEMP_PASSWORD_LEN);
    for &b in bytes {
        if out.chars().count() >= TEMP_PASSWORD_LEN {
            break;
        }
        if (b as usize) < limit {
            out.push(alphabet[(b as usize) % n]);
        }
    }
    out
}

/// Generate a temp password from OS randomness.
///
/// Follows the crate convention in `sauron-core::ids` (`getrandom::fill`)
/// rather than pulling in `rand`. Loops because rejection sampling can consume
/// more bytes than it emits.
pub fn generate_temp_password() -> String {
    let mut out = String::with_capacity(TEMP_PASSWORD_LEN);
    while out.chars().count() < TEMP_PASSWORD_LEN {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).expect("OS RNG must be available");
        let chunk = temp_password_from_bytes(&buf);
        for c in chunk.chars() {
            if out.chars().count() >= TEMP_PASSWORD_LEN {
                break;
            }
            out.push(c);
        }
    }
    out
}
```

Check `getrandom`'s API shape against `sauron-core/src/ids.rs:14` — that file calls `getrandom::fill(&mut buf)`. Use the identical call so both agree on the version's API.

- [ ] **Step 5: Register the module**

In `backend/crates/sauron-auth/src/lib.rs`, alongside the existing `pub mod` declarations:

```rust
pub mod guard;
```

Check how `lib.rs` re-exports the other modules (`grep -n 'pub use' backend/crates/sauron-auth/src/lib.rs`) and follow that convention — if `rbac`/`extractors` items are re-exported at the crate root, add:

```rust
pub use guard::{
    check_no_escalation, check_role_edit, generate_temp_password, role_permissions, scope_parts,
};
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd backend && cargo test -p sauron-auth guard`
Expected: PASS, 12 tests.

- [ ] **Step 7: Lint**

Run: `cd backend && cargo fmt && cargo clippy -p sauron-auth -- -D warnings`
Expected: clean.

- [ ] **Step 8: Stage**

```bash
git add backend/crates/sauron-auth/src/guard.rs backend/crates/sauron-auth/src/lib.rs backend/crates/sauron-auth/Cargo.toml backend/Cargo.toml
```

---

### Task 3: Repo layer

All new database queries, in one place. No handler calls them yet.

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs`

**Interfaces:**
- Consumes: `User` with the Task 1 columns.
- Produces:
  - `pub const REVOKE_DEACTIVATED: &str = "deactivated"`
  - `create_user_with_temp_password(conn, email, password_hash, name) -> QueryResult<User>`
  - `set_user_active(conn, user_id, active) -> QueryResult<usize>`
  - `set_user_password(conn, user_id, password_hash) -> QueryResult<usize>`
  - `get_user(conn, id) -> QueryResult<Option<User>>`
  - `count_user_grants_outside_org(conn, user_id, org_id) -> QueryResult<i64>`
  - `count_org_manage_grants_for_user_excluding_user(conn, org_id, user_id) -> QueryResult<i64>`
  - `update_grant(conn, grant_id, role_id, scope_type, scope_id) -> QueryResult<RoleGrant>`
  - `update_role(conn, org_id, role_id, name, description, permissions) -> QueryResult<Role>`
  - `revoke_all_refresh_tokens_for_user_with_reason(conn, user_id, reason) -> QueryResult<usize>`

  Tasks 5–9 consume these.

- [ ] **Step 1: Add the revoke reason constant**

Find the existing `REVOKE_REUSE` constant (`grep -n 'REVOKE_REUSE' backend/crates/sauron-db/src/repo.rs`) and add beside it:

```rust
/// Refresh tokens killed because an admin deactivated the account. Distinct
/// from `REVOKE_REUSE` so the rotation grace window (which exists to survive
/// two dashboard tabs racing) can never resurrect a deactivated session.
pub const REVOKE_DEACTIVATED: &str = "deactivated";
```

- [ ] **Step 2: Add the user queries**

Append near the other `users` helpers (after `touch_last_login`, line 68):

```rust
/// Create a user whose password was generated by an admin. Identical to
/// `create_user` except the account is flagged as owing a password change.
pub async fn create_user_with_temp_password(
    conn: &mut AsyncPgConnection,
    email: &str,
    password_hash: &str,
    name: &str,
) -> QueryResult<User> {
    let email = email.to_lowercase();
    diesel::insert_into(users::table)
        .values((
            users::email.eq(&email),
            users::password_hash.eq(password_hash),
            users::name.eq(name),
            users::must_change_password.eq(true),
        ))
        .returning(User::as_returning())
        .get_result(conn)
        .await
}

pub async fn get_user(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<User>> {
    users::table
        .find(id)
        .select(User::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn set_user_active(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    active: bool,
) -> QueryResult<usize> {
    diesel::update(users::table.find(user_id))
        .set((
            users::is_active.eq(active),
            users::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}

/// Set a new password and clear the forced-change flag. Always clears it: the
/// only way to reach this is the self-service change endpoint, where the user
/// chose the password themselves.
pub async fn set_user_password(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    password_hash: &str,
) -> QueryResult<usize> {
    diesel::update(users::table.find(user_id))
        .set((
            users::password_hash.eq(password_hash),
            users::must_change_password.eq(false),
            users::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}
```

Confirm `User::as_returning()` matches the pattern used by the existing `create_user` (line 23) — copy whichever of `as_returning()` / explicit column list it uses.

- [ ] **Step 3: Add the guard-support counts**

Append near `count_org_manage_grants_excluding` (line 380), matching its `sql_query` + `GrantCountRow` style:

```rust
/// How many grants this user holds in orgs *other* than `org_id`.
///
/// Deactivation is account-global, but `member:manage` is org-scoped. If the
/// target belongs to another org too, this org's admin has no authority to
/// disable their login there, so a non-zero count blocks the operation.
pub async fn count_user_grants_outside_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM role_grants \
         WHERE user_id = $1 AND org_id <> $2",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<SqlUuid, _>(org_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// How many grants conferring `org:manage` this org would still have if every
/// grant belonging to `user_id` were ignored.
///
/// `count_org_manage_grants_excluding` excludes a single grant, which is right
/// for deleting one. Deactivation disables a whole person, who may hold several
/// org:manage grants at once, so the exclusion has to be by user.
pub async fn count_org_manage_grants_for_user_excluding_user(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    user_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n \
         FROM role_grants g JOIN roles r ON g.role_id = r.id \
         WHERE g.org_id = $1 AND g.user_id <> $2 AND g.scope_type = 'org' \
           AND r.permissions @> to_jsonb('org:manage'::text)",
    )
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(user_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}
```

These deliberately mirror `count_org_manage_grants_excluding` (`repo.rs:380`) clause for clause, including two details that are easy to lose:

- **`g.scope_type = 'org'`.** The existing guard counts only *org-scoped* grants. `org:manage` held at project or app scope does not administer the org, and if the new counts omitted this filter, deactivation and `delete_grant` would disagree about whether the org still has an administrator — one would permit an action the other forbids.
- **`r.permissions @> to_jsonb('org:manage'::text)`**, not `@> '["org:manage"]'::jsonb`. Both are semantically equivalent for containment, but matching the existing text keeps all three counts greppable as one family.

`SqlUuid` is the existing alias imported at the top of `repo.rs` for `diesel::sql_types::Uuid`; use it, as the neighbouring query does.

- [ ] **Step 4: Add the update queries**

```rust
pub async fn update_grant(
    conn: &mut AsyncPgConnection,
    grant_id: Uuid,
    role_id: Uuid,
    scope_type: &str,
    scope_id: Uuid,
) -> QueryResult<RoleGrant> {
    diesel::update(role_grants::table.find(grant_id))
        .set((
            role_grants::role_id.eq(role_id),
            role_grants::scope_type.eq(scope_type),
            role_grants::scope_id.eq(scope_id),
        ))
        .returning(RoleGrant::as_returning())
        .get_result(conn)
        .await
}

/// Update a custom role. Scoped by `org_id` as well as `role_id` so a mistaken
/// call cannot reach across orgs, and filtered on `is_system` so a preset can
/// never be written even if a caller-side check is missed.
pub async fn update_role(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    role_id: Uuid,
    name: &str,
    description: &str,
    permissions: Value,
) -> QueryResult<Role> {
    diesel::update(
        roles::table
            .filter(roles::id.eq(role_id))
            .filter(roles::org_id.eq(org_id))
            .filter(roles::is_system.eq(false)),
    )
    .set((
        roles::name.eq(name),
        roles::description.eq(description),
        roles::permissions.eq(permissions),
    ))
    .returning(Role::as_returning())
    .get_result(conn)
    .await
}

pub async fn revoke_all_refresh_tokens_for_user_with_reason(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    reason: &str,
) -> QueryResult<usize> {
    diesel::update(
        refresh_tokens::table
            .filter(refresh_tokens::user_id.eq(user_id))
            .filter(refresh_tokens::revoked_at.is_null()),
    )
    .set((
        refresh_tokens::revoked_at.eq(Utc::now()),
        refresh_tokens::revoked_reason.eq(reason),
    ))
    .execute(conn)
    .await
}
```

`update_role` returns `Err(NotFound)` when the row is a preset or belongs to another org, because the filters exclude it. Task 9 still checks explicitly — the filter is defence in depth, not the error message the user should see.

The existing `revoke_all_refresh_tokens_for_user` (line 181) hardcodes `REVOKE_REUSE`. Leave it alone; the reason-taking variant is additive so no existing caller changes behavior.

- [ ] **Step 5: Verify it compiles**

Run: `cd backend && cargo check -p sauron-db`
Expected: success. If `roles::is_system` or `roles::description` is missing, check the real column names in `schema.rs` (`grep -n 'roles (' -A 12 backend/crates/sauron-db/src/schema.rs`) and correct them.

- [ ] **Step 6: Lint and stage**

```bash
cd backend && cargo fmt && cargo clippy -p sauron-db -- -D warnings
git add backend/crates/sauron-db/src/repo.rs
```

---

### Task 4: Refactor `orgs.rs` onto `guard`

Pure refactor, no behavior change. Doing this before adding handlers means Tasks 5–9 have one canonical guard to call instead of copying the inline loops a third and fourth time.

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs:231-245` (in `create_grant`)
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs:276-304` (in `delete_grant`)

**Interfaces:**
- Consumes: `guard::{role_permissions, check_no_escalation, scope_parts}` from Task 2.
- Produces: no new API. Leaves `create_grant`/`delete_grant` behaviorally identical.

- [ ] **Step 1: Import the guards**

At the top of `orgs.rs`, extend the `sauron_auth` imports (line 10-11):

```rust
use sauron_auth::guard::{check_no_escalation, role_permissions, scope_parts};
use sauron_auth::rbac::grants_from_rows;
use sauron_auth::{authorize_org, perm, AuthError, AuthUser};
```

- [ ] **Step 2: Replace the inline parse + check in `create_grant`**

Lines 231-245 currently read:

```rust
    let role_perms: Vec<String> = match &role.permissions {
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    let granter =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, scope_project, scope_app)
            .await?;
    for p in &role_perms {
        if !granter.contains(p) {
            return Err(ApiError::Auth(AuthError::Forbidden));
        }
    }
```

Replace with:

```rust
    let role_perms = role_permissions(&role.permissions);
    let granter =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, scope_project, scope_app)
            .await?;
    check_no_escalation(&granter, &role_perms).map_err(ApiError::Auth)?;
```

- [ ] **Step 3: Replace the inline parse + check in `delete_grant`**

Lines 276-282 (the `role_perms` block) become:

```rust
    let role_perms = role_permissions(&role.permissions);
```

Lines 288-304 (the scope match and the remover loop) become:

```rust
    let project_of_app = if grant.scope_type == "app" {
        repo::app_ancestry(&mut conn, grant.scope_id)
            .await?
            .map(|(project_id, _)| project_id)
    } else {
        None
    };
    let (scope_project, scope_app) =
        scope_parts(&grant.scope_type, grant.scope_id, project_of_app);
    let remover =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, scope_project, scope_app)
            .await?;
    check_no_escalation(&remover, &role_perms).map_err(ApiError::Auth)?;
```

This preserves the original semantics exactly, including the case where an app's ancestry lookup returns `None`: the old code produced `(None, Some(scope_id))`, and `scope_parts("app", id, None)` returns the same pair — the behavior Task 2's `scope_parts_maps_each_scope_type` test pins.

- [ ] **Step 3a: Extract the scope-validation helper**

The `match req.scope_type` block at `orgs.rs:200-226` validates that a grant's scope target lives in this org — a cross-tenant boundary. Tasks 5 and 8 both need the identical check. Three copies means a future fix to the boundary has to land in three places, and missing one silently opens a cross-org grant path.

Add to `orgs.rs`, above `create_grant`:

```rust
/// Validate that a scope target belongs to `org_id`, returning the app's
/// parent project when the scope is an app (which `scope_parts` needs).
///
/// This is the cross-tenant boundary for grants: without it a caller could
/// name a project or app in someone else's org and have a grant created
/// against it. One implementation, called by every handler that accepts a
/// caller-supplied scope.
async fn validate_scope_in_org(
    conn: &mut sauron_db::AsyncPgConnection,
    org_id: Uuid,
    scope_type: &str,
    scope_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
    let not_in_org = || ApiError::BadRequest("scope target is not in this org".into());
    match scope_type {
        "org" => {
            if scope_id != org_id {
                return Err(not_in_org());
            }
            Ok(None)
        }
        "project" => {
            if repo::project_org(conn, scope_id).await? != Some(org_id) {
                return Err(not_in_org());
            }
            Ok(None)
        }
        "app" => match repo::app_ancestry(conn, scope_id).await? {
            Some((project_id, o)) if o == org_id => Ok(Some(project_id)),
            _ => Err(not_in_org()),
        },
        _ => Err(ApiError::BadRequest("invalid scope_type".into())),
    }
}
```

Confirm the connection type `db(&state)` yields (`grep -n 'type PgConn' backend/bins/sauron-api/src/routes/mod.rs` or `grep -n 'async fn db' -A 6 …`) and use that in the signature — if it is a pooled wrapper, take `&mut PgConn` or deref at the call site.

Then replace lines 200-226 of `create_grant` with:

```rust
    let project_of_app =
        validate_scope_in_org(&mut conn, org_id, &req.scope_type, req.scope_id).await?;
    let (scope_project, scope_app) =
        scope_parts(&req.scope_type, req.scope_id, project_of_app);
```

Behavior is identical: the same three error messages for the same three cases. The `_ => unreachable!()` arm is gone because the helper now handles an unknown `scope_type` itself — `create_grant`'s existing explicit check at line 175 still runs first, so that arm was already dead.

- [ ] **Step 4: Drop the now-unused import if needed**

If `serde_json::Value` is no longer referenced in `orgs.rs`, remove `use serde_json::Value;` (line 7). `create_role` still builds a `Value::Array` at line 369, so it is probably still needed — let clippy decide.

- [ ] **Step 5: Verify the refactor compiles clean**

Run: `cd backend && cargo clippy -p sauron-api -- -D warnings`
Expected: clean, no unused-import or unused-variable warnings.

- [ ] **Step 6: Confirm no behavior changed**

Run: `cd backend && cargo test --workspace`
Expected: PASS, same count as before this task.

This is a refactor with no test of its own — the guarantee is that `check_no_escalation` is a literal transcription of the loop it replaces, and Task 2 tests it directly.

- [ ] **Step 7: Stage**

```bash
cd backend && cargo fmt
git add backend/bins/sauron-api/src/routes/orgs.rs
```

---

### Task 5: `POST /v1/orgs/{org_id}/members` — create member

Creates the account and its first grant atomically, returning a one-time temp password.

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs` (append after `create_grant`)
- Modify: `backend/bins/sauron-api/src/main.rs` (route)
- Modify: `backend/crates/sauron-db/src/repo.rs` (the atomic insert; no Cargo.toml change — no new dependency)

**Interfaces:**
- Consumes: `guard::{generate_temp_password, role_permissions, check_no_escalation, scope_parts}`, `repo::find_user_by_email`, `validate_scope_in_org` (Task 4 Step 3a), the role validation already in `create_grant`.
- Also adds: `repo::create_member_with_grant` + `repo::NewMemberRow` (Step 1), which **replace** Task 3's `create_user_with_temp_password` — this task deletes that function.
- Produces: `pub async fn create_member(...)`, request `CreateMemberReq`, response JSON `{ user_id, grant_id, temp_password }`. Task 11's `createMember` client mirrors this shape.

- [ ] **Step 1: Add the atomic insert to `repo.rs`**

The account and its first grant must land atomically: a failed grant would otherwise leave an account with no access, holding the email in `users_email_lower_key`, invisible in every members list.

**Do not use `conn.transaction(...)`.** diesel-async 0.9.2 bounds its callback on `AsyncFnOnce` with a `for<'r>` HRTB, which in practice requires an *async closure* — stable only from Rust 1.85. This workspace declares `rust-version = "1.82"` (`backend/Cargo.toml:9`) and `packaging/rpm/sauron.spec:48-49` declares `BuildRequires: cargo >= 1.82` / `rust >= 1.82`. Raising the floor to 1.85 to gain one transaction would narrow which distros can build the RPM.

A **data-modifying CTE is atomic in a single statement**: no transaction block, no async closure, no MSRV change, and one round trip instead of two. Add to `repo.rs`, beside the other `sql_query` helpers:

```rust
#[derive(Debug, QueryableByName)]
pub struct NewMemberRow {
    #[diesel(sql_type = SqlUuid)]
    pub user_id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    pub grant_id: Uuid,
}

/// Create a user and their first grant in one statement.
///
/// A single data-modifying CTE rather than a transaction: Postgres runs both
/// INSERTs atomically within the statement, so a grant failure rolls the user
/// back for free. This avoids `conn.transaction`, whose diesel-async 0.9
/// signature needs async closures (Rust 1.85) and would push the workspace
/// MSRV past the 1.82 the RPM spec builds against.
///
/// A duplicate email surfaces as `DatabaseError(UniqueViolation)` from
/// `users_email_lower_key`; the caller maps that to 409.
pub async fn create_member_with_grant(
    conn: &mut AsyncPgConnection,
    email: &str,
    password_hash: &str,
    name: &str,
    org_id: Uuid,
    role_id: Uuid,
    scope_type: &str,
    scope_id: Uuid,
) -> QueryResult<NewMemberRow> {
    let email = email.to_lowercase();
    diesel::sql_query(
        "WITH new_user AS ( \
             INSERT INTO users (email, password_hash, name, must_change_password) \
             VALUES ($1, $2, $3, true) \
             RETURNING id \
         ) \
         INSERT INTO role_grants (org_id, user_id, role_id, scope_type, scope_id) \
         SELECT $4, new_user.id, $5, $6, $7 FROM new_user \
         RETURNING user_id, id AS grant_id",
    )
    .bind::<Text, _>(email)
    .bind::<Text, _>(password_hash)
    .bind::<Text, _>(name)
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(role_id)
    .bind::<Text, _>(scope_type)
    .bind::<SqlUuid, _>(scope_id)
    .get_result(conn)
    .await
}
```

`email.to_lowercase()` mirrors `create_user` (`repo.rs:29`) — the unique index is on `lower(email)`, so skipping it would let `A@b.com` and `a@b.com` both insert and then collide.

Confirm `Text` is imported in `repo.rs` from `diesel::sql_types` (the file already imports `BigInt` and `SqlUuid` from there); add it to that same `use` if absent.

Then **delete `create_user_with_temp_password`** (added in Task 3). This CTE supersedes it and nothing else calls it — leaving both means the next person reaches for the non-atomic one.

- [ ] **Step 2: Add the request type and handler**

Append to `backend/bins/sauron-api/src/routes/orgs.rs`, after `create_grant` ends (line 259):

```rust
#[derive(Deserialize)]
pub struct CreateMemberReq {
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub role_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
}

/// Create a user account and its first grant in one step.
///
/// The password is generated, never supplied by the caller: an admin who could
/// choose it would hold a working durable credential for somebody else's
/// account. It is returned exactly once, here, and `must_change_password`
/// makes it useless for anything but being replaced.
pub async fn create_member(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Json(req): Json<CreateMemberReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_MANAGE).await?;

    if !req.email.contains('@') {
        return Err(ApiError::BadRequest("a valid email is required".into()));
    }
    if !matches!(req.scope_type.as_str(), "org" | "project" | "app") {
        return Err(ApiError::BadRequest("invalid scope_type".into()));
    }
    if repo::find_user_by_email(&mut conn, &req.email)
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict(
            "a user with that email already exists — use Grant access instead".into(),
        ));
    }

    // Role must be a preset or belong to this org.
    let role = repo::get_role(&mut conn, req.role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(role_org) = role.org_id {
        if role_org != org_id {
            return Err(ApiError::BadRequest(
                "role does not belong to this org".into(),
            ));
        }
    }

    // Scope target must belong to this org, and gives us the (project, app)
    // pair the escalation check needs. Shared helper from Task 4 Step 3a — the
    // org-containment check has one implementation, not one per handler.
    let project_of_app =
        validate_scope_in_org(&mut conn, org_id, &req.scope_type, req.scope_id).await?;
    let (scope_project, scope_app) =
        scope_parts(&req.scope_type, req.scope_id, project_of_app);

    // Creating a user must not be a way around the grant escalation check.
    let role_perms = role_permissions(&role.permissions);
    let creator =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, scope_project, scope_app)
            .await?;
    check_no_escalation(&creator, &role_perms).map_err(ApiError::Auth)?;

    let temp_password = generate_temp_password();
    let hash = hash_password_async(temp_password.clone())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // One statement, atomic: a grant failure must not leave an account that
    // holds the email but has no access and appears in no list.
    let created = repo::create_member_with_grant(
        &mut conn,
        &req.email,
        &hash,
        &req.name,
        org_id,
        req.role_id,
        &req.scope_type,
        req.scope_id,
    )
    .await
    .map_err(|e| match e {
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => ApiError::Conflict(
            "a user with that email already exists — use Grant access instead".into(),
        ),
        other => ApiError::from(other),
    })?;

    Ok(Json(serde_json::json!({
        "user_id": created.user_id,
        "grant_id": created.grant_id,
        "temp_password": temp_password,
    })))
}
```

- [ ] **Step 3: Add the imports**

At the top of `orgs.rs`:

```rust
use sauron_auth::guard::{
    check_no_escalation, generate_temp_password, role_permissions, scope_parts,
};
```

Task 4 already imports `check_no_escalation`, `role_permissions`, and `scope_parts` — extend that existing `use` with `generate_temp_password` rather than adding a second line. No `diesel_async::AsyncConnection` or `ScopedFutureExt` import is needed; there is no transaction block.

`hash_password_async` is currently imported inside `auth.rs` from `sauron_auth::password`. Check with `grep -n 'hash_password_async' backend/bins/sauron-api/src/routes/auth.rs` — if it is imported from the crate, import it the same way here (`use sauron_auth::password::hash_password_async;`) rather than re-exporting through the `auth` route module.



- [ ] **Step 4: Register the route**

In `backend/bins/sauron-api/src/main.rs`, beside the existing members route (line ~154):

```rust
        .route(
            "/v1/orgs/{org_id}/members",
            get(routes::orgs::list_members).post(routes::orgs::create_member),
        )
```

Check the existing route syntax first: axum 0.7 uses `:org_id`, axum 0.8 uses `{org_id}`. Copy whichever form the neighbouring routes use.

- [ ] **Step 5: Verify it compiles**

Run: `cd backend && cargo clippy -p sauron-api -- -D warnings`
Expected: clean.

- [ ] **Step 6: Exercise it**

Start the API, then with an Owner's access token in `$TOKEN` and an org id in `$ORG`:

```bash
ROLE=$(curl -s -H "Authorization: Bearer $TOKEN" \
  "http://localhost:8080/v1/orgs/$ORG/roles" | jq -r '.[] | select(.name=="Viewer") | .id')

curl -s -X POST "http://localhost:8080/v1/orgs/$ORG/members" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"email\":\"newbie@example.com\",\"name\":\"New Bie\",\"role_id\":\"$ROLE\",\"scope_type\":\"org\",\"scope_id\":\"$ORG\"}" | jq
```

Expected: `{ "user_id": "...", "grant_id": "...", "temp_password": "<16 chars>" }`.

Repeat the same call: expected `409` with "already exists — use Grant access instead".

Confirm the flag landed:
```bash
psql "$DATABASE_URL" -c "SELECT email, is_active, must_change_password FROM users WHERE email='newbie@example.com';"
```
Expected: `t` for `is_active`, `t` for `must_change_password`.

- [ ] **Step 7: Stage**

```bash
cd backend && cargo fmt
git add backend/bins/sauron-api/src/routes/orgs.rs backend/bins/sauron-api/src/main.rs backend/crates/sauron-db/src/repo.rs
```

---

### Task 6: `PATCH /v1/orgs/{org_id}/members/{user_id}` — deactivate / reactivate

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs`
- Modify: `backend/bins/sauron-api/src/main.rs`

**Interfaces:**
- Consumes: `repo::{get_user, set_user_active, count_user_grants_outside_org, count_org_manage_grants_for_user_excluding_user, revoke_all_refresh_tokens_for_user_with_reason, REVOKE_DEACTIVATED}`.
- Produces: `pub async fn set_member_active(...)`, request `SetMemberActiveReq { is_active: bool }`, response `{ "ok": true }`. Task 11's `setMemberActive` mirrors it.

- [ ] **Step 1: Add the handler**

Append to `orgs.rs`:

```rust
#[derive(Deserialize)]
pub struct SetMemberActiveReq {
    pub is_active: bool,
}

/// Enable or disable a member's ability to log in.
///
/// Deliberately leaves `role_grants` untouched. This is not a delete: the
/// member stays in the list, badged and reversible. Removing access to one
/// scope is what `DELETE /v1/grants/{id}` is for.
pub async fn set_member_active(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SetMemberActiveReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_MANAGE).await?;

    let user = repo::get_user(&mut conn, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    // The target must actually be a member of this org, or any admin could
    // toggle any account in the deployment by guessing a uuid.
    if repo::user_grants_in_org(&mut conn, user_id, org_id)
        .await?
        .is_empty()
    {
        return Err(ApiError::NotFound);
    }

    if !req.is_active {
        if user_id == auth.user_id {
            return Err(ApiError::Conflict(
                "you cannot deactivate your own account".into(),
            ));
        }
        // member:manage is org-scoped; deactivation is account-global. Allowing
        // it for someone who also belongs to another org would let this org's
        // admin lock them out of an org they have no authority over.
        if repo::count_user_grants_outside_org(&mut conn, user_id, org_id).await? > 0 {
            return Err(ApiError::Conflict(
                "this member belongs to another organization and cannot be deactivated from here"
                    .into(),
            ));
        }
        // Same reasoning as delete_grant's last-owner guard: an org with no
        // org:manage holder can never regain one, because create_grant's
        // escalation check makes it ungrantable.
        if repo::count_org_manage_grants_for_user_excluding_user(&mut conn, org_id, user_id).await?
            == 0
        {
            return Err(ApiError::Conflict(
                "cannot deactivate the last member with org:manage — assign it to someone else first"
                    .into(),
            ));
        }
    }

    repo::set_user_active(&mut conn, user_id, req.is_active).await?;
    if !req.is_active {
        repo::revoke_all_refresh_tokens_for_user_with_reason(
            &mut conn,
            user_id,
            repo::REVOKE_DEACTIVATED,
        )
        .await?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}
```

The access token already issued to a member being deactivated stays valid until it expires (15 minutes by default). Revoking the refresh tokens is what makes the lockout permanent. Closing that window entirely would need a per-request user lookup on every endpoint; the spec accepts the window.

- [ ] **Step 2: Register the route**

```rust
        .route(
            "/v1/orgs/{org_id}/members/{user_id}",
            patch(routes::orgs::set_member_active),
        )
```

Add `patch` to the `axum::routing::{...}` import at the top of `main.rs` if it is not already there.

- [ ] **Step 3: Verify it compiles**

Run: `cd backend && cargo clippy -p sauron-api -- -D warnings`
Expected: clean.

- [ ] **Step 4: Exercise the guards**

With `$USER` = the member created in Task 5:

```bash
# Deactivate — expect ok
curl -s -X PATCH "http://localhost:8080/v1/orgs/$ORG/members/$USER" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"is_active":false}' | jq

# Self — expect 409
curl -s -o /dev/null -w '%{http_code}\n' -X PATCH \
  "http://localhost:8080/v1/orgs/$ORG/members/$SELF" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"is_active":false}'

# Reactivate — expect ok
curl -s -X PATCH "http://localhost:8080/v1/orgs/$ORG/members/$USER" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"is_active":true}' | jq
```

Expected: `{"ok":true}`, then `409`, then `{"ok":true}`.

Confirm tokens were revoked with the right reason after the deactivate:
```bash
psql "$DATABASE_URL" -c "SELECT revoked_reason FROM refresh_tokens WHERE user_id='$USER' AND revoked_at IS NOT NULL;"
```
Expected: `deactivated`.

- [ ] **Step 5: Stage**

```bash
cd backend && cargo fmt
git add backend/bins/sauron-api/src/routes/orgs.rs backend/bins/sauron-api/src/main.rs
```

---

### Task 7: Forced password change + deactivation enforcement

The security core. A temp password must grant no capability except replacing itself, and that has to hold against a raw API call, not just a UI redirect.

**Files:**
- Modify: `backend/crates/sauron-auth/src/jwt.rs:14-20`, `:39-53`
- Modify: `backend/crates/sauron-auth/src/extractors.rs`
- Modify: `backend/bins/sauron-api/src/routes/mod.rs:101-120`
- Modify: `backend/bins/sauron-api/src/routes/auth.rs` (login, refresh, new handler)
- Modify: `backend/bins/sauron-api/src/main.rs`

**Interfaces:**
- Consumes: `repo::{get_user, set_user_password, revoke_all_refresh_tokens_for_user_with_reason}`.
- Produces:
  - `Claims.must_change_password: bool`
  - `JwtKeys::issue_access(user_id: Uuid, must_change_password: bool) -> anyhow::Result<(String, i64)>` — **signature change**, 1 call site
  - `issue_tokens(state, conn, user_id, user_agent, must_change_password: bool)` — **signature change**, 5 call sites
  - `AuthError::{AccountDeactivated, PasswordChangeRequired}`
  - `pub async fn change_password(...)` returning `AuthResponse`

- [ ] **Step 1: Write the failing claim test**

Add to the `mod tests` block in `backend/crates/sauron-auth/src/jwt.rs`:

```rust
    #[test]
    fn access_token_carries_the_password_change_flag() {
        let keys = JwtKeys::new("test-secret-please-change-0000000000", 900);
        let uid = Uuid::new_v4();
        let (token, _) = keys.issue_access(uid, true).unwrap();
        assert!(keys.decode_access(&token).unwrap().must_change_password);

        let (token, _) = keys.issue_access(uid, false).unwrap();
        assert!(!keys.decode_access(&token).unwrap().must_change_password);
    }

    #[test]
    fn tokens_minted_before_the_flag_existed_still_decode() {
        // Sessions live across a deploy. A token issued by the previous build
        // has no `must_change_password` field at all; without #[serde(default)]
        // every logged-in user is signed out the moment this ships.
        use jsonwebtoken::{encode, EncodingKey, Header};
        #[derive(serde::Serialize)]
        struct LegacyClaims {
            sub: String,
            iat: i64,
            exp: i64,
            jti: String,
            typ: String,
        }
        let uid = Uuid::new_v4();
        let now = Utc::now().timestamp();
        let legacy = LegacyClaims {
            sub: uid.to_string(),
            iat: now,
            exp: now + 900,
            jti: "abc123".to_string(),
            typ: "access".to_string(),
        };
        let secret = "test-secret-please-change-0000000000";
        let token = encode(
            &Header::default(),
            &legacy,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let keys = JwtKeys::new(secret, 900);
        let claims = keys.decode_access(&token).unwrap();
        assert_eq!(claims.sub, uid.to_string());
        assert!(!claims.must_change_password);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd backend && cargo test -p sauron-auth jwt`
Expected: FAIL to compile — `issue_access` takes 1 argument, and `Claims` has no field `must_change_password`.

- [ ] **Step 3: Add the claim**

In `backend/crates/sauron-auth/src/jwt.rs`, the `Claims` struct (line 14):

```rust
pub struct Claims {
    pub sub: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
    pub typ: String,
    /// The holder owes a password change; the extractor rejects every request
    /// except the change itself. `serde(default)` because tokens issued before
    /// this field existed must keep decoding across the deploy.
    #[serde(default)]
    pub must_change_password: bool,
}
```

And `issue_access` (line 39):

```rust
    pub fn issue_access(
        &self,
        user_id: Uuid,
        must_change_password: bool,
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
        };
        let token = encode(&Header::default(), &claims, &self.enc)
            .map_err(|e| anyhow::anyhow!("jwt encode: {e}"))?;
        Ok((token, exp))
    }
```

Update the existing `access_token_roundtrip` test to pass `false`.

- [ ] **Step 4: Run to verify it passes**

Run: `cd backend && cargo test -p sauron-auth jwt`
Expected: PASS, 4 tests.

- [ ] **Step 5: Add the two error variants**

In `backend/crates/sauron-auth/src/extractors.rs`, extend `AuthError` and its `parts()`:

```rust
    /// The account exists and the password was correct, but an admin disabled
    /// it. Only ever returned *after* a successful password verification.
    AccountDeactivated,
    /// The caller holds a temp password and must replace it before doing
    /// anything else.
    PasswordChangeRequired,
```

```rust
            AuthError::AccountDeactivated => (
                StatusCode::FORBIDDEN,
                "account_deactivated",
                "this account has been deactivated",
            ),
            AuthError::PasswordChangeRequired => (
                StatusCode::FORBIDDEN,
                "password_change_required",
                "you must change your password before continuing",
            ),
```

- [ ] **Step 6: Gate the extractor**

In `AuthUser::from_request_parts`, after `user_id` is parsed and before `Ok(AuthUser { ... })`:

```rust
        // A temp password may do exactly one thing: become a real one.
        // Enforcing this in the extractor rather than in the dashboard is the
        // point — a UI redirect is bypassable with curl, which would leave the
        // admin who generated the password holding a working credential for
        // somebody else's account.
        if claims.must_change_password {
            let path = parts.uri.path();
            let allowed = matches!(path, "/v1/auth/password" | "/v1/auth/logout");
            if !allowed {
                return Err(AuthError::PasswordChangeRequired);
            }
        }
```

The allowlist is matched on the exact path. If the API is ever mounted under a prefix, this becomes a suffix match — note it here rather than generalising now.

- [ ] **Step 7: Add the allowlist test**

In `extractors.rs`'s `mod tests`:

```rust
    #[test]
    fn password_change_allowlist_is_exactly_two_paths() {
        let allowed = |p: &str| matches!(p, "/v1/auth/password" | "/v1/auth/logout");
        assert!(allowed("/v1/auth/password"));
        assert!(allowed("/v1/auth/logout"));
        // Everything a temp-password holder might otherwise reach.
        for p in [
            "/v1/orgs",
            "/v1/auth/refresh",
            "/v1/projects",
            "/v1/admin/storage",
            "/v1/auth/passwordx",
        ] {
            assert!(!allowed(p), "{p} must not be reachable");
        }
    }

    #[test]
    fn deactivated_and_change_required_are_distinct_forbidden_codes() {
        let (s1, c1, _) = AuthError::AccountDeactivated.parts();
        let (s2, c2, _) = AuthError::PasswordChangeRequired.parts();
        assert_eq!(s1, StatusCode::FORBIDDEN);
        assert_eq!(s2, StatusCode::FORBIDDEN);
        assert_ne!(c1, c2);
        // The dashboard routes on these codes; a rename is a breaking change.
        assert_eq!(c1, "account_deactivated");
        assert_eq!(c2, "password_change_required");
    }
```

`/v1/auth/refresh` is deliberately excluded: it is unauthenticated (it takes the refresh token in the body, not a bearer header), so it never reaches this gate, and listing it would wrongly suggest a temp-password holder can rotate into a clean token.

- [ ] **Step 8: Thread the flag through `issue_tokens`**

In `backend/bins/sauron-api/src/routes/mod.rs:101`:

```rust
pub(crate) async fn issue_tokens(
    state: &AppState,
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    user_agent: Option<String>,
    must_change_password: bool,
) -> Result<TokenPair, ApiError> {
    let (access, exp) = state
        .keys
        .issue_access(user_id, must_change_password)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
```

The rest of the function is unchanged.

- [ ] **Step 9: Update the five call sites**

| Location | Argument | Why |
|---|---|---|
| `auth.rs:241` (register) | `false` | The user chose their own password |
| `auth.rs:302` (login) | `user.must_change_password` | Carry the account's real state |
| `auth.rs:360` (refresh, race path) | `user.must_change_password` | Requires the user lookup added in Step 11 |
| `auth.rs:377` (refresh, normal path) | `user.must_change_password` | Same |
| change-password (Step 12) | `false` | Just cleared |

- [ ] **Step 10: Check `is_active` at login**

In `auth.rs`, immediately after the `let user = match found { ... };` block (line 299) and before `touch_last_login`:

```rust
    // Checked here, not earlier: an is_active branch before the password
    // verification would answer in microseconds for a deactivated account and
    // tens of milliseconds for an active one, reintroducing exactly the
    // user-enumeration oracle the dummy-verify above exists to close. Someone
    // who does not know the password learns nothing.
    if !user.is_active {
        return Err(ApiError::Auth(AuthError::AccountDeactivated));
    }
```

Confirm `AuthError` is imported in `auth.rs`; add `use sauron_auth::AuthError;` if not.

- [ ] **Step 11: Check `is_active` at refresh**

`refresh` currently issues tokens from `token.user_id` without loading the user. Both issue paths now need the user anyway for the flag. Before each `issue_tokens` call in `refresh` (lines 360 and 377):

```rust
        let user = repo::get_user(&mut conn, user_id)
            .await?
            .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
        if !user.is_active {
            return Err(ApiError::Auth(AuthError::AccountDeactivated));
        }
        let tokens = issue_tokens(&state, &mut conn, user_id, None, user.must_change_password)
            .await?;
```

At line 377 the variable is `token.user_id`, not `user_id` — adjust accordingly. Without this, a deactivated member with a live refresh token in localStorage keeps minting fresh access tokens indefinitely and the deactivation never takes effect.

- [ ] **Step 12: Add the change-password handler**

Append to `auth.rs`:

```rust
#[derive(Deserialize)]
pub struct ChangePasswordReq {
    pub current_password: String,
    pub new_password: String,
}

/// Self-service password change. The only endpoint a temp-password holder can
/// reach.
pub async fn change_password(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordReq>,
) -> Result<Json<AuthResponse>, ApiError> {
    if req.current_password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::Auth(AuthError::InvalidCredentials));
    }
    if req.new_password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    if req.new_password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::BadRequest(format!(
            "password must be at most {MAX_PASSWORD_LEN} characters"
        )));
    }
    if req.new_password == req.current_password {
        return Err(ApiError::BadRequest(
            "the new password must be different from the current one".into(),
        ));
    }

    let mut conn = db(&state).await?;
    let user = repo::get_user(&mut conn, auth.user_id)
        .await?
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
    if !user.is_active {
        return Err(ApiError::Auth(AuthError::AccountDeactivated));
    }
    if !verify_password_async(req.current_password.clone(), user.password_hash.clone()).await {
        return Err(ApiError::Auth(AuthError::InvalidCredentials));
    }

    let hash = hash_password_async(req.new_password.clone())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    repo::set_user_password(&mut conn, auth.user_id, &hash).await?;

    // Revoke everything, including the caller's own session, then re-issue.
    // Keeping the current session would not work: its access token still
    // carries must_change_password, so the Step 6 gate would keep rejecting the
    // user until it expired — immediately after they did the one thing it was
    // demanding. Re-issuing also logs out every other device, which is correct
    // when the old credential may be known to whoever generated it.
    repo::revoke_all_refresh_tokens_for_user_with_reason(
        &mut conn,
        auth.user_id,
        repo::REVOKE_PASSWORD_CHANGED,
    )
    .await?;
    let tokens = issue_tokens(&state, &mut conn, auth.user_id, None, false).await?;

    let fresh = repo::get_user(&mut conn, auth.user_id)
        .await?
        .ok_or_else(|| ApiError::Internal("user vanished mid-request".into()))?;
    Ok(Json(AuthResponse {
        tokens,
        user: fresh,
    }))
}
```

Add the companion constant beside `REVOKE_DEACTIVATED` in `repo.rs` (Task 3, Step 1):

```rust
/// Refresh tokens rotated out because the user changed their own password.
pub const REVOKE_PASSWORD_CHANGED: &str = "password_changed";
```

The final `get_user` re-read returns the user with `must_change_password` already false, so the dashboard's stored user object is correct without a second round trip.

- [ ] **Step 13: Register the route**

In `main.rs`, beside the other `/v1/auth` routes (line 149-152):

```rust
        .route("/v1/auth/password", post(routes::auth::change_password))
```

The path must match the extractor allowlist in Step 6 exactly.

- [ ] **Step 14: Run the full test suite**

Run: `cd backend && cargo test --workspace && cargo clippy --workspace -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 15: Exercise the gate end-to-end**

Using the temp password from Task 5:

```bash
# Log in as the new member
NEW=$(curl -s -X POST http://localhost:8080/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"newbie@example.com","password":"<TEMP>"}' | jq -r .access_token)

# Any normal endpoint — expect 403 password_change_required
curl -s -H "Authorization: Bearer $NEW" http://localhost:8080/v1/orgs | jq

# Change it — expect a fresh token pair and user.must_change_password = false
FRESH=$(curl -s -X POST http://localhost:8080/v1/auth/password \
  -H "Authorization: Bearer $NEW" -H 'Content-Type: application/json' \
  -d '{"current_password":"<TEMP>","new_password":"a-real-password-1"}' | tee /dev/stderr | jq -r .access_token)

# Same endpoint with the fresh token — expect the org list
curl -s -H "Authorization: Bearer $FRESH" http://localhost:8080/v1/orgs | jq
```

Expected in order: `403 password_change_required`; a token pair with `"must_change_password": false`; then the org list. The third call is the regression guard for the stale-claim lockout.

Then deactivate them (Task 6) and confirm login returns `403 account_deactivated`, while a *wrong* password on the same account still returns `401 invalid_credentials` — the enumeration oracle must stay closed.

- [ ] **Step 16: Stage**

```bash
cd backend && cargo fmt
git add backend/crates/sauron-auth/src/jwt.rs backend/crates/sauron-auth/src/extractors.rs \
        backend/crates/sauron-db/src/repo.rs backend/bins/sauron-api/src/routes/mod.rs \
        backend/bins/sauron-api/src/routes/auth.rs backend/bins/sauron-api/src/main.rs
```

---

### Task 8: `PATCH /v1/grants/{grant_id}` — edit a member's role/scope

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs`
- Modify: `backend/bins/sauron-api/src/main.rs`

**Interfaces:**
- Consumes: `guard::{role_permissions, check_no_escalation, scope_parts}`, `repo::{get_grant, get_role, update_grant, count_org_manage_grants_excluding, project_org, app_ancestry}`.
- Produces: `pub async fn update_grant_handler(...)`, request `UpdateGrantReq { role_id?, scope_type?, scope_id? }`, response `{ "id": uuid }`. Task 11's `updateGrant` mirrors it.

- [ ] **Step 1: Add the handler**

Append to `orgs.rs`:

```rust
#[derive(Deserialize)]
pub struct UpdateGrantReq {
    pub role_id: Option<Uuid>,
    pub scope_type: Option<String>,
    pub scope_id: Option<Uuid>,
}

/// Change a member's role and/or scope in place.
///
/// One statement rather than a client-side delete-then-recreate: a recreate
/// that failed would silently strand the member with no access, and the
/// last-owner guard has to judge the final state, not the intermediate one
/// where the grant is already gone.
pub async fn update_grant_handler(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(grant_id): Path<Uuid>,
    Json(req): Json<UpdateGrantReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    let grant = repo::get_grant(&mut conn, grant_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let org_id = grant.org_id;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_MANAGE).await?;

    let new_role_id = req.role_id.unwrap_or(grant.role_id);
    let new_scope_type = req
        .scope_type
        .clone()
        .unwrap_or_else(|| grant.scope_type.clone());
    let new_scope_id = req.scope_id.unwrap_or(grant.scope_id);

    if !matches!(new_scope_type.as_str(), "org" | "project" | "app") {
        return Err(ApiError::BadRequest("invalid scope_type".into()));
    }

    let new_role = repo::get_role(&mut conn, new_role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(role_org) = new_role.org_id {
        if role_org != org_id {
            return Err(ApiError::BadRequest(
                "role does not belong to this org".into(),
            ));
        }
    }

    // New scope must be inside this org. Shared helper from Task 4 Step 3a.
    let new_project_of_app =
        validate_scope_in_org(&mut conn, org_id, &new_scope_type, new_scope_id).await?;

    // Both directions, mirroring create_grant + delete_grant: the caller must
    // outrank what they are granting AND what they are taking away. Checking
    // only the new role would let an Admin rewrite the Owner's grant down to
    // Viewer — a delete they are already forbidden from performing.
    let old_role = repo::get_role(&mut conn, grant.role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let old_perms = role_permissions(&old_role.permissions);
    let new_perms = role_permissions(&new_role.permissions);

    let old_project_of_app = if grant.scope_type == "app" {
        repo::app_ancestry(&mut conn, grant.scope_id)
            .await?
            .map(|(project_id, _)| project_id)
    } else {
        None
    };
    let (old_sp, old_sa) = scope_parts(&grant.scope_type, grant.scope_id, old_project_of_app);
    let caller_at_old =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, old_sp, old_sa).await?;
    check_no_escalation(&caller_at_old, &old_perms).map_err(ApiError::Auth)?;

    let (new_sp, new_sa) = scope_parts(&new_scope_type, new_scope_id, new_project_of_app);
    let caller_at_new =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, new_sp, new_sa).await?;
    check_no_escalation(&caller_at_new, &new_perms).map_err(ApiError::Auth)?;

    // If this grant currently carries org:manage and the edit drops it, the org
    // must retain another holder.
    let loses_org_manage = old_perms.iter().any(|p| p == perm::ORG_MANAGE)
        && !new_perms.iter().any(|p| p == perm::ORG_MANAGE);
    let leaves_org_scope = grant.scope_type == "org" && new_scope_type != "org";
    if loses_org_manage || (leaves_org_scope && old_perms.iter().any(|p| p == perm::ORG_MANAGE)) {
        let remaining =
            repo::count_org_manage_grants_excluding(&mut conn, org_id, grant_id).await?;
        if remaining == 0 {
            return Err(ApiError::Conflict(
                "cannot remove the last grant with org:manage — assign it to another member first"
                    .into(),
            ));
        }
    }

    let updated = repo::update_grant(
        &mut conn,
        grant_id,
        new_role_id,
        &new_scope_type,
        new_scope_id,
    )
    .await
    .map_err(|e| match e {
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => ApiError::Conflict("this member already has that role at that scope".into()),
        other => ApiError::from(other),
    })?;

    Ok(Json(serde_json::json!({ "id": updated.id })))
}
```

`role_grants` has `UNIQUE (user_id, role_id, scope_type, scope_id)`, so editing a grant into an existing one is a unique violation — mapped to a readable `409` rather than a 500. Confirm `ApiError` has a `From<diesel::result::Error>` impl (`grep -n 'impl From<diesel' backend/bins/sauron-api/src/error.rs`); if the conversion is named differently, use whatever the other handlers use for the fallthrough arm.

- [ ] **Step 2: Register the route**

In `main.rs`, on the existing `/v1/grants/{grant_id}` entry:

```rust
        .route(
            "/v1/grants/{grant_id}",
            delete(routes::orgs::delete_grant).patch(routes::orgs::update_grant_handler),
        )
```

- [ ] **Step 3: Verify it compiles**

Run: `cd backend && cargo clippy -p sauron-api -- -D warnings`
Expected: clean.

- [ ] **Step 4: Exercise it**

```bash
# Move the member from Viewer to Developer at org scope
DEV=$(curl -s -H "Authorization: Bearer $TOKEN" \
  "http://localhost:8080/v1/orgs/$ORG/roles" | jq -r '.[]|select(.name=="Developer")|.id')

curl -s -X PATCH "http://localhost:8080/v1/grants/$GRANT" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"role_id\":\"$DEV\"}" | jq

# Confirm via the members list
curl -s -H "Authorization: Bearer $TOKEN" \
  "http://localhost:8080/v1/orgs/$ORG/members" | jq '.[]|{email,role_name,scope_type}'
```

Expected: `{"id": "<grant>"}`, then the member showing `"role_name": "Developer"`.

Then try to edit the **Owner's** grant while authenticated as a non-owner Admin: expect `403`.

- [ ] **Step 5: Stage**

```bash
cd backend && cargo fmt
git add backend/bins/sauron-api/src/routes/orgs.rs backend/bins/sauron-api/src/main.rs
```

---

### Task 9: `PATCH /v1/orgs/{org_id}/roles/{role_id}` — edit a custom role

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs`
- Modify: `backend/bins/sauron-api/src/main.rs`

**Interfaces:**
- Consumes: `guard::{role_permissions, check_role_edit}`, `repo::{get_role, update_role}`.
- Produces: `pub async fn update_role_handler(...)`, request `UpdateRoleReq { name?, description?, permissions? }`, response `Role`. Task 11's `updateRole` mirrors it.

- [ ] **Step 1: Add the handler**

Append to `orgs.rs`:

```rust
#[derive(Deserialize)]
pub struct UpdateRoleReq {
    pub name: Option<String>,
    pub description: Option<String>,
    pub permissions: Option<Vec<String>>,
}

/// Edit a role this org owns.
///
/// Presets are refused: `ensure_preset_roles` re-syncs them from rbac.rs at
/// every API boot, so an edit would silently revert on the next restart —
/// worse than not offering it.
pub async fn update_role_handler(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((org_id, role_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateRoleReq>,
) -> Result<Json<Role>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::ROLE_MANAGE).await?;

    let role = repo::get_role(&mut conn, role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if role.org_id != Some(org_id) {
        // Covers both a preset (org_id NULL) and another org's role. Returning
        // NotFound rather than Forbidden avoids confirming the role exists.
        if role.is_system {
            return Err(ApiError::BadRequest(
                "system roles cannot be edited".into(),
            ));
        }
        return Err(ApiError::NotFound);
    }
    if role.is_system {
        return Err(ApiError::BadRequest("system roles cannot be edited".into()));
    }

    let name = req.name.clone().unwrap_or_else(|| role.name.clone());
    if name.trim().is_empty() {
        return Err(ApiError::BadRequest("role name is required".into()));
    }
    let description = req
        .description
        .clone()
        .unwrap_or_else(|| role.description.clone());

    let old_perms = role_permissions(&role.permissions);
    let new_perms = req.permissions.clone().unwrap_or_else(|| old_perms.clone());

    for p in &new_perms {
        if !perm::ALL.contains(&p.as_str()) {
            return Err(ApiError::BadRequest(format!("unknown permission: {p}")));
        }
    }

    // Both directions. Adding a permission you lack is escalation; removing one
    // you lack is sabotage — a Developer holding role:manage could otherwise
    // strip org:manage from the Admin role and disable everyone above them.
    let own = sauron_auth::effective_at_org(&mut conn, auth.user_id, org_id).await?;
    check_role_edit(&own, &old_perms, &new_perms).map_err(ApiError::Auth)?;

    // A role edit changes every holder's access at once. If this role is the
    // only source of org:manage in the org, dropping it orphans the org exactly
    // as deleting the last owner grant would.
    if old_perms.iter().any(|p| p == perm::ORG_MANAGE)
        && !new_perms.iter().any(|p| p == perm::ORG_MANAGE)
    {
        let remaining =
            repo::count_org_manage_grants_excluding_role(&mut conn, org_id, role_id).await?;
        if remaining == 0 {
            return Err(ApiError::Conflict(
                "this is the org's last role granting org:manage — grant it elsewhere first"
                    .into(),
            ));
        }
    }

    let perms = Value::Array(new_perms.iter().map(|p| Value::String(p.clone())).collect());
    let updated = repo::update_role(&mut conn, org_id, role_id, &name, &description, perms).await?;
    Ok(Json(updated))
}
```

- [ ] **Step 2: Add the supporting count to `repo.rs`**

Beside the other counts from Task 3:

```rust
/// How many grants would still confer `org:manage` in this org if `role_id`
/// stopped conferring it.
pub async fn count_org_manage_grants_excluding_role(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    role_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n \
         FROM role_grants g JOIN roles r ON g.role_id = r.id \
         WHERE g.org_id = $1 AND g.role_id <> $2 AND g.scope_type = 'org' \
           AND r.permissions @> to_jsonb('org:manage'::text)",
    )
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(role_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}
```

- [ ] **Step 3: Confirm the `Role` field names**

Run: `grep -n "struct Role" -A 12 backend/crates/sauron-db/src/models.rs`

The handler reads `role.name`, `role.description`, `role.permissions`, `role.org_id`, `role.is_system`. If `description` is `Option<String>`, adjust the `unwrap_or_else` accordingly and pass `description.as_deref().unwrap_or("")` to `update_role`.

- [ ] **Step 4: Register the route**

```rust
        .route(
            "/v1/orgs/{org_id}/roles/{role_id}",
            patch(routes::orgs::update_role_handler),
        )
```

- [ ] **Step 5: Verify it compiles**

Run: `cd backend && cargo clippy -p sauron-api -- -D warnings`
Expected: clean.

- [ ] **Step 6: Exercise it**

```bash
# Create a custom role, then edit it
CUSTOM=$(curl -s -X POST "http://localhost:8080/v1/orgs/$ORG/roles" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"Triage","description":"","permissions":["issue:read"]}' | jq -r .id)

curl -s -X PATCH "http://localhost:8080/v1/orgs/$ORG/roles/$CUSTOM" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"permissions":["issue:read","issue:write","alert:read"]}' | jq '{name,permissions}'

# A preset — expect 400 "system roles cannot be edited"
OWNER=$(curl -s -H "Authorization: Bearer $TOKEN" \
  "http://localhost:8080/v1/orgs/$ORG/roles" | jq -r '.[]|select(.name=="Owner")|.id')
curl -s -X PATCH "http://localhost:8080/v1/orgs/$ORG/roles/$OWNER" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"permissions":["issue:read"]}' | jq

# Unknown permission — expect 400
curl -s -X PATCH "http://localhost:8080/v1/orgs/$ORG/roles/$CUSTOM" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"permissions":["issue:nope"]}' | jq
```

Expected: the updated role with 3 permissions; then `system roles cannot be edited`; then `unknown permission: issue:nope`.

Note that `alert:read` is one of the 7 permissions currently missing from the dashboard's list — this call proves the backend accepts it, which is what Task 10 fixes on the client.

- [ ] **Step 7: Stage**

```bash
cd backend && cargo fmt && cargo test --workspace
git add backend/bins/sauron-api/src/routes/orgs.rs backend/bins/sauron-api/src/main.rs backend/crates/sauron-db/src/repo.rs
```

---

### Task 10: Permission catalog + parity test

Fixes the drift that would make the role editor destructive. Must land before Task 13.

**Files:**
- Create: `dashboard/src/lib/models/permissions.ts`
- Create: `dashboard/src/lib/models/permissions.test.ts`
- Modify: `dashboard/src/lib/models/index.ts:113-134`

**Interfaces:**
- Produces: `ALL_PERMISSIONS: Permission[]` (23, canonical order), `PERMISSION_GROUPS: PermissionGroup[]`, `PERMISSION_LABELS: Record<string, string>`. Tasks 12 and 13 consume all three.

- [ ] **Step 1: Write the failing parity test**

Create `dashboard/src/lib/models/permissions.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { ALL_PERMISSIONS, PERMISSION_GROUPS, PERMISSION_LABELS } from './permissions';

// Mirror of perm::ALL in backend/crates/sauron-auth/src/rbac.rs:56, in the same
// order. If the backend gains a permission and this list does not, the role
// editor silently strips it from every role on the first save — the checkbox
// grid submits its full state, and a permission with no checkbox reads as
// unchecked.
const BACKEND_CATALOG = [
  'issue:read',
  'issue:write',
  'event:read',
  'funnel:write',
  'artifact:write',
  'source:read',
  'monitor:read',
  'monitor:write',
  'app:read',
  'app:create',
  'app:update',
  'app:delete',
  'app:rotate_key',
  'project:read',
  'project:create',
  'project:update',
  'project:delete',
  'member:read',
  'member:manage',
  'role:manage',
  'org:manage',
  'alert:read',
  'alert:write',
];

describe('permission catalog', () => {
  it('matches the backend catalog exactly, in order', () => {
    expect(ALL_PERMISSIONS).toEqual(BACKEND_CATALOG);
  });

  it('has 23 permissions', () => {
    expect(ALL_PERMISSIONS).toHaveLength(23);
  });

  it('groups every permission exactly once', () => {
    const grouped = PERMISSION_GROUPS.flatMap((g) => g.permissions);
    expect([...grouped].sort()).toEqual([...ALL_PERMISSIONS].sort());
    expect(new Set(grouped).size).toBe(grouped.length);
  });

  it('labels every permission', () => {
    for (const p of ALL_PERMISSIONS) {
      expect(PERMISSION_LABELS[p], `missing label for ${p}`).toBeTruthy();
    }
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd dashboard && npx vitest run src/lib/models/permissions.test.ts`
Expected: FAIL — `Failed to resolve import "./permissions"`.

- [ ] **Step 3: Write the catalog**

Create `dashboard/src/lib/models/permissions.ts`:

```ts
import type { Permission } from './index';

/**
 * Every permission the backend recognises, mirroring perm::ALL in
 * backend/crates/sauron-auth/src/rbac.rs:56 in the same order.
 *
 * Kept complete on purpose. The role editor submits the full checkbox state,
 * so a permission missing from this list is a permission the editor silently
 * removes from any role that has it.
 */
export const ALL_PERMISSIONS: Permission[] = [
  'issue:read',
  'issue:write',
  'event:read',
  'funnel:write',
  'artifact:write',
  'source:read',
  'monitor:read',
  'monitor:write',
  'app:read',
  'app:create',
  'app:update',
  'app:delete',
  'app:rotate_key',
  'project:read',
  'project:create',
  'project:update',
  'project:delete',
  'member:read',
  'member:manage',
  'role:manage',
  'org:manage',
  'alert:read',
  'alert:write',
];

export interface PermissionGroup {
  label: string;
  permissions: Permission[];
}

/** Rendering order for the checkbox grid. Every permission appears once. */
export const PERMISSION_GROUPS: PermissionGroup[] = [
  { label: 'Issues & events', permissions: ['issue:read', 'issue:write', 'event:read'] },
  { label: 'Analytics', permissions: ['funnel:write'] },
  { label: 'Symbolication', permissions: ['artifact:write', 'source:read'] },
  { label: 'Uptime', permissions: ['monitor:read', 'monitor:write'] },
  {
    label: 'Apps',
    permissions: ['app:read', 'app:create', 'app:update', 'app:delete', 'app:rotate_key'],
  },
  {
    label: 'Projects',
    permissions: ['project:read', 'project:create', 'project:update', 'project:delete'],
  },
  { label: 'Alerting', permissions: ['alert:read', 'alert:write'] },
  {
    label: 'Organization',
    permissions: ['member:read', 'member:manage', 'role:manage', 'org:manage'],
  },
];

export const PERMISSION_LABELS: Record<string, string> = {
  'issue:read': 'View issues',
  'issue:write': 'Resolve, assign, and comment on issues',
  'event:read': 'View raw events and analytics',
  'funnel:write': 'Create and edit funnels',
  'artifact:write': 'Upload source maps and debug symbols',
  'source:read': 'View de-obfuscated source code in stack traces',
  'monitor:read': 'View uptime monitors',
  'monitor:write': 'Create and edit uptime monitors',
  'app:read': 'View apps',
  'app:create': 'Create apps',
  'app:update': 'Edit app settings',
  'app:delete': 'Delete apps',
  'app:rotate_key': 'Rotate app ingest keys',
  'project:read': 'View projects',
  'project:create': 'Create projects',
  'project:update': 'Edit project settings',
  'project:delete': 'Delete projects',
  'member:read': 'View members',
  'member:manage': 'Add, edit, and deactivate members',
  'role:manage': 'Create and edit roles',
  'org:manage': 'Manage organization settings',
  'alert:read': 'View alert rules and channels',
  'alert:write': 'Create and edit alert rules and channels',
};
```

`source:read` is the one worth reading twice: it gates de-obfuscated **source code** in stack traces, not symbol names. Its label says so, because an admin ticking boxes has no other way to know.

- [ ] **Step 4: Complete the `Permission` union**

`dashboard/src/lib/models/index.ts:113` is missing four members that exist in the backend catalog: `artifact:write`, `source:read`, `monitor:read`, `monitor:write`. The `(string & {})` fallback means TypeScript never complained. Add them in canonical position:

```ts
export type Permission =
  | 'issue:read'
  | 'issue:write'
  | 'event:read'
  | 'funnel:write'
  | 'artifact:write'
  | 'source:read'
  | 'monitor:read'
  | 'monitor:write'
  | 'app:read'
  | 'app:create'
  | 'app:update'
  | 'app:delete'
  | 'app:rotate_key'
  | 'project:read'
  | 'project:create'
  | 'project:update'
  | 'project:delete'
  | 'member:read'
  | 'member:manage'
  | 'role:manage'
  | 'org:manage'
  | 'alert:read'
  | 'alert:write'
  | (string & {});
```

- [ ] **Step 5: Run to verify it passes**

Run: `cd dashboard && npx vitest run src/lib/models/permissions.test.ts`
Expected: PASS, 4 tests.

- [ ] **Step 6: Typecheck and stage**

```bash
cd dashboard && npm run check
git add dashboard/src/lib/models/permissions.ts dashboard/src/lib/models/permissions.test.ts dashboard/src/lib/models/index.ts
```

---

### Task 11: Models + API client

**Files:**
- Modify: `dashboard/src/lib/models/index.ts`
- Modify: `dashboard/src/lib/api/orgs.ts`
- Modify: `dashboard/src/lib/api/auth.ts`

**Interfaces:**
- Produces:
  - `MemberGrant.is_active: boolean`, `User.must_change_password: boolean`
  - `Member` (grouped-by-user view), `groupMembers(grants: MemberGrant[]): Member[]`
  - `createMember`, `setMemberActive`, `updateGrant`, `updateRole` in `api/orgs.ts`
  - `changePassword` in `api/auth.ts`

  Tasks 12 and 13 consume all of these.

- [ ] **Step 1: Extend the model types**

In `dashboard/src/lib/models/index.ts`, add `is_active` to `MemberGrant` (line 152) and the grouped type beside it:

```ts
export interface MemberGrant {
  id: string;
  user_id: string;
  email: string;
  name: string | null;
  role_id: string;
  role_name: string;
  scope_type: ScopeType;
  scope_id: string;
  is_active: boolean;
}

/**
 * One person, with every grant they hold in the org.
 *
 * The API returns one row per grant. The table renders one row per person:
 * deactivation and editing are per-account, so a member with three grants
 * would otherwise show three identical Deactivate buttons.
 */
export interface Member {
  user_id: string;
  email: string;
  name: string | null;
  is_active: boolean;
  grants: MemberGrant[];
}

export interface CreateMemberPayload {
  email: string;
  name: string;
  role_id: string;
  scope_type: ScopeType;
  scope_id: string;
}

export interface CreateMemberResult {
  user_id: string;
  grant_id: string;
  temp_password: string;
}

export interface UpdateGrantPayload {
  role_id?: string;
  scope_type?: ScopeType;
  scope_id?: string;
}

export interface UpdateRolePayload {
  name?: string;
  description?: string;
  permissions?: Permission[];
}
```

Add `must_change_password: boolean` to the `User` interface (find it with `grep -n 'interface User' -A 10 dashboard/src/lib/models/index.ts`).

Also add `is_active` to the backend's `MemberGrant` serializer — `backend/bins/sauron-api/src/routes/orgs.rs:122` does not currently return it. In `list_members`, `repo::list_org_grants` must also select `users::is_active`, and the tuple destructure at `orgs.rs:144` gains a field. Check `repo::list_org_grants` (`repo.rs:406`) and extend its `.select(...)` and return type from `(RoleGrant, String, String, String)` to `(RoleGrant, String, String, String, bool)`.

- [ ] **Step 2: Add the grouping helper**

Append to `dashboard/src/lib/models/index.ts`:

```ts
/** Collapse the flat grant list into one entry per person, preserving order. */
export function groupMembers(grants: MemberGrant[]): Member[] {
  const byUser = new Map<string, Member>();
  for (const g of grants) {
    const existing = byUser.get(g.user_id);
    if (existing) {
      existing.grants.push(g);
    } else {
      byUser.set(g.user_id, {
        user_id: g.user_id,
        email: g.email,
        name: g.name,
        is_active: g.is_active,
        grants: [g],
      });
    }
  }
  return [...byUser.values()];
}
```

- [ ] **Step 3: Write the grouping test**

Create `dashboard/src/lib/models/group-members.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { groupMembers, type MemberGrant } from './index';

function grant(overrides: Partial<MemberGrant>): MemberGrant {
  return {
    id: 'g1',
    user_id: 'u1',
    email: 'a@example.com',
    name: 'A',
    role_id: 'r1',
    role_name: 'Viewer',
    scope_type: 'org',
    scope_id: 'o1',
    is_active: true,
    ...overrides,
  };
}

describe('groupMembers', () => {
  it('returns one entry per user', () => {
    const out = groupMembers([
      grant({ id: 'g1', user_id: 'u1' }),
      grant({ id: 'g2', user_id: 'u1', scope_type: 'project', scope_id: 'p1' }),
      grant({ id: 'g3', user_id: 'u2', email: 'b@example.com' }),
    ]);
    expect(out).toHaveLength(2);
    expect(out[0].grants.map((g) => g.id)).toEqual(['g1', 'g2']);
    expect(out[1].grants.map((g) => g.id)).toEqual(['g3']);
  });

  it('preserves first-seen order', () => {
    const out = groupMembers([
      grant({ user_id: 'u2', email: 'b@example.com' }),
      grant({ user_id: 'u1', email: 'a@example.com' }),
    ]);
    expect(out.map((m) => m.email)).toEqual(['b@example.com', 'a@example.com']);
  });

  it('carries is_active onto the person', () => {
    const out = groupMembers([grant({ is_active: false })]);
    expect(out[0].is_active).toBe(false);
  });

  it('handles an empty list', () => {
    expect(groupMembers([])).toEqual([]);
  });
});
```

Run: `cd dashboard && npx vitest run src/lib/models/group-members.test.ts`
Expected: PASS, 4 tests.

- [ ] **Step 4: Add the org API functions**

Append to `dashboard/src/lib/api/orgs.ts`, and extend its import block with the new types:

```ts
export async function createMember(
  orgId: string,
  body: CreateMemberPayload,
): Promise<CreateMemberResult> {
  const { data } = await api.post<CreateMemberResult>(`/v1/orgs/${orgId}/members`, body);
  return data;
}

export async function setMemberActive(
  orgId: string,
  userId: string,
  isActive: boolean,
): Promise<void> {
  await api.patch(`/v1/orgs/${orgId}/members/${userId}`, { is_active: isActive });
}

export async function updateGrant(
  grantId: string,
  body: UpdateGrantPayload,
): Promise<{ id: string }> {
  const { data } = await api.patch<{ id: string }>(`/v1/grants/${grantId}`, body);
  return data;
}

export async function updateRole(
  orgId: string,
  roleId: string,
  body: UpdateRolePayload,
): Promise<Role> {
  const { data } = await api.patch<Role>(`/v1/orgs/${orgId}/roles/${roleId}`, body);
  return data;
}
```

- [ ] **Step 5: Add the auth API function**

Append to `dashboard/src/lib/api/auth.ts`:

```ts
/**
 * Goes through the main client, not bareClient: it needs the bearer token, and
 * it is one of only two endpoints the API allows while a password change is
 * outstanding.
 */
export async function changePassword(
  currentPassword: string,
  newPassword: string,
): Promise<AuthSession> {
  const { data } = await api.post<AuthSession>('/v1/auth/password', {
    current_password: currentPassword,
    new_password: newPassword,
  });
  return data;
}
```

- [ ] **Step 6: Typecheck and stage**

```bash
cd dashboard && npm run check && npx vitest run
git add dashboard/src/lib/models/index.ts dashboard/src/lib/models/group-members.test.ts \
        dashboard/src/lib/api/orgs.ts dashboard/src/lib/api/auth.ts
git add backend/bins/sauron-api/src/routes/orgs.rs backend/crates/sauron-db/src/repo.rs
```

---

### Task 12: Forced change-password screen

**Files:**
- Create: `dashboard/src/pages/ChangePassword.svelte`
- Modify: `dashboard/src/lib/stores/auth.svelte.ts`
- Modify: `dashboard/src/routes.ts`

**Interfaces:**
- Consumes: `changePassword` (Task 11), `User.must_change_password`.
- Produces: `authStore.mustChangePassword: boolean`, route `/change-password`.

- [ ] **Step 1: Fix the boot trap**

`boot()` (`auth.svelte.ts:102-109`) calls `getMe()`, then treats **any** throw as "not logged in" and clears local state. Once Task 7's gate ships, `/v1/me` returns `403 password_change_required` for exactly the users this feature is for — so every temp-password holder gets silently logged out on refresh instead of seeing the change screen, and the account becomes unusable.

Rewrite `boot()`:

```ts
  async boot(): Promise<void> {
    this.status = 'booting';
    if (!readRefreshToken()) {
      this.status = 'unauthenticated';
      return;
    }
    try {
      await this.refresh();
    } catch {
      this.clearLocal();
      this.status = 'unauthenticated';
      return;
    }
    try {
      this.user = await authApi.getMe();
      this.status = 'authenticated';
    } catch (err) {
      // A pending password change blocks /v1/me along with everything else.
      // That is a valid session that owes one action, not a failed one —
      // clearing it here would lock the user out of the only screen that can
      // fix it.
      if (isPasswordChangeRequired(err)) {
        this.user = null;
        this.status = 'authenticated';
        return;
      }
      this.clearLocal();
      this.status = 'unauthenticated';
    }
  }
```

Add the helper near the top of the file:

```ts
/** True for the API's 403 password_change_required. */
function isPasswordChangeRequired(err: unknown): boolean {
  const e = err as { response?: { status?: number; data?: { error?: { code?: string } } } };
  return (
    e?.response?.status === 403 &&
    e?.response?.data?.error?.code === 'password_change_required'
  );
}
```

Match the error shape to `errorMessage` in `dashboard/src/lib/api/client.ts` — read it first (`grep -n 'export function errorMessage' -A 15 dashboard/src/lib/api/client.ts`) and reuse whatever accessor it already uses for `error.code`, rather than duplicating a second guess at the envelope.

- [ ] **Step 2: Track the flag on the store**

Add to the `AuthStore` class:

```ts
  /**
   * The session is valid but owes a password change. Derived from the user
   * object when we have one; when /v1/me was blocked we have no user, and the
   * block itself is the signal.
   */
  mustChangePassword = $state(false);
```

Set it in `login`, `register`, and `boot`:

```ts
    this.mustChangePassword = session.user.must_change_password;   // login / register
```
```ts
      if (isPasswordChangeRequired(err)) {
        this.user = null;
        this.mustChangePassword = true;
        this.status = 'authenticated';
        return;
      }
```

Clear it in `clearLocal()`:

```ts
    this.mustChangePassword = false;
```

- [ ] **Step 3: Redirect from the route guard**

`guarded()` (`routes.ts:38`) wraps components with an `authed` condition. Add a second condition so no authenticated page renders while a change is outstanding:

```ts
function passwordCurrent(): boolean {
  if (authStore.mustChangePassword) {
    replace('/change-password');
    return false;
  }
  return true;
}

function guarded(component: Component<never>) {
  return wrap({ component: component as never, conditions: [authed, passwordCurrent] });
}
```

Import `replace` from `svelte-spa-router` if it is not already imported. Register the new page **ungated** so it does not redirect to itself:

```ts
  '/change-password': wrap({ component: ChangePassword as never, conditions: [authed] }),
```

This is convenience, not enforcement. The real block is Task 7's extractor gate; if this redirect were the only barrier, a curl with the temp password would still work.

- [ ] **Step 4: Build the page**

Create `dashboard/src/pages/ChangePassword.svelte`:

```svelte
<script lang="ts">
  import { replace } from 'svelte-spa-router';
  import Button from '../lib/components/ui/Button.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import { changePassword } from '../lib/api/auth';
  import { errorMessage } from '../lib/api/client';
  import { authStore } from '../lib/stores/auth.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';

  let currentPassword = $state('');
  let newPassword = $state('');
  let confirmPassword = $state('');
  let saving = $state(false);
  let error = $state<string | null>(null);

  const tooShort = $derived(newPassword.length > 0 && newPassword.length < 8);
  const mismatch = $derived(confirmPassword.length > 0 && confirmPassword !== newPassword);
  const canSubmit = $derived(
    !saving &&
      currentPassword.length > 0 &&
      newPassword.length >= 8 &&
      confirmPassword === newPassword &&
      newPassword !== currentPassword,
  );

  async function submit(e: Event) {
    e.preventDefault();
    if (!canSubmit) return;
    saving = true;
    error = null;
    try {
      await authStore.applyPasswordChange(currentPassword, newPassword);
      toastStore.success('Password updated.');
      replace('/');
    } catch (err) {
      error = errorMessage(err);
    } finally {
      saving = false;
    }
  }
</script>

<div class="change-password">
  <Card title="Choose a password">
    <p class="lede">
      Your account was created with a temporary password. Choose your own before continuing.
    </p>
    <form onsubmit={submit}>
      <Input
        label="Current password"
        type="password"
        bind:value={currentPassword}
        autocomplete="current-password"
        required
      />
      <Input
        label="New password"
        type="password"
        bind:value={newPassword}
        autocomplete="new-password"
        hint="At least 8 characters."
        error={tooShort ? 'Must be at least 8 characters.' : undefined}
        required
      />
      <Input
        label="Confirm new password"
        type="password"
        bind:value={confirmPassword}
        autocomplete="new-password"
        error={mismatch ? 'Passwords do not match.' : undefined}
        required
      />
      {#if error}<p class="error">{error}</p>{/if}
      <Button type="submit" variant="primary" disabled={!canSubmit} loading={saving}>
        Update password
      </Button>
    </form>
  </Card>
</div>
```

Check `Input.svelte`'s actual props before writing this (`grep -n '\$props' -A 20 dashboard/src/lib/components/ui/Input.svelte`) — `label`, `hint`, `error`, and `loading` on `Button` are assumed here. Match the real prop names, and follow how `Login.svelte` lays out its form for the surrounding markup and styles.

- [ ] **Step 5: Add the store action**

The page calls `authStore.applyPasswordChange` rather than the API directly, because the response carries a new token pair that has to land in the store and localStorage — a component writing those would duplicate `login`'s bookkeeping. Add to `AuthStore`:

```ts
  /**
   * Change the password and adopt the fresh session the server returns. The
   * old refresh token is revoked server-side, so the new pair must replace it
   * here or the next refresh fails.
   */
  async applyPasswordChange(currentPassword: string, newPassword: string): Promise<void> {
    const session = await authApi.changePassword(currentPassword, newPassword);
    this.accessToken = session.access_token;
    this.user = session.user;
    writeRefreshToken(session.refresh_token);
    this.mustChangePassword = false;
    this.status = 'authenticated';
  }
```

- [ ] **Step 6: Verify in the browser**

Start the dashboard, log in as the member created in Task 5 with the temp password.

Expected: redirected to `/change-password`; navigating manually to `#/issues` bounces back; submitting a valid new password lands on the overview with data loading normally. Reload the page mid-flow (before changing) and confirm you stay on the change screen rather than being logged out — that is the Step 1 regression.

- [ ] **Step 7: Typecheck and stage**

```bash
cd dashboard && npm run check
git add dashboard/src/pages/ChangePassword.svelte dashboard/src/lib/stores/auth.svelte.ts dashboard/src/routes.ts
```

---

### Task 13: `PermissionPicker` + `RoleEditorDialog`

The roles half of the UI. Independently verifiable: the New role button keeps working and gains an Edit sibling, before the members table is touched at all.

**Files:**
- Create: `dashboard/src/lib/components/members/PermissionPicker.svelte`
- Create: `dashboard/src/lib/components/members/RoleEditorDialog.svelte`
- Modify: `dashboard/src/pages/Members.svelte` (swap the inline role form for the dialog)

**Interfaces:**
- Consumes: `PERMISSION_GROUPS`, `PERMISSION_LABELS` (Task 10); `createRole`, `updateRole` (Task 11).
- Produces:
  - `PermissionPicker` props: `{ selected: Permission[], disabled?: boolean, onchange: (next: Permission[]) => void }`
  - `RoleEditorDialog` props: `{ open: boolean, orgId: string, role: Role | null, memberCount?: number, onclose: () => void, onsaved: (role: Role) => void }` — `role: null` means create.

- [ ] **Step 1: Build `PermissionPicker`**

Create `dashboard/src/lib/components/members/PermissionPicker.svelte`:

```svelte
<script lang="ts">
  import { PERMISSION_GROUPS, PERMISSION_LABELS } from '../../models/permissions';
  import type { Permission } from '../../models';

  interface Props {
    selected: Permission[];
    disabled?: boolean;
    onchange: (next: Permission[]) => void;
  }

  let { selected, disabled = false, onchange }: Props = $props();

  const selectedSet = $derived(new Set(selected));

  function toggle(permission: Permission) {
    if (disabled) return;
    const next = new Set(selectedSet);
    if (next.has(permission)) next.delete(permission);
    else next.add(permission);
    // Emit in catalog order so a role's stored array is stable regardless of
    // the order the boxes were clicked in.
    onchange(
      PERMISSION_GROUPS.flatMap((g) => g.permissions).filter((p) => next.has(p)),
    );
  }
</script>

<div class="permission-picker" class:disabled>
  {#each PERMISSION_GROUPS as group (group.label)}
    <fieldset>
      <legend>{group.label}</legend>
      {#each group.permissions as permission (permission)}
        <label class="permission">
          <input
            type="checkbox"
            checked={selectedSet.has(permission)}
            {disabled}
            onchange={() => toggle(permission)}
          />
          <span class="name">{permission}</span>
          <span class="description">{PERMISSION_LABELS[permission]}</span>
        </label>
      {/each}
    </fieldset>
  {/each}
</div>
```

A raw `<input type="checkbox">` is fine here — the house kit has no Checkbox component (`ls dashboard/src/lib/components/ui/`). Confirm that is still true; if a Checkbox exists, use it.

- [ ] **Step 2: Build `RoleEditorDialog`**

Create `dashboard/src/lib/components/members/RoleEditorDialog.svelte`. It handles create and edit, because the two differ only in which endpoint they call and whether the fields start populated — two components would duplicate the whole permission grid.

```svelte
<script lang="ts">
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import Badge from '../ui/Badge.svelte';
  import PermissionPicker from './PermissionPicker.svelte';
  import { createRole, updateRole } from '../../api/orgs';
  import { errorMessage } from '../../api/client';
  import type { Permission, Role } from '../../models';

  interface Props {
    open: boolean;
    orgId: string;
    /** null = create a new role. */
    role: Role | null;
    /** How many members hold this role; shown as an impact warning on edit. */
    memberCount?: number;
    onclose: () => void;
    onsaved: (role: Role) => void;
  }

  let { open, orgId, role, memberCount = 0, onclose, onsaved }: Props = $props();

  let name = $state('');
  let description = $state('');
  let permissions = $state<Permission[]>([]);
  let saving = $state(false);
  let error = $state<string | null>(null);

  const isEdit = $derived(role !== null);
  // Presets are re-synced from rbac.rs at every API boot, so an edit would
  // revert on the next restart. Show them, never write them.
  const readOnly = $derived(role?.is_system === true);
  const title = $derived(
    readOnly ? `Role: ${role?.name}` : isEdit ? `Edit ${role?.name}` : 'New role',
  );

  // Repopulate whenever the dialog opens on a different role.
  $effect(() => {
    if (!open) return;
    name = role?.name ?? '';
    description = role?.description ?? '';
    permissions = [...(role?.permissions ?? [])];
    error = null;
  });

  const canSubmit = $derived(!saving && !readOnly && name.trim().length > 0);

  async function submit() {
    if (!canSubmit) return;
    saving = true;
    error = null;
    try {
      const saved = role
        ? await updateRole(orgId, role.id, { name, description, permissions })
        : await createRole(orgId, { name, description, permissions });
      onsaved(saved);
      onclose();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      saving = false;
    }
  }
</script>

<Modal {open} {title} onclose={onclose}>
  {#if readOnly}
    <p class="lede">
      <Badge tone="neutral">system</Badge>
      Built-in roles cannot be edited. Create a custom role to define your own permission set.
    </p>
  {:else}
    <Input label="Name" bind:value={name} required />
    <Input label="Description" bind:value={description} />
    {#if isEdit && memberCount > 0}
      <p class="warning">
        {memberCount} {memberCount === 1 ? 'member holds' : 'members hold'} this role. Saving
        changes their access immediately.
      </p>
    {/if}
  {/if}

  <PermissionPicker
    selected={permissions}
    disabled={readOnly}
    onchange={(next) => (permissions = next)}
  />

  {#if error}<p class="error">{error}</p>{/if}

  {#snippet footer()}
    <Button variant="ghost" onclick={onclose}>{readOnly ? 'Close' : 'Cancel'}</Button>
    {#if !readOnly}
      <Button variant="primary" disabled={!canSubmit} loading={saving} onclick={submit}>
        {isEdit ? 'Save changes' : 'Create role'}
      </Button>
    {/if}
  {/snippet}
</Modal>
```

Read `Modal.svelte`'s props first (`grep -n '\$props' -A 15 dashboard/src/lib/components/ui/Modal.svelte`) — whether it takes `title`/`onclose` and whether it exposes a `footer` snippet or expects buttons in the default slot. Match it exactly; the snippet syntax above is a guess at the house pattern.

- [ ] **Step 3: Wire it into `Members.svelte`**

Replace the inline role form (the `showRoleForm` / `roleName` / `roleDescription` / `rolePerms` / `creatingRole` state and its markup) with dialog state:

```ts
  let roleDialogOpen = $state(false);
  let editingRole = $state<Role | null>(null);

  const roleMemberCounts = $derived.by(() => {
    const counts: Record<string, number> = {};
    for (const m of members) counts[m.role_id] = (counts[m.role_id] ?? 0) + 1;
    return counts;
  });

  function openNewRole() {
    editingRole = null;
    roleDialogOpen = true;
  }

  function openEditRole(role: Role) {
    editingRole = role;
    roleDialogOpen = true;
  }

  function onRoleSaved(saved: Role) {
    const i = roles.findIndex((r) => r.id === saved.id);
    if (i >= 0) roles[i] = saved;
    else roles = [...roles, saved];
    toastStore.success(`Role "${saved.name}" saved.`);
  }
```

Delete the now-unused `ALL_PERMISSIONS` constant at the top of `Members.svelte` — Task 10's catalog supersedes it. This deletion is the point of Task 10: leaving both means the next editor picks the wrong one.

In the Roles list, give every row an action button:

```svelte
      <Button
        variant="ghost"
        size="sm"
        onclick={() => openEditRole(role)}
        disabled={!canManageRoles && !role.is_system}
      >
        {role.is_system ? 'View' : 'Edit'}
      </Button>
```

System roles read "View" and open read-only for everyone with `member:read`; custom roles read "Edit" and require `role:manage`.

And render the dialog once, near the end of the template:

```svelte
{#if sessionStore.currentOrg}
  <RoleEditorDialog
    open={roleDialogOpen}
    orgId={sessionStore.currentOrg.id}
    role={editingRole}
    memberCount={editingRole ? (roleMemberCounts[editingRole.id] ?? 0) : 0}
    onclose={() => (roleDialogOpen = false)}
    onsaved={onRoleSaved}
  />
{/if}
```

- [ ] **Step 4: Verify in the browser**

Expected: "New role" opens an empty dialog with all 23 permissions in 8 groups; creating still works. "Edit" on a custom role opens it populated and saves. "View" on Owner/Admin/Developer/Viewer opens read-only with checkboxes disabled and no Save button.

The regression that matters: create a role with `alert:read` via the dialog, save, reopen, and confirm `alert:read` is still ticked. Before Task 10 it would have been silently dropped.

- [ ] **Step 5: Typecheck and stage**

```bash
cd dashboard && npm run check
git add dashboard/src/lib/components/members/PermissionPicker.svelte \
        dashboard/src/lib/components/members/RoleEditorDialog.svelte \
        dashboard/src/pages/Members.svelte
```

---

### Task 14: `CreateMemberDialog` + `EditMemberDialog`

**Files:**
- Create: `dashboard/src/lib/components/members/CreateMemberDialog.svelte`
- Create: `dashboard/src/lib/components/members/EditMemberDialog.svelte`

**Interfaces:**
- Consumes: `createMember`, `updateGrant`, `createGrant` (Task 11); the `ScopeOption[]` shape already in `Members.svelte:66-91`.
- Produces:
  - `CreateMemberDialog` props: `{ open, orgId, roles: Role[], scopeOptions: ScopeOption[], onclose, oncreated: () => void }`
  - `EditMemberDialog` props: `{ open, orgId, member: Member | null, roles: Role[], scopeOptions: ScopeOption[], onclose, onchanged: () => void }`
  - `ScopeOption` moved to `dashboard/src/lib/models/index.ts` so all three files share one definition.

- [ ] **Step 1: Move `ScopeOption` into the models**

Cut the interface from `Members.svelte:71-76` and put it in `dashboard/src/lib/models/index.ts`:

```ts
/** One entry in the scope picker: the org, a project, or an app. */
export interface ScopeOption {
  key: string;   // `${scope_type}:${scope_id}`
  label: string;
  scope_type: ScopeType;
  scope_id: string;
}
```

Import it back into `Members.svelte`. Both dialogs take it as a prop rather than rebuilding it — the derivation needs `sessionStore.projects` plus a fetch of every project's apps, and duplicating that in three components would triple the requests.

- [ ] **Step 2: Build `CreateMemberDialog`**

```svelte
<script lang="ts">
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import CopyButton from '../ui/CopyButton.svelte';
  import { createMember } from '../../api/orgs';
  import { errorMessage } from '../../api/client';
  import type { Role, ScopeOption } from '../../models';

  interface Props {
    open: boolean;
    orgId: string;
    roles: Role[];
    scopeOptions: ScopeOption[];
    onclose: () => void;
    oncreated: () => void;
  }

  let { open, orgId, roles, scopeOptions, onclose, oncreated }: Props = $props();

  let email = $state('');
  let name = $state('');
  let roleId = $state('');
  let scopeKey = $state('');
  let saving = $state(false);
  let error = $state<string | null>(null);
  /** Set once the account exists. The dialog switches to the reveal panel. */
  let tempPassword = $state<string | null>(null);

  $effect(() => {
    if (!open) return;
    email = '';
    name = '';
    roleId = roles[0]?.id ?? '';
    scopeKey = scopeOptions[0]?.key ?? '';
    tempPassword = null;
    error = null;
  });

  const canSubmit = $derived(
    !saving && email.includes('@') && roleId !== '' && scopeKey !== '',
  );

  async function submit() {
    if (!canSubmit) return;
    const scope = scopeOptions.find((s) => s.key === scopeKey);
    if (!scope) return;
    saving = true;
    error = null;
    try {
      const result = await createMember(orgId, {
        email,
        name,
        role_id: roleId,
        scope_type: scope.scope_type,
        scope_id: scope.scope_id,
      });
      // Reveal, do not close. This is the only time this value exists.
      tempPassword = result.temp_password;
      oncreated();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      saving = false;
    }
  }
</script>

<Modal {open} title={tempPassword ? 'Member created' : 'Create member'} onclose={onclose}>
  {#if tempPassword}
    <p class="lede">
      Give <strong>{email}</strong> this temporary password. They must change it the first
      time they sign in, and it will not do anything else until they do.
    </p>
    <div class="temp-password">
      <code>{tempPassword}</code>
      <CopyButton value={tempPassword} />
    </div>
    <p class="warning">
      This is the only time it is shown. If you lose it, deactivate the account and create
      it again.
    </p>
  {:else}
    <Input label="Email" type="email" bind:value={email} required />
    <Input label="Name" bind:value={name} />
    <label>
      Role
      <select bind:value={roleId}>
        {#each roles as role (role.id)}<option value={role.id}>{role.name}</option>{/each}
      </select>
    </label>
    <label>
      Scope
      <select bind:value={scopeKey}>
        {#each scopeOptions as opt (opt.key)}<option value={opt.key}>{opt.label}</option>{/each}
      </select>
    </label>
    {#if error}<p class="error">{error}</p>{/if}
  {/if}

  {#snippet footer()}
    {#if tempPassword}
      <Button variant="primary" onclick={onclose}>Done</Button>
    {:else}
      <Button variant="ghost" onclick={onclose}>Cancel</Button>
      <Button variant="primary" disabled={!canSubmit} loading={saving} onclick={submit}>
        Create member
      </Button>
    {/if}
  {/snippet}
</Modal>
```

The dialog deliberately stays open after a successful create. Closing on success would destroy the only copy of the password. `oncreated()` fires immediately so the table refreshes behind the reveal panel.

Reuse the existing `<select>` markup and classes from the current Grant access form (`Members.svelte:224-247`) rather than the bare elements above, so the controls match the rest of the page.

- [ ] **Step 3: Build `EditMemberDialog`**

```svelte
<script lang="ts">
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import { createGrant, updateGrant } from '../../api/orgs';
  import { errorMessage } from '../../api/client';
  import type { Member, MemberGrant, Role, ScopeOption } from '../../models';

  interface Props {
    open: boolean;
    orgId: string;
    member: Member | null;
    roles: Role[];
    scopeOptions: ScopeOption[];
    onclose: () => void;
    onchanged: () => void;
  }

  let { open, orgId, member, roles, scopeOptions, onclose, onchanged }: Props = $props();

  /** Pending role/scope selection per existing grant id. */
  let edits = $state<Record<string, { roleId: string; scopeKey: string }>>({});
  /** The "add another grant" row. */
  let addRoleId = $state('');
  let addScopeKey = $state('');
  let saving = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!open || !member) return;
    const next: Record<string, { roleId: string; scopeKey: string }> = {};
    for (const g of member.grants) {
      next[g.id] = { roleId: g.role_id, scopeKey: `${g.scope_type}:${g.scope_id}` };
    }
    edits = next;
    addRoleId = roles[0]?.id ?? '';
    addScopeKey = scopeOptions[0]?.key ?? '';
    error = null;
  });

  function isDirty(grant: MemberGrant): boolean {
    const e = edits[grant.id];
    return (
      !!e &&
      (e.roleId !== grant.role_id || e.scopeKey !== `${grant.scope_type}:${grant.scope_id}`)
    );
  }

  async function saveGrant(grant: MemberGrant) {
    const e = edits[grant.id];
    const scope = scopeOptions.find((s) => s.key === e.scopeKey);
    if (!scope) return;
    saving = true;
    error = null;
    try {
      await updateGrant(grant.id, {
        role_id: e.roleId,
        scope_type: scope.scope_type,
        scope_id: scope.scope_id,
      });
      onchanged();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      saving = false;
    }
  }

  async function addGrant() {
    if (!member) return;
    const scope = scopeOptions.find((s) => s.key === addScopeKey);
    if (!scope) return;
    saving = true;
    error = null;
    try {
      await createGrant(orgId, {
        email: member.email,
        role_id: addRoleId,
        scope_type: scope.scope_type,
        scope_id: scope.scope_id,
      });
      onchanged();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      saving = false;
    }
  }
</script>
```

Each grant row gets its own role `<select>`, scope `<select>`, and a Save button enabled only when `isDirty(grant)`. Per-grant saves rather than one bulk submit: each `PATCH` is independently authorized and can fail on its own guard (last-owner, duplicate), and a bulk save would have to report a partial failure with no way to roll the successful ones back.

`addGrant` uses `createGrant` with the member's email, which is the existing endpoint — no new backend surface needed for "give this person a second grant".

- [ ] **Step 4: Typecheck and stage**

```bash
cd dashboard && npm run check
git add dashboard/src/lib/components/members/CreateMemberDialog.svelte \
        dashboard/src/lib/components/members/EditMemberDialog.svelte \
        dashboard/src/lib/models/index.ts dashboard/src/pages/Members.svelte
```

These components are not reachable from the UI until Task 15 wires them in; `npm run check` passing is this task's gate.

---

### Task 15: Members page — regrouped table + wiring

Brings the dialogs onto the page and switches the table from one-row-per-grant to one-row-per-person.

**Files:**
- Modify: `dashboard/src/pages/Members.svelte`

**Interfaces:**
- Consumes: `groupMembers`, `setMemberActive` (Task 11); `CreateMemberDialog`, `EditMemberDialog` (Task 14); `RoleEditorDialog` (Task 13).
- Produces: the finished page. Nothing else consumes it.

- [ ] **Step 1: Add the member state and derivations**

```ts
  import { groupMembers, type Member } from '../lib/models';
  import { setMemberActive } from '../lib/api/orgs';
  import CreateMemberDialog from '../lib/components/members/CreateMemberDialog.svelte';
  import EditMemberDialog from '../lib/components/members/EditMemberDialog.svelte';

  const grouped = $derived(groupMembers(members));

  let createOpen = $state(false);
  let editingMember = $state<Member | null>(null);
  let togglingUserId = $state<string | null>(null);
```

- [ ] **Step 2: Add the deactivate action**

```ts
  async function toggleActive(member: Member) {
    const org = sessionStore.currentOrg;
    if (!org) return;
    togglingUserId = member.user_id;
    try {
      await setMemberActive(org.id, member.user_id, !member.is_active);
      toastStore.success(
        member.is_active
          ? `${member.email} can no longer sign in.`
          : `${member.email} can sign in again.`,
      );
      await load(org.id);
    } catch (err) {
      // The backend's 409s carry the actionable text (last owner, cross-org,
      // self) — surface it verbatim rather than a generic failure.
      toastStore.error(errorMessage(err));
    } finally {
      togglingUserId = null;
    }
  }
```

Deactivation goes through `ConfirmDialog` (the house component already used elsewhere) rather than firing on click — it signs someone out of every device. Reactivation does not need a confirm.

- [ ] **Step 3: Regroup the table**

Replace the members table body so it iterates `grouped` instead of `members`. Each row:

| Column | Content |
|---|---|
| Member | Avatar initials, name, email, plus a `<Badge tone="warning">Deactivated</Badge>` when `!member.is_active` |
| Role | One `<Badge>` per grant showing `role_name` |
| Scope | One chip per grant: `scopeLabel(grant)`, each with its own Remove button (existing `deleteGrant`) |
| Actions | Edit (`canManage`), Deactivate / Reactivate (`canManage`) |

Keep the existing `scopeLabel` and `scopeTone` helpers as-is — they take a `MemberGrant`, and the chips still pass one. Dim the whole row when `!member.is_active`.

- [ ] **Step 4: Add the Create member button**

Beside the existing "Grant access" card header:

```svelte
  {#if canManage}
    <Button variant="primary" onclick={() => (createOpen = true)}>Create member</Button>
  {/if}
```

Keep the Grant access form. The two are different jobs — grant is for someone who already has an account (including members of other orgs), create is for someone who does not. Removing it would break the only path for the former.

Retitle the existing form's helper text to say so, and rename the stale `inviteEmail` / `inviting` variables (`Members.svelte:49-52`) to `grantEmail` / `granting`. The "Invite / grant form" comment at line 48 describes an invitation flow that does not exist and never did.

- [ ] **Step 5: Render the dialogs**

```svelte
{#if sessionStore.currentOrg}
  <CreateMemberDialog
    open={createOpen}
    orgId={sessionStore.currentOrg.id}
    {roles}
    {scopeOptions}
    onclose={() => (createOpen = false)}
    oncreated={() => load(sessionStore.currentOrg!.id)}
  />
  <EditMemberDialog
    open={editingMember !== null}
    orgId={sessionStore.currentOrg.id}
    member={editingMember}
    {roles}
    {scopeOptions}
    onclose={() => (editingMember = null)}
    onchanged={() => load(sessionStore.currentOrg!.id)}
  />
{/if}
```

- [ ] **Step 6: Check the file size**

Run: `wc -l dashboard/src/pages/Members.svelte`
Expected: meaningfully below the 546 it started at — the role form, permission list, and grant form internals have all moved out. If it grew instead, something was copied rather than moved.

- [ ] **Step 7: Verify in the browser**

Expected: a member holding two grants shows as **one** row with two scope chips and one Deactivate button. Deactivating badges the row and dims it; the member stays listed. Reactivating restores it. Edit opens the dialog populated with both grants.

- [ ] **Step 8: Typecheck and stage**

```bash
cd dashboard && npm run check && npx vitest run
git add dashboard/src/pages/Members.svelte
```

---

### Task 16: Docs + end-to-end verification

**Files:**
- Modify: `dashboard/src/pages/Docs.svelte:1127-1134`
- Modify: `docs/superpowers/specs/2026-07-26-member-lifecycle-design.md` (status line)

- [ ] **Step 1: Update the in-app docs**

`Docs.svelte:1127-1134` currently describes members as grant-only. Rewrite that section to cover: creating a member and handing over the temp password; that the member must change it on first sign-in; that deactivation blocks sign-in without removing grants; and that presets are read-only while custom roles are editable.

- [ ] **Step 2: Run the full suite**

```bash
cd backend && cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace
cd ../dashboard && npm run check && npx vitest run
```

Expected: all green. `cargo fmt --check` rather than `cargo fmt` — this is the gate, so it should fail rather than silently fix.

- [ ] **Step 3: Full end-to-end walkthrough**

Against a running API + dashboard, as an Owner. This is the real gate for everything the unit tests cannot reach — the transaction, the cross-org guard, token revocation, and the extractor gate.

1. Members → **Create member** with a Viewer role at org scope. Copy the temp password.
2. Confirm the reveal panel warns it is shown once, and the new member appears in the table after closing.
3. Sign out. Sign in as the new member with the temp password.
4. Confirm the forced change screen appears and `#/issues` bounces back to it.
5. **Reload the page.** Confirm you stay on the change screen — not signed out. (Task 12 Step 1.)
6. Change the password. Confirm you land on the overview with data loading.
7. Confirm the old temp password no longer signs in.
8. Sign back in as the Owner. Edit the member's role to Developer; confirm the table updates.
9. Add a second grant at project scope; confirm the row shows two chips and one Deactivate button.
10. Deactivate them. Confirm the row is badged and dimmed, and both grants survive.
11. Confirm they cannot sign in: `403 account_deactivated`. Confirm a *wrong* password on that account still returns `401 invalid_credentials`.
12. Reactivate. Confirm sign-in works and both grants are intact.
13. Create a custom role with `alert:read` and `source:read`; save, reopen, confirm both are still ticked.
14. Open Owner from the Roles list; confirm it is read-only with no Save button.
15. Try to deactivate yourself; confirm the 409 message appears in a toast.

- [ ] **Step 4: Confirm the migration is reversible**

```bash
cd backend && cargo run --bin sauron-migrate -- revert && cargo run --bin sauron-migrate
```

Check the exact revert flag first (`cargo run --bin sauron-migrate -- --help`); if the binary has no revert path, apply `down.sql` by hand with psql and re-run the migration. Expected: no error, `\d users` shows the columns gone and then back.

- [ ] **Step 5: Mark the spec implemented**

Change the spec's `Status:` line to `implemented 2026-07-26`.

- [ ] **Step 6: Stage everything**

```bash
git add -A
git status
```

Report the full diff summary. **Do not commit** — the user commits.

---

## Self-Review

**Spec coverage**

| Spec section | Task |
|---|---|
| §1 Migration | 1 |
| §2 Create member | 5 (endpoint), 14 (dialog), 15 (wiring) |
| §3 Deactivate/reactivate + login/refresh checks | 6 (endpoint), 7 (auth checks), 15 (UI) |
| §4 Forced password change | 7 (endpoint + gate), 12 (screen) |
| §5 Edit a member's grant | 8 (endpoint), 14 (dialog) |
| §6 Edit a custom role | 9 (endpoint), 13 (dialog) |
| §7 Permission drift fix | 10 |
| §7 Component split | 13, 14, 15 |
| §7 Table regrouped by member | 11 (`groupMembers`), 15 (render) |
| §7 API client | 11 |
| Error handling table | 5, 6, 7, 8, 9 |
| Testing (guard extraction) | 2; unit tests in 2, 7, 10, 11 |
| Docs | 16 |

Every spec section maps to a task.

**Deviations from the spec, decided while planning**

1. The spec put guard extraction under Testing. It is Task 2, before any handler, so Tasks 5–9 call one canonical guard instead of copying the inline loop twice more.
2. The spec did not mention `boot()`. Task 12 Step 1 fixes it: `getMe()` throwing `password_change_required` would otherwise sign out exactly the users this feature creates.
3. The spec did not mention `refresh`. Task 7 Step 11 adds the `is_active` check there; without it a deactivated member with a stored refresh token keeps minting access tokens and never actually loses access.
4. The spec said `list_members` returns `is_active`; the existing `MemberGrant` serializer and `repo::list_org_grants` do not select it. Task 11 Step 1 extends both.
5. Added `REVOKE_PASSWORD_CHANGED` (Task 7 Step 12) — reusing `REVOKE_DEACTIVATED` for a self-service change would misreport why a session ended.
6. Added `count_org_manage_grants_excluding_role` (Task 9 Step 2) — the existing count excludes one *grant*, but a role edit affects every grant holding that role.
7. Task 8 also guards a grant being moved *off* org scope while carrying `org:manage`, which the spec's last-owner rule implies but does not state.

**Type consistency**

- `issue_access` gains its second parameter in Task 7 Step 3; its one call site is updated in Step 8. ✓
- `issue_tokens` gains its fifth parameter in Task 7 Step 8; all five call sites are listed in Step 9. ✓
- `MemberGrant.is_active` is added to the TS type and the Rust serializer in the same task (11). ✓
- `ScopeOption` is defined once in `models/index.ts` (Task 14 Step 1) and imported by `Members.svelte`, `CreateMemberDialog`, and `EditMemberDialog`. ✓
- Handler names avoid collisions with the repo functions they call: `update_grant_handler` / `repo::update_grant`, `update_role_handler` / `repo::update_role`. ✓
- `ALL_PERMISSIONS` exists in two places during Tasks 10–12; Task 13 Step 3 deletes the stale copy in `Members.svelte`. Flagged there explicitly. ✓
