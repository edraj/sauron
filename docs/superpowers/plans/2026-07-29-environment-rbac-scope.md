# Environment as an RBAC Scope — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make an environment a grant level a member can be confined to, so an environment they hold no grant on is unreadable rather than merely unselected.

**Architecture:** `rbac::Scope` gains a fourth variant `Env(Uuid)` and the existing cascade (org ⊇ project ⊇ app ⊇ env) extends to it unchanged. A new `authorize_env` resolves the caller's reach on an app into an `EnvFilter`, which gains a `Subset(Vec<Uuid>)` variant so a partial-reach caller's "all environments" narrows to the environments they actually hold. Two cross-environment data defects that become *access* defects once the boundary is real — `issues` string/level fields and `sessions` crash counters — are fixed by deriving per-environment values from the environment-stamped child tables.

**Tech Stack:** Rust (axum, diesel-async 0.9.2, tokio), Postgres 16 (partitioned tables), Svelte 5 (runes), vitest, cargo test.

## Global Constraints

Every task's requirements implicitly include this section. These are not suggestions.

- **NEVER run any `diesel` CLI command.** It rewrites `schema.rs` from 27 `diesel::table!` blocks to 87 and still compiles. `grep -c '^diesel::table!' backend/crates/sauron-db/src/schema.rs` must equal **27** at the end of every task — that count is the only detector.
- **NEVER commit. NEVER create a branch.** All work stays in the working tree and is reviewed via diffs. Strip any commit step from your own habits; this plan's tasks deliberately have none.
- **`localhost:5432` is an unrelated Postgres container.** Use the compose container IP: `docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' sauron-postgres-1`. Compose services publish no host ports. The DB tests read `TEST_DATABASE_URL`; a bare `cargo test --workspace` silently *skips* them and still prints `ok`.
- **DuckDB env vars** point at the repo-root cache and are required for `sauron-tier` to build:
  ```bash
  export DUCKDB_LIB_DIR="$PWD/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu"
  export DUCKDB_INCLUDE_DIR="$DUCKDB_LIB_DIR"
  export LD_LIBRARY_PATH="$DUCKDB_LIB_DIR:$LD_LIBRARY_PATH"
  ```
  **Never pass `--all-features`** — DuckDB bundling is off and `--all-features` turns it back on.
- **Scratch artifacts go in `.superpowers/sdd/` prefixed `s3-`.** That directory is shared with three other programmes and is gitignored; an unprefixed `task-N-report.md` has already destroyed two files in this project.
- **Do not disturb existing dev data.** The DB test harness creates and drops its own ephemeral database per run.
- **Verification gate** (run at the end of every task that touches Rust):
  ```bash
  cd backend
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  TEST_DATABASE_URL=postgres://sauron:sauron@<container-ip>:5432/sauron \
    cargo test -p sauron-db --test env_scoping
  cargo test --workspace                    # TEST_DATABASE_URL unset — must still be ok
  grep -c '^diesel::table!' crates/sauron-db/src/schema.rs   # must print 27
  ```
  Dashboard tasks additionally run `npm test`, `npx svelte-check --tsconfig ./tsconfig.json`, `npm run build` from `dashboard/`.
- **Every fix carries a deliberate-break proof.** Revert the fix, run the new test, paste the failure, restore. A test that passes both with and without the fix is not a test.

---

## File Map

**Backend — auth core**
- `backend/crates/sauron-auth/src/rbac.rs` — `Scope::Env`, 4-arg `grant_applies`/`effective_permissions`/`has_permission`, `Reach.envs`, `grants_from_rows` env arm, `authorize_env`.
- `backend/crates/sauron-auth/src/guard.rs` — `scope_parts` env arm, `ResolvedScope::target` 4-tuple.

**Backend — scope plumbing**
- `backend/crates/sauron-db/src/scope.rs` — `EnvFilter::Subset`, `bind_env!` macro, `scope_env!` fourth arm.
- `backend/bins/sauron-api/src/routes/scope.rs` — `read_scope*` become async + authorize-aware.

**Backend — data**
- `backend/migrations/2026-07-29-000029_env_scope_grants/` — `role_grants` CHECK += `'env'`.
- `backend/migrations/2026-07-29-000030_error_event_title_culprit/` — `error_events.title`/`culprit`.
- `backend/crates/sauron-db/src/schema.rs` — **hand-edited**, two columns added to the `error_events` block only.
- `backend/crates/sauron-db/src/models.rs` — `NewErrorEvent` gains the two fields.
- `backend/crates/sauron-pipeline/src/process.rs` — pass the already-computed `title`/`culprit` into `insert_error_event`.
- `backend/crates/sauron-db/src/repo.rs` — `list_issues`/`get_issue`/`top_issues`/`issue_stats` derivation; `overview_totals`/`session_stats` crashed; `list_environments_for_reach`.

**Backend — routes**
- `backend/bins/sauron-api/src/routes/environments.rs` — `list_environments` via reach.
- `backend/bins/sauron-api/src/routes/orgs.rs` — scope_type arms, `Scope::parts()` reuse.
- 12 route files threading the async `read_scope`.

**Dashboard**
- `dashboard/src/lib/models/index.ts` — `ScopeType` union.
- `dashboard/src/lib/models/scope-tree.ts` — `ScopeSelection.envs`, 4-level implication.
- `dashboard/src/lib/models/grant-plan.ts` — `grantsToBlocks` env bucket, `isCovered` 4th case.
- `dashboard/src/lib/components/members/ScopeTree.svelte` — third nested level.
- `dashboard/src/lib/stores/session.svelte.ts` — `can()` env level.

**Tests**
- `backend/crates/sauron-db/tests/common/mod.rs` — seed extension.
- `backend/crates/sauron-db/tests/env_scoping.rs` — derivation + Subset tests.
- `backend/bins/sauron-api/tests/http_env_scoping.rs` — router enumeration + 403 contract.
- `dashboard/src/lib/models/{grant-plan,scope-tree,scope-type}.test.ts`.

---

## Phase 1 — The RBAC core (Tasks 1–4)

Nothing in Phase 1 changes behaviour: no env grants exist yet, so every resolution
must land exactly where it does today. Phase 1's tests are what prove that.

---

### Task 1: `Scope::Env` and the four-level cascade

**Files:**
- Modify: `backend/crates/sauron-auth/src/rbac.rs`
- Modify: `backend/crates/sauron-auth/src/guard.rs:108-135`
- Test: `backend/crates/sauron-auth/src/rbac.rs` (inline `mod tests`)

**Interfaces:**
- Produces: `Scope::Env(Uuid)`; `grant_applies(scope, org, project: Option<Uuid>, app: Option<Uuid>, env: Option<Uuid>) -> bool`; `effective_permissions(grants, org, project, app, env) -> HashSet<String>`; `has_permission(grants, permission, org, project, app, env) -> bool`; `Reach { org: bool, projects: Vec<Uuid>, apps: Vec<Uuid>, envs: Vec<Uuid> }`; `Scope::parts()` returning `("env", id)` for the new variant.
- Consumes: nothing.

- [ ] **Step 1: Write the failing cascade tests**

Append to `mod tests` in `backend/crates/sauron-auth/src/rbac.rs`:

```rust
fn env_a1p() -> Uuid {
    Uuid::from_u128(1000)
}
fn env_a1s() -> Uuid {
    Uuid::from_u128(1001)
}

/// An app grant covers every environment under it, including ones created
/// after the grant was written.
#[test]
fn app_grant_covers_every_environment_under_it() {
    let g = vec![preset_grant(Scope::App(app_a1()), &VIEWER)];
    for env in [env_a1p(), env_a1s()] {
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            Some(env)
        ));
    }
}

/// An env grant covers that environment only — not a sibling environment in
/// the same app, and not the app-level check itself.
#[test]
fn env_grant_covers_that_environment_only() {
    let g = vec![preset_grant(Scope::Env(env_a1p()), &VIEWER)];
    assert!(has_permission(
        &g,
        perm::ISSUE_READ,
        org(),
        Some(proj_a()),
        Some(app_a1()),
        Some(env_a1p())
    ));
    // sibling environment — DENIED
    assert!(!has_permission(
        &g,
        perm::ISSUE_READ,
        org(),
        Some(proj_a()),
        Some(app_a1()),
        Some(env_a1s())
    ));
    // app-level check (env = None) — DENIED: an env grant cannot authorize an
    // app-wide action.
    assert!(!has_permission(
        &g,
        perm::ISSUE_READ,
        org(),
        Some(proj_a()),
        Some(app_a1()),
        None
    ));
    // project and org level — DENIED
    assert!(!has_permission(
        &g,
        perm::ISSUE_READ,
        org(),
        Some(proj_a()),
        None,
        None
    ));
    assert!(!has_permission(&g, perm::ISSUE_READ, org(), None, None, None));
}

#[test]
fn org_and_project_grants_still_reach_environments() {
    let og = vec![preset_grant(Scope::Org(org()), &DEVELOPER)];
    assert!(has_permission(
        &og,
        perm::ISSUE_WRITE,
        org(),
        Some(proj_a()),
        Some(app_a1()),
        Some(env_a1p())
    ));
    let pg = vec![preset_grant(Scope::Project(proj_a()), &DEVELOPER)];
    assert!(has_permission(
        &pg,
        perm::ISSUE_WRITE,
        org(),
        Some(proj_a()),
        Some(app_a1()),
        Some(env_a1p())
    ));
    // sibling project's app+env — DENIED
    assert!(!has_permission(
        &pg,
        perm::ISSUE_WRITE,
        org(),
        Some(proj_b()),
        Some(app_b1()),
        Some(env_a1p())
    ));
}

#[test]
fn reach_for_collects_environments() {
    let g = vec![
        grant(Scope::Env(env_a1p()), &[perm::ISSUE_READ]),
        grant(Scope::Env(env_a1s()), &[perm::ISSUE_READ]),
        grant(Scope::Env(Uuid::from_u128(1002)), &[perm::EVENT_READ]),
    ];
    let reach = reach_for(&g, perm::ISSUE_READ);
    assert!(!reach.org);
    assert!(reach.projects.is_empty());
    assert!(reach.apps.is_empty());
    assert_eq!(reach.envs, vec![env_a1p(), env_a1s()]);
}

/// `grants_from_rows` silently drops unknown scope strings (`_ => return None`).
/// Adding 'env' to the DB CHECK without this arm makes every environment grant
/// vanish at read time with no signal at all. Delete the "env" arm and this
/// test fails.
#[test]
fn grants_from_rows_parses_the_env_scope() {
    let rows = vec![(
        "env".to_string(),
        env_a1p(),
        serde_json::json!(["issue:read"]),
    )];
    let grants = grants_from_rows(rows);
    assert_eq!(grants.len(), 1, "env scope_type must not be dropped");
    assert_eq!(grants[0].scope, Scope::Env(env_a1p()));
}

#[test]
fn scope_parts_round_trips_env() {
    assert_eq!(Scope::Env(env_a1p()).parts(), ("env", env_a1p()));
    let (scope_type, scope_id) = Scope::Env(env_a1p()).parts();
    let rows = vec![(
        scope_type.to_string(),
        scope_id,
        serde_json::json!(["issue:read"]),
    )];
    assert_eq!(grants_from_rows(rows)[0].scope, Scope::Env(env_a1p()));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd backend && cargo test -p sauron-auth 2>&1 | head -40`
Expected: compile FAIL — `no variant named 'Env' found for enum 'Scope'`, and arity errors on `has_permission`.

- [ ] **Step 3: Add the variant and widen the resolution core**

In `backend/crates/sauron-auth/src/rbac.rs`, extend the module doc's cascade paragraph and change:

```rust
/// The level a grant applies at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Org(Uuid),
    Project(Uuid),
    App(Uuid),
    Env(Uuid),
}

impl Scope {
    pub fn parts(self) -> (&'static str, Uuid) {
        match self {
            Scope::Org(id) => ("org", id),
            Scope::Project(id) => ("project", id),
            Scope::App(id) => ("app", id),
            Scope::Env(id) => ("env", id),
        }
    }
}

fn grant_applies(
    scope: Scope,
    org: Uuid,
    project: Option<Uuid>,
    app: Option<Uuid>,
    env: Option<Uuid>,
) -> bool {
    match scope {
        Scope::Org(o) => o == org,
        Scope::Project(p) => Some(p) == project,
        Scope::App(a) => Some(a) == app,
        Scope::Env(e) => Some(e) == env,
    }
}
```

Add `env: Option<Uuid>` as the final parameter of `effective_permissions` and
`has_permission`, threading it into `grant_applies`. Add `envs: Vec<Uuid>` to
`Reach` and a `Scope::Env(e) => reach.envs.push(e)` arm to `reach_for`. Add the
`"env" => Scope::Env(scope_id)` arm to `grants_from_rows`.

- [ ] **Step 4: Fix every call site the compiler names**

Run: `cd backend && cargo build --workspace 2>&1 | grep -E "^error" | head -40`

Add a trailing `None` to each `effective_permissions` / `has_permission` call
that targets an org/project/app (that is, all of them at this point — no caller
knows about environments yet). Do **not** invent env values anywhere in this
task; Task 3 introduces the first real one.

In `backend/crates/sauron-auth/src/guard.rs`, extend `scope_parts` and
`ResolvedScope::target` to carry the environment. `scope_parts` currently
returns `(Option<Uuid>, Option<Uuid>)` and falls back to `(None, None)` for
unknown strings — widen both to a 3-tuple `(project, app, env)` and add:

```rust
"env" => (project_of_app, app_of_env, Some(scope_id)),
```

Keep the unknown-string fallback as the all-`None` org-scope case, and keep its
existing "fail narrow" doc comment accurate by updating it to mention the third
element.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd backend && cargo test -p sauron-auth`
Expected: PASS, including the six new tests.

- [ ] **Step 6: Deliberate-break proof**

Remove the `"env" => Scope::Env(scope_id)` arm from `grants_from_rows`, run
`cargo test -p sauron-auth grants_from_rows_parses_the_env_scope`, paste the
failure into `.superpowers/sdd/s3-task-1-report.md`, then restore it.

- [ ] **Step 7: Run the verification gate**

Run the Global Constraints gate. Record output in `.superpowers/sdd/s3-task-1-report.md`.

---

### Task 2: `role_grants` accepts the `env` scope type

**Files:**
- Create: `backend/migrations/2026-07-29-000029_env_scope_grants/up.sql`
- Create: `backend/migrations/2026-07-29-000029_env_scope_grants/down.sql`
- Test: `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Consumes: `Scope::Env` from Task 1.
- Produces: a `role_grants` table that accepts `scope_type = 'env'`.

- [ ] **Step 1: Write the migration**

`up.sql`:

```sql
-- `role_grants.scope_type` gains 'env', making an environment a grantable
-- scope level (Slice 3). The CHECK constraint created in
-- 2026-07-12-000002_projects_apps_rbac was unnamed, so it carries Postgres's
-- auto-generated name `role_grants_scope_type_check`.
--
-- `scope_id` remains polymorphic with no FK, exactly as for 'app' and
-- 'project' — a retired environment's grants outlive it, which is why the
-- dashboard's grant editor carries an `unmatched` list.
ALTER TABLE role_grants DROP CONSTRAINT role_grants_scope_type_check;
ALTER TABLE role_grants
    ADD CONSTRAINT role_grants_scope_type_check
    CHECK (scope_type IN ('org', 'project', 'app', 'env'));
```

`down.sql`:

```sql
-- Env grants must go BEFORE the narrower constraint is restored, or this
-- migration fails against its own data. This deletes access; it is a
-- destructive rollback by necessity, not by choice.
DELETE FROM role_grants WHERE scope_type = 'env';

ALTER TABLE role_grants DROP CONSTRAINT role_grants_scope_type_check;
ALTER TABLE role_grants
    ADD CONSTRAINT role_grants_scope_type_check
    CHECK (scope_type IN ('org', 'project', 'app'));
```

- [ ] **Step 2: Write the failing test**

Append to `backend/crates/sauron-db/tests/env_scoping.rs`:

```rust
/// The CHECK constraint must accept 'env'. Without migration 29 this insert
/// raises `new row for relation "role_grants" violates check constraint`.
#[tokio::test]
async fn role_grants_accepts_the_env_scope_type() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs(&mut conn).await;

    let inserted = diesel::sql_query(
        "INSERT INTO role_grants (org_id, user_id, role_id, scope_type, scope_id)
         SELECT $1, u.id, r.id, 'env', $2
         FROM users u, roles r
         WHERE u.email = $3 AND r.name = 'Viewer'
         LIMIT 1",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.org_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .bind::<diesel::sql_types::Text, _>(ids.owner_email.clone())
    .execute(&mut conn)
    .await
    .expect("env-scoped grant must be accepted by the CHECK constraint");
    assert_eq!(inserted, 1);
}
```

If `SeedIds` does not already expose `org_id` and `owner_email`, add them in
this task — the seed creates both and simply does not surface them.

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd backend && TEST_DATABASE_URL=postgres://sauron:sauron@<ip>:5432/sauron cargo test -p sauron-db --test env_scoping role_grants_accepts`
Expected: FAIL — `violates check constraint "role_grants_scope_type_check"`.

Note: the harness runs pending migrations on its ephemeral database, so this
fails only until `up.sql` exists — write the test first anyway, so you see it fail.

- [ ] **Step 4: Run the migration and verify the test passes**

Run: `cd backend && cargo run -p sauron-migrate` against the dev database, then
the test command from Step 3.
Expected: PASS.

`sauron-migrate` has **no subcommands** — `cargo run -p sauron-migrate -- revert`
does nothing at all. To exercise `down.sql`, apply it by hand with `psql`.

- [ ] **Step 5: Prove the down migration works**

Apply `down.sql` by hand against a scratch database that has an env grant in it,
confirm it succeeds (the `DELETE` runs first), then re-apply `up.sql`. Paste both
into `.superpowers/sdd/s3-task-2-report.md`. A `down.sql` that fails against its
own data is the failure mode this step exists to catch.

- [ ] **Step 6: Run the verification gate**

---

### Task 3: `EnvFilter::Subset` and the `bind_env!` macro

**Files:**
- Modify: `backend/crates/sauron-db/src/scope.rs`
- Test: `backend/crates/sauron-db/src/scope.rs` (inline `mod tests`)

**Interfaces:**
- Produces: `EnvFilter::Subset(Vec<Uuid>)`; `EnvFilter::sql_fragment(bind_index)` and `sql_fragment_for(alias, bind_index)` emitting `= ANY($n)` for `Subset`; `bind_env!($stmt, $env)` macro; `scope_env!` fourth arm. `EnvFilter` and `ReadScope` are no longer `Copy`.
- Consumes: nothing.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `backend/crates/sauron-db/src/scope.rs`:

```rust
/// `Subset` consumes exactly ONE bind index, like `One` — an array bind is a
/// single placeholder. If it consumed zero (like `All`/`Unattributed`) or two,
/// every subsequent bind in all 25 raw statements would shift.
#[test]
fn subset_reserves_exactly_one_bind_index() {
    let f = EnvFilter::Subset(vec![Uuid::from_u128(1), Uuid::from_u128(2)]);
    assert_eq!(f.sql_fragment(3), " AND environment_id = ANY($3)");
    assert_eq!(f.sql_fragment_for("e", 4), " AND e.environment_id = ANY($4)");
}

/// `= ANY(array)` never matches NULL, which is the correct semantics: an
/// unattributed row belongs to no environment and so belongs to nobody's
/// readable set. This is a documentation test of intent — the SQL behaviour is
/// asserted against the real server in `env_scoping.rs`.
#[test]
fn subset_fragment_uses_any_not_in() {
    let f = EnvFilter::Subset(vec![Uuid::from_u128(1)]);
    assert!(f.sql_fragment(1).contains("= ANY("));
    assert!(!f.sql_fragment(1).contains(" IN ("));
}

#[test]
fn subset_binds_the_whole_vec() {
    let ids = vec![Uuid::from_u128(1), Uuid::from_u128(2)];
    let f = EnvFilter::Subset(ids.clone());
    assert_eq!(f.bind_uuids(), Some(ids));
    assert_eq!(EnvFilter::All.bind_uuids(), None);
    assert_eq!(EnvFilter::Unattributed.bind_uuids(), None);
    assert_eq!(
        EnvFilter::One(Uuid::from_u128(9)).bind_uuids(),
        Some(vec![Uuid::from_u128(9)])
    );
}

#[test]
fn scope_env_subset_emits_an_any_predicate() {
    let ids = vec![Uuid::from_u128(1), Uuid::from_u128(2)];
    let query = analytics_events::table
        .select(analytics_events::id)
        .into_boxed();
    let scoped = scope_env!(query, analytics_events, &EnvFilter::Subset(ids));
    let sql = debug_query::<Pg, _>(&scoped).to_string();
    assert!(
        sql.contains(r#""analytics_events"."environment_id" = ANY"#),
        "{sql}"
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd backend && cargo test -p sauron-db --lib scope::`
Expected: FAIL — `no variant named 'Subset'`, `no method named 'bind_uuids'`.

- [ ] **Step 3: Implement the variant**

In `backend/crates/sauron-db/src/scope.rs`:

```rust
/// Which environments a read covers.
///
/// No longer `Copy`: `Subset` owns a `Vec`. That is deliberate — every
/// `ReadScope`-taking function had to be revisited when the variant landed,
/// and a silent `Copy` would have let some of them keep the old semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvFilter {
    All,
    One(Uuid),
    /// Exactly the environments the caller holds a grant on. Produced by
    /// `authorize_env` when the caller has environment grants but no app-wide
    /// reach. Never empty — an empty readable set is a 403, not a filter that
    /// matches nothing.
    Subset(Vec<Uuid>),
    Unattributed,
}

impl EnvFilter {
    pub fn sql_fragment(&self, bind_index: usize) -> String {
        match self {
            EnvFilter::All => String::new(),
            EnvFilter::One(_) => format!(" AND environment_id = ${bind_index}"),
            EnvFilter::Subset(_) => format!(" AND environment_id = ANY(${bind_index})"),
            EnvFilter::Unattributed => " AND environment_id IS NULL".to_string(),
        }
    }

    pub fn sql_fragment_for(&self, alias: &str, bind_index: usize) -> String {
        match self {
            EnvFilter::All => String::new(),
            EnvFilter::One(_) => format!(" AND {alias}.environment_id = ${bind_index}"),
            EnvFilter::Subset(_) => {
                format!(" AND {alias}.environment_id = ANY(${bind_index})")
            }
            EnvFilter::Unattributed => format!(" AND {alias}.environment_id IS NULL"),
        }
    }

    /// The values the bind `sql_fragment` reserved, or `None` if it reserved
    /// none. `One` returns a one-element vec so callers have a single shape.
    pub fn bind_uuids(&self) -> Option<Vec<Uuid>> {
        match self {
            EnvFilter::One(id) => Some(vec![*id]),
            EnvFilter::Subset(ids) => Some(ids.clone()),
            EnvFilter::All | EnvFilter::Unattributed => None,
        }
    }

    /// Whether this filter consumed the bind index `sql_fragment` was given.
    pub fn consumes_bind(&self) -> bool {
        matches!(self, EnvFilter::One(_) | EnvFilter::Subset(_))
    }
}
```

Keep `bind_uuid()` as a deprecated alias? **No** — delete it. Every one of its 25
call sites must be visited by Task 4, and leaving the old method lets one be
missed silently.

Add the `bind_env!` macro next to `scope_env!`:

```rust
/// Bind an [`EnvFilter`]'s value onto a boxed raw query, whichever shape it is.
///
/// A macro rather than a function for the same reason `scope_env!` is one: the
/// two `.bind::<T, _>()` calls have different `T`, and diesel's builder type
/// changes with each bind, so a generic helper cannot name the return type.
/// Both arms produce the same `BoxedSqlQuery` type at the call site, so this
/// expands cleanly into an assignment.
#[macro_export]
macro_rules! bind_env {
    ($stmt:expr, $env:expr) => {
        match $env {
            $crate::scope::EnvFilter::All | $crate::scope::EnvFilter::Unattributed => $stmt,
            $crate::scope::EnvFilter::One(id) => {
                $stmt.bind::<diesel::sql_types::Uuid, _>(*id)
            }
            $crate::scope::EnvFilter::Subset(ids) => $stmt
                .bind::<diesel::sql_types::Array<diesel::sql_types::Uuid>, _>(ids.clone()),
        }
    };
}
```

Add the `Subset` arm to `scope_env!`:

```rust
$crate::scope::EnvFilter::Subset(ids) => $q.filter($table::environment_id.eq_any(ids.clone())),
```

Change `scope_env!` and `ReadScope` to take `&EnvFilter` / be `Clone` not `Copy`.

- [ ] **Step 4: Run to verify the new tests pass**

Run: `cd backend && cargo test -p sauron-db --lib scope::`
Expected: PASS. The crate will not yet build as a whole — Task 4 fixes the call sites.

- [ ] **Step 5: Record the compiler's call-site list**

Run: `cd backend && cargo build -p sauron-db 2>&1 | grep -E "^error" | wc -l`

Paste the count and the file:line list into `.superpowers/sdd/s3-task-3-report.md`.
Task 4 works through exactly this list; if it is not ~25 bind sites plus the
`Copy` fallout, stop and say so rather than improvising.

---

### Task 4: Thread `Subset` through all 38 read functions

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (25 `bind_uuid` sites, 20 `scope_env!` sites, 57 `sql_fragment` sites)
- Test: `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Consumes: `EnvFilter::Subset`, `bind_env!`, `bind_uuids`, `consumes_bind` from Task 3.
- Produces: all 38 `ReadScope`-taking functions accepting `Subset` correctly.

- [ ] **Step 1: Write the failing test**

Append to `backend/crates/sauron-db/tests/env_scoping.rs`. This is the test that
matters most in the whole slice — it asserts `Subset` equals the union of its
parts across every read shape:

```rust
/// `Subset([a, b])` must equal `One(a)` ∪ `One(b)` for counts, and must
/// EXCLUDE unattributed rows — `= ANY(array)` never matches NULL. If a
/// function's bind arithmetic is wrong for `Subset`, it either errors at
/// runtime (bind count mismatch) or silently returns `One`'s answer.
#[tokio::test]
async fn subset_equals_the_union_of_its_environments_and_excludes_unattributed() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs(&mut conn).await;
    let since = ids.pinned_now - chrono::Duration::days(30);

    let both = ReadScope::new(ids.app_id, EnvFilter::Subset(vec![ids.env_a, ids.env_b]));
    let only_a = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));
    let only_b = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b));
    let unattributed = ReadScope::new(ids.app_id, EnvFilter::Unattributed);
    let all = ReadScope::new(ids.app_id, EnvFilter::All);

    let t_both = repo::overview_totals(&mut conn, both.clone(), since).await.unwrap();
    let t_a = repo::overview_totals(&mut conn, only_a.clone(), since).await.unwrap();
    let t_b = repo::overview_totals(&mut conn, only_b.clone(), since).await.unwrap();
    let t_un = repo::overview_totals(&mut conn, unattributed, since).await.unwrap();
    let t_all = repo::overview_totals(&mut conn, all, since).await.unwrap();

    assert_eq!(
        t_both.events,
        t_a.events + t_b.events,
        "Subset events must be the exact union of its two environments"
    );
    assert_eq!(t_both.errors, t_a.errors + t_b.errors);
    assert_eq!(t_both.sessions, t_a.sessions + t_b.sessions);

    assert!(
        t_un.events > 0,
        "seed must contain unattributed rows for this test to mean anything"
    );
    assert_eq!(
        t_all.events,
        t_both.events + t_un.events,
        "All = Subset(every env) + Unattributed; Subset must NOT include NULLs"
    );

    // A single-element Subset must agree exactly with One.
    let single = ReadScope::new(ids.app_id, EnvFilter::Subset(vec![ids.env_a]));
    let t_single = repo::overview_totals(&mut conn, single, since).await.unwrap();
    assert_eq!(t_single.events, t_a.events);
    assert_eq!(t_single.errors, t_a.errors);
}

/// Every raw-SQL read function must survive `Subset` without a bind mismatch.
/// A wrong bind index raises `bind message supplies N parameters, but prepared
/// statement requires M` at runtime — invisible to any unit test.
#[tokio::test]
async fn every_scoped_read_accepts_subset_without_a_bind_mismatch() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs(&mut conn).await;
    let since = ids.pinned_now - chrono::Duration::days(30);
    let scope = ReadScope::new(ids.app_id, EnvFilter::Subset(vec![ids.env_a, ids.env_b]));

    repo::list_issues(&mut conn, scope.clone(), &Default::default(), 50, 0)
        .await
        .expect("list_issues under Subset");
    repo::issue_stats(&mut conn, scope.clone()).await.expect("issue_stats");
    repo::top_issues(&mut conn, scope.clone(), since, 10).await.expect("top_issues");
    repo::list_persons(&mut conn, scope.clone(), since, 50, 0).await.expect("list_persons");
    repo::list_devices(&mut conn, scope.clone(), since, 50, 0).await.expect("list_devices");
    repo::list_sessions(&mut conn, scope.clone(), 50, 0).await.expect("list_sessions");
    repo::session_stats(&mut conn, scope.clone(), since).await.expect("session_stats");
    repo::user_stats(&mut conn, scope.clone(), since).await.expect("user_stats");
    repo::active_user_series(&mut conn, scope.clone(), since).await.expect("active_user_series");
    repo::session_duration_series(&mut conn, scope.clone(), since)
        .await
        .expect("session_duration_series");
    repo::session_duration_histogram(&mut conn, scope.clone(), since)
        .await
        .expect("session_duration_histogram");
    repo::journey_graph(&mut conn, scope.clone(), since, 20).await.expect("journey_graph");
}
```

Adjust each call's argument list to the real signature in `repo.rs` — the names
above are correct but several take extra `limit`/`offset`/filter arguments. Do
not guess: read the signature.

- [ ] **Step 2: Run to verify failure**

Run: `cd backend && cargo build -p sauron-db 2>&1 | head -30`
Expected: FAIL — the call sites from Task 3's recorded list.

- [ ] **Step 3: Convert every `bind_uuid` site**

Each site currently reads:

```rust
if let Some(id) = scope.env.bind_uuid() {
    stmt = stmt.bind::<SqlUuid, _>(id);
}
```

Replace with:

```rust
stmt = bind_env!(stmt, &scope.env);
```

And each `next_bind` computation of the form:

```rust
let mut next_bind = if env_bind_value.is_some() { 4usize } else { 3usize };
```

becomes:

```rust
let mut next_bind = if scope.env.consumes_bind() { 4usize } else { 3usize };
```

Work through Task 3's recorded list in file order. Do not batch-`sed` this —
several sites bind in the middle of a chain and the ordering matters.

- [ ] **Step 4: Convert the `scope_env!` and signature sites**

`scope_env!(q, table, scope.env)` becomes `scope_env!(q, table, &scope.env)`.
Functions taking `scope: ReadScope` by value keep doing so (it is `Clone`); fix
the call sites the compiler flags with `.clone()` rather than reshaping
signatures.

- [ ] **Step 5: Run the new tests**

Run: `cd backend && TEST_DATABASE_URL=... cargo test -p sauron-db --test env_scoping subset`
Expected: PASS both tests.

- [ ] **Step 6: Verify the bind arithmetic against the real server**

For each of the 25 raw statements, `PREPARE` the `Subset` form against Postgres
and confirm `pg_prepared_statements.parameter_types` has the arity you expect.
This is how S2 caught its bind bugs and it is the only mechanism that sees them.
Record the count checked in `.superpowers/sdd/s3-task-4-report.md`.

- [ ] **Step 7: Deliberate-break proof**

In one raw statement, change `consumes_bind()` to a hardcoded `false` so the
index does not shift. Run the Subset test, paste the bind-mismatch error,
restore.

- [ ] **Step 8: Run the verification gate**

---

## Phase 2 — Enforcement (Tasks 5–7)

Phase 2 is where the boundary becomes real. The invariant to hold on to: **a
member with app-wide reach must resolve exactly as they do today, with no extra
query.** Every task in this phase is judged against that.

---

### Task 5: `authorize_env_read` — one call that authorizes and scopes

**Files:**
- Modify: `backend/crates/sauron-auth/src/rbac.rs`
- Modify: `backend/crates/sauron-db/src/repo.rs` (add `env_ids_for_app`)
- Test: `backend/crates/sauron-auth/src/rbac.rs` (inline), `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Consumes: `Scope::Env`, `Reach.envs`, `has_permission` (5-arg) from Task 1; `EnvFilter::Subset` from Task 3.
- Produces:
  - `repo::env_ids_for_app(conn, app_id: Uuid) -> QueryResult<Vec<Uuid>>` — every environment of the app **including retired ones**.
  - `rbac::resolve_env_filter(grants: &[Grant], permission: &str, org: Uuid, project: Uuid, app: Uuid, app_env_ids: &[Uuid], requested: EnvFilter) -> Result<EnvFilter, EnvDenied>` — the pure decision function.
  - `pub enum EnvDenied { NoReach, EnvNotInApp, EnvNotGranted, UnattributedNeedsAppReach }`
  - `rbac::authorize_env_read(conn, user_id, app_id, permission, requested: EnvFilter) -> Result<ReadScope, AuthError>` — the DB-backed wrapper.

The decision function is pure and separately unit-tested; that is the whole point
of splitting it out. The wrapper does I/O only.

- [ ] **Step 1: Write the failing decision-table tests**

The wire contract from the spec, as executable rows. Append to `mod tests` in
`backend/crates/sauron-auth/src/rbac.rs`:

```rust
fn app_envs() -> Vec<Uuid> {
    vec![env_a1p(), env_a1s()]
}

fn resolve(
    grants: &[Grant],
    requested: EnvFilter,
) -> Result<EnvFilter, EnvDenied> {
    resolve_env_filter(
        grants,
        perm::ISSUE_READ,
        org(),
        proj_a(),
        app_a1(),
        &app_envs(),
        requested,
    )
}

// --- row 1: app-wide reach resolves exactly as it does today ---------------

#[test]
fn app_wide_reach_passes_every_filter_through_unchanged() {
    let g = vec![preset_grant(Scope::App(app_a1()), &VIEWER)];
    assert_eq!(resolve(&g, EnvFilter::All), Ok(EnvFilter::All));
    assert_eq!(
        resolve(&g, EnvFilter::One(env_a1p())),
        Ok(EnvFilter::One(env_a1p()))
    );
    assert_eq!(
        resolve(&g, EnvFilter::Unattributed),
        Ok(EnvFilter::Unattributed)
    );
}

#[test]
fn org_and_project_reach_are_also_app_wide() {
    for g in [
        vec![preset_grant(Scope::Org(org()), &VIEWER)],
        vec![preset_grant(Scope::Project(proj_a()), &VIEWER)],
    ] {
        assert_eq!(resolve(&g, EnvFilter::All), Ok(EnvFilter::All));
        assert_eq!(
            resolve(&g, EnvFilter::Unattributed),
            Ok(EnvFilter::Unattributed)
        );
    }
}

/// Even with app-wide reach, an environment id that is not this app's is
/// refused — this is the existence + ownership check `parse_env`'s doc comment
/// has been asking for since Slice 2.
#[test]
fn app_wide_reach_still_refuses_a_foreign_environment_id() {
    let g = vec![preset_grant(Scope::App(app_a1()), &VIEWER)];
    let foreign = Uuid::from_u128(9999);
    assert_eq!(
        resolve(&g, EnvFilter::One(foreign)),
        Err(EnvDenied::EnvNotInApp)
    );
}

// --- row 2: partial reach auto-narrows ------------------------------------

#[test]
fn partial_reach_narrows_all_to_the_held_environments() {
    let g = vec![preset_grant(Scope::Env(env_a1p()), &VIEWER)];
    assert_eq!(
        resolve(&g, EnvFilter::All),
        Ok(EnvFilter::Subset(vec![env_a1p()]))
    );
}

#[test]
fn partial_reach_with_two_environments_narrows_to_both() {
    let g = vec![
        preset_grant(Scope::Env(env_a1p()), &VIEWER),
        preset_grant(Scope::Env(env_a1s()), &VIEWER),
    ];
    match resolve(&g, EnvFilter::All) {
        Ok(EnvFilter::Subset(mut ids)) => {
            ids.sort();
            let mut want = vec![env_a1p(), env_a1s()];
            want.sort();
            assert_eq!(ids, want);
        }
        other => panic!("expected Subset of both environments, got {other:?}"),
    }
}

#[test]
fn partial_reach_allows_a_held_environment_and_refuses_a_sibling() {
    let g = vec![preset_grant(Scope::Env(env_a1p()), &VIEWER)];
    assert_eq!(
        resolve(&g, EnvFilter::One(env_a1p())),
        Ok(EnvFilter::One(env_a1p()))
    );
    assert_eq!(
        resolve(&g, EnvFilter::One(env_a1s())),
        Err(EnvDenied::EnvNotGranted)
    );
}

/// Unattributed rows belong to no environment, so they belong to nobody's
/// readable set. An env-scoped caller asking for them is refused, not given an
/// empty list — "matches nothing" is not "you may not ask".
#[test]
fn partial_reach_refuses_unattributed() {
    let g = vec![preset_grant(Scope::Env(env_a1p()), &VIEWER)];
    assert_eq!(
        resolve(&g, EnvFilter::Unattributed),
        Err(EnvDenied::UnattributedNeedsAppReach)
    );
}

/// An env grant on ANOTHER app's environment must not widen this app's
/// readable set. `reach.envs` is intersected with the app's own environments.
#[test]
fn an_env_grant_from_another_app_contributes_nothing() {
    let other_app_env = Uuid::from_u128(7777);
    let g = vec![preset_grant(Scope::Env(other_app_env), &VIEWER)];
    assert_eq!(resolve(&g, EnvFilter::All), Err(EnvDenied::NoReach));
}

// --- row 3: no reach at all ------------------------------------------------

#[test]
fn no_reach_is_denied_for_every_filter() {
    let g = vec![preset_grant(Scope::App(app_a2()), &VIEWER)];
    assert_eq!(resolve(&g, EnvFilter::All), Err(EnvDenied::NoReach));
    assert_eq!(
        resolve(&g, EnvFilter::One(env_a1p())),
        Err(EnvDenied::NoReach)
    );
    assert_eq!(
        resolve(&g, EnvFilter::Unattributed),
        Err(EnvDenied::NoReach)
    );
}

/// Holding the wrong permission is the same as holding nothing. A Viewer's
/// env grant does not confer `issue:write`.
#[test]
fn a_grant_lacking_the_permission_confers_no_reach() {
    let g = vec![preset_grant(Scope::Env(env_a1p()), &VIEWER)];
    let got = resolve_env_filter(
        &g,
        perm::ISSUE_WRITE,
        org(),
        proj_a(),
        app_a1(),
        &app_envs(),
        EnvFilter::All,
    );
    assert_eq!(got, Err(EnvDenied::NoReach));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd backend && cargo test -p sauron-auth resolve_env 2>&1 | head -20`
Expected: FAIL — `cannot find function 'resolve_env_filter'`.

- [ ] **Step 3: Implement the decision function**

`sauron-auth` must depend on `sauron-db`'s `scope` module — it already depends on
`sauron_db` for `repo`/`models`, so import `sauron_db::scope::EnvFilter`.

```rust
/// Why an environment-scoped read was refused. Mapped to HTTP by the caller;
/// kept separate from `AuthError` so the pure decision function stays free of
/// transport concerns and stays unit-testable without a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvDenied {
    /// No grant carrying this permission reaches this app or any of its
    /// environments.
    NoReach,
    /// The requested environment id is not one of this app's environments —
    /// it does not exist, or it belongs to a different app.
    EnvNotInApp,
    /// The environment exists on this app, but the caller holds no grant on it.
    EnvNotGranted,
    /// `?environment_id=none` selects rows attributed to no environment, which
    /// only a caller with app-wide reach may read.
    UnattributedNeedsAppReach,
}

/// Resolve what the caller asked for into what they are allowed to have.
///
/// Pure: no I/O, no clock. `app_env_ids` is every environment of the app,
/// **including retired ones** — a retired environment's history stays readable
/// (Slice 1's invariant), so excluding them here would make `Subset` narrower
/// than the `All` it stands in for.
///
/// The order of the checks is load-bearing. Ownership (`EnvNotInApp`) is
/// tested before grant-holding (`EnvNotGranted`) so that a caller probing for
/// which environment ids exist learns nothing they could not learn from
/// `list_environments` — both refusals are a 403 at the HTTP layer.
pub fn resolve_env_filter(
    grants: &[Grant],
    permission: &str,
    org: Uuid,
    project: Uuid,
    app: Uuid,
    app_env_ids: &[Uuid],
    requested: EnvFilter,
) -> Result<EnvFilter, EnvDenied> {
    let app_wide = has_permission(grants, permission, org, Some(project), Some(app), None);

    if app_wide {
        return match requested {
            EnvFilter::All => Ok(EnvFilter::All),
            EnvFilter::Unattributed => Ok(EnvFilter::Unattributed),
            EnvFilter::One(id) => {
                if app_env_ids.contains(&id) {
                    Ok(EnvFilter::One(id))
                } else {
                    Err(EnvDenied::EnvNotInApp)
                }
            }
            // A caller cannot ask for a Subset over the wire; it is only ever
            // produced here. Treat it as All rather than trusting the input.
            EnvFilter::Subset(_) => Ok(EnvFilter::All),
        };
    }

    let reach = reach_for(grants, permission);
    let mut readable: Vec<Uuid> = app_env_ids
        .iter()
        .copied()
        .filter(|e| reach.envs.contains(e))
        .collect();
    readable.sort();
    readable.dedup();

    if readable.is_empty() {
        return Err(EnvDenied::NoReach);
    }

    match requested {
        EnvFilter::All | EnvFilter::Subset(_) => Ok(EnvFilter::Subset(readable)),
        EnvFilter::Unattributed => Err(EnvDenied::UnattributedNeedsAppReach),
        EnvFilter::One(id) => {
            if !app_env_ids.contains(&id) {
                Err(EnvDenied::EnvNotInApp)
            } else if readable.contains(&id) {
                Ok(EnvFilter::One(id))
            } else {
                Err(EnvDenied::EnvNotGranted)
            }
        }
    }
}
```

- [ ] **Step 4: Add `env_ids_for_app` to the repo**

In `backend/crates/sauron-db/src/repo.rs`, beside `list_environments`:

```rust
/// Every environment id of an app, **including retired ones**.
///
/// Retired environments are included deliberately: their history stays
/// readable (an app's data does not disappear because an environment was
/// retired), so a caller's readable subset must be able to contain one.
/// `list_environments` excludes them because they must not be *selectable*;
/// that is a different question from whether they are *readable*.
pub async fn env_ids_for_app(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    environments::table
        .filter(environments::app_id.eq(app_id))
        .select(environments::id)
        .load(conn)
        .await
}
```

- [ ] **Step 5: Implement the DB-backed wrapper**

```rust
/// Authorize an environment-scoped **read** and produce its `ReadScope`.
///
/// Replaces the `authorize_app(...)` + `read_scope_raw(...)` pair. They are one
/// call because they were two decisions that had to agree, and four separate
/// defects in this feature came from two things that had to agree by hand.
///
/// Cost: identical to `authorize_app` for the overwhelmingly common case —
/// a caller with app-wide reach asking for every environment never triggers the
/// `env_ids_for_app` lookup.
pub async fn authorize_env_read(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
    requested: EnvFilter,
) -> Result<ReadScope, AuthError> {
    let (project_id, org_id) = repo::app_ancestry(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::NotFound)?;

    let rows = repo::user_grants_in_org(conn, user_id, org_id)
        .await
        .map_err(|_| AuthError::Internal)?;
    let grants = grants_from_rows(rows);

    // Fast path: app-wide reach over every environment needs no environment
    // lookup at all, so today's callers pay exactly today's cost.
    if matches!(requested, EnvFilter::All)
        && has_permission(
            &grants,
            permission,
            org_id,
            Some(project_id),
            Some(app_id),
            None,
        )
    {
        return Ok(ReadScope::new(app_id, EnvFilter::All));
    }

    let app_env_ids = repo::env_ids_for_app(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?;

    let resolved = resolve_env_filter(
        &grants,
        permission,
        org_id,
        project_id,
        app_id,
        &app_env_ids,
        requested,
    )
    .map_err(|_| AuthError::Forbidden)?;

    Ok(ReadScope::new(app_id, resolved))
}
```

Every `EnvDenied` maps to `Forbidden`. That is deliberate: distinguishing
"does not exist" from "not granted" over HTTP would let a caller enumerate
environment ids.

- [ ] **Step 6: Run the tests**

Run: `cd backend && cargo test -p sauron-auth`
Expected: PASS — all twelve new tests.

- [ ] **Step 7: Deliberate-break proof**

Change the `readable.is_empty()` guard to return `Ok(EnvFilter::All)` instead of
`Err(NoReach)` — the classic fail-open. Run
`cargo test -p sauron-auth no_reach_is_denied_for_every_filter` and
`an_env_grant_from_another_app_contributes_nothing`, paste both failures into
`.superpowers/sdd/s3-task-5-report.md`, restore.

- [ ] **Step 8: Run the verification gate**

---

### Task 6: Thread `authorize_env_read` through all 23 scoped handlers

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/scope.rs`
- Modify: `backend/bins/sauron-api/src/routes/{analytics,issues,sessions,devices,screens,performance,journeys,funnels,apps}.rs`
- Test: `backend/bins/sauron-api/tests/http_env_scoping.rs`

**Interfaces:**
- Consumes: `authorize_env_read` from Task 5; `parse_env`/`raw_environment_id` from S2.
- Produces: `scope::authorized_read_scope(conn, user_id, app_id, permission, raw_query) -> Result<ReadScope, ApiError>` — the single call every scoped handler makes.

The 23 call sites are enumerated in `.superpowers/sdd/s3-task-6-brief.md`; they
are every `read_scope`/`read_scope_raw` site listed in the Slice 3 ground map.

- [ ] **Step 1: Write the failing HTTP contract test**

Append to `backend/bins/sauron-api/tests/http_env_scoping.rs`:

```rust
/// The wire contract's 403 rows, driven through the real router. A unit test of
/// the decision function cannot see these — the S2 review's F7 finding was that
/// zero rejecting routes were exercised over HTTP, and the original Critical in
/// this feature lived entirely in which extractor a handler imported.
#[tokio::test]
async fn env_scoped_member_is_confined_over_http() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let f = h.seed_env_scoped_member().await;

    // Their own environment: 200.
    let r = h
        .get_as(&f.member_token, &format!("/v1/apps/{}/issues?environment_id={}", f.app_id, f.granted_env))
        .await;
    assert_eq!(r.status(), 200, "member must read the environment they hold");

    // A sibling environment in the same app: 403, not 200-with-zero-rows.
    let r = h
        .get_as(&f.member_token, &format!("/v1/apps/{}/issues?environment_id={}", f.app_id, f.other_env))
        .await;
    assert_eq!(r.status(), 403, "sibling environment must be refused, not empty");

    // Unattributed: 403.
    let r = h
        .get_as(&f.member_token, &format!("/v1/apps/{}/issues?environment_id=none", f.app_id))
        .await;
    assert_eq!(r.status(), 403, "unattributed needs app-wide reach");

    // Absent: 200, auto-narrowed. Must return strictly fewer rows than the
    // owner sees, or the narrowing did not happen.
    let member_all = h
        .get_json_as(&f.member_token, &format!("/v1/apps/{}/issues", f.app_id))
        .await;
    let owner_all = h
        .get_json_as(&f.owner_token, &format!("/v1/apps/{}/issues", f.app_id))
        .await;
    let m = member_all.as_array().unwrap().len();
    let o = owner_all.as_array().unwrap().len();
    assert!(
        m < o,
        "absent environment_id must auto-narrow for a partial-reach member: \
         member saw {m}, owner saw {o}"
    );
    assert!(m > 0, "auto-narrowing must not narrow to nothing");
}

/// An owner's behaviour must be byte-identical to before this slice.
#[tokio::test]
async fn app_wide_member_is_unaffected() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let f = h.seed_env_scoped_member().await;

    for path in [
        format!("/v1/apps/{}/issues", f.app_id),
        format!("/v1/apps/{}/issues?environment_id={}", f.app_id, f.granted_env),
        format!("/v1/apps/{}/issues?environment_id={}", f.app_id, f.other_env),
        format!("/v1/apps/{}/issues?environment_id=none", f.app_id),
    ] {
        let r = h.get_as(&f.owner_token, &path).await;
        assert_eq!(r.status(), 200, "owner must still reach {path}");
    }

    // A well-formed but foreign environment id is now refused rather than
    // silently returning an empty list — the check `parse_env`'s doc comment
    // has been asking for since Slice 2.
    let foreign = uuid::Uuid::new_v4();
    let r = h
        .get_as(&f.owner_token, &format!("/v1/apps/{}/issues?environment_id={foreign}", f.app_id))
        .await;
    assert_eq!(r.status(), 403, "foreign environment id must not be a silent empty list");
}
```

`Harness::seed_env_scoped_member` is new: it creates an org with an owner, an app
with two environments, error events in **both** environments, a second user, and
one `role_grants` row with `scope_type='env'` on the first environment only.
Write it in this task alongside the existing harness helpers.

- [ ] **Step 2: Run to verify failure**

Run: `cd backend && TEST_DATABASE_URL=... TEST_REDIS_URL=... cargo test -p sauron-api --test http_env_scoping env_scoped_member`
Expected: FAIL — sibling environment returns 200 with an empty array, not 403.

- [ ] **Step 3: Add the combined helper to `scope.rs`**

```rust
/// Authorize an environment-scoped read and produce its `ReadScope` in one
/// call, sourcing `environment_id` from the raw query string.
///
/// This supersedes calling `authorize_app` and `read_scope_raw` separately.
/// Both orderings of that pair were correct, but only if both were present —
/// and the whole history of this feature is defects where two things that had
/// to agree were maintained by hand. One call cannot half-happen.
pub async fn authorized_read_scope(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
    raw_query: Option<&str>,
) -> Result<ReadScope, ApiError> {
    let requested = parse_env(raw_environment_id(raw_query).as_deref())?;
    let scope = rbac::authorize_env_read(conn, user_id, app_id, permission, requested).await?;
    Ok(scope)
}
```

Update the module doc's contract table to add the reach dimension, and replace
`parse_env`'s "Slice 3 will need an existence + app-ownership check" paragraph
with a statement that it now happens in `resolve_env_filter`, naming it.

- [ ] **Step 4: Convert all 23 handlers**

Each currently reads:

```rust
authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
let scope = read_scope_raw(app_id, raw_query.as_deref())?;
```

becomes:

```rust
let scope = authorized_read_scope(
    &mut conn,
    auth.user_id,
    app_id,
    perm::EVENT_READ,
    raw_query.as_deref(),
)
.await?;
```

Handlers using `read_scope` (with a `Query<T>` field) rather than
`read_scope_raw` **must also switch to `axum::extract::RawQuery`** — they are
`sessions.rs`, `devices.rs`, `screens.rs`, `performance.rs`, `journeys.rs`,
`funnels.rs::compute`, `apps.rs::first_event`. Add the extractor to the handler
signature; leave their existing `Query<T>` for their other parameters.

`issues.rs`'s `detail` and `events` use `authorize_app_perms` (which returns the
whole permission set for a downstream `source:read` check). Keep that call, and
add `authorized_read_scope` alongside it — do **not** try to merge them in this
task.

`apps.rs::first_event` authorizes on `APP_READ`, not `EVENT_READ`. Preserve
that; it is the one scoped read gated on an app permission and changing it is
out of scope.

- [ ] **Step 5: Delete the now-unreachable helpers**

`read_scope` and `read_scope_raw` have no callers left. Delete them and their
tests. Leaving them is how a future handler reintroduces the unauthorized path.

- [ ] **Step 6: Run the tests**

Run: `cd backend && TEST_DATABASE_URL=... TEST_REDIS_URL=... cargo test -p sauron-api`
Expected: PASS.

- [ ] **Step 7: Deliberate-break proof**

Revert one handler (`analytics.rs::overview`) to the old
`authorize_app` + inline `parse_env` pair, run the HTTP test, paste the failure,
restore.

- [ ] **Step 8: Run the verification gate**

---

### Task 7: Discovery — `list_environments` by reach, and the grant surface

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/environments.rs:86-97`
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs` (lines 62, 109-113, 231, 257-315, 362-388, 611, 771, 800, 820)
- Test: `backend/bins/sauron-api/tests/http_env_scoping.rs`

**Interfaces:**
- Consumes: `Reach.envs`, `Scope::parts()` from Task 1.
- Produces: `list_environments` returning only the caller's readable environments; `orgs.rs` accepting and validating `scope_type = 'env'`.

- [ ] **Step 1: Write the failing tests**

```rust
/// An env-scoped member must see only the environments they hold — otherwise
/// the topbar offers a picker entry that 403s the moment it is chosen.
#[tokio::test]
async fn list_environments_is_filtered_by_reach() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let f = h.seed_env_scoped_member().await;

    let owner = h
        .get_json_as(&f.owner_token, &format!("/v1/apps/{}/environments", f.app_id))
        .await;
    assert_eq!(owner.as_array().unwrap().len(), 2, "owner sees both");

    let member = h
        .get_json_as(&f.member_token, &format!("/v1/apps/{}/environments", f.app_id))
        .await;
    let envs = member.as_array().unwrap();
    assert_eq!(envs.len(), 1, "env-scoped member sees only their own");
    assert_eq!(envs[0]["id"].as_str().unwrap(), f.granted_env.to_string());
}

/// The grant API must accept the new scope type end to end.
#[tokio::test]
async fn an_env_scoped_grant_can_be_created_over_http() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let f = h.seed_env_scoped_member().await;

    let r = h
        .post_as(
            &f.owner_token,
            &format!("/v1/orgs/{}/grants", f.org_id),
            serde_json::json!({
                "email": f.member_email,
                "role_id": f.viewer_role_id,
                "scopes": [{ "scope_type": "env", "scope_id": f.other_env }],
            }),
        )
        .await;
    assert_eq!(r.status(), 200, "env scope_type must be accepted");

    // And it must be reflected in /access, so the dashboard can render it.
    let access = h
        .get_json_as(&f.member_token, &format!("/v1/orgs/{}/access", f.org_id))
        .await;
    let has_env_grant = access["grants"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["scope_type"] == "env");
    assert!(has_env_grant, "/access must surface env grants: {access}");
}

/// A cross-tenant environment id must be refused, exactly as an app id is.
/// `scope_id` has no FK, so this check is the only thing enforcing it.
#[tokio::test]
async fn an_env_scope_from_another_org_is_refused() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let f = h.seed_env_scoped_member().await;
    let other = h.seed_second_org().await;

    let r = h
        .post_as(
            &f.owner_token,
            &format!("/v1/orgs/{}/grants", f.org_id),
            serde_json::json!({
                "email": f.member_email,
                "role_id": f.viewer_role_id,
                "scopes": [{ "scope_type": "env", "scope_id": other.env_id }],
            }),
        )
        .await;
    assert_eq!(r.status(), 400, "an environment outside the org must be refused");
}
```

- [ ] **Step 2: Run to verify failure**

Expected: FAIL — `list_environments` returns 2 for the member; the grant POST
returns 400 `invalid scope_type`.

- [ ] **Step 3: Rewrite `list_environments` on the reach pattern**

Mirror `projects.rs::list_apps:138-170` exactly:

```rust
pub async fn list_environments(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<Vec<Environment>>, ApiError> {
    let mut conn = db(&state).await?;
    // Same shape as `list_apps`, one level down: `authorize_app` gates on a
    // fixed (org, project, app, None) target that an env-scoped grant can never
    // satisfy, so an env-scoped member would 403 from the very endpoint that
    // populates their environment picker.
    let (project_id, org_id) = repo::app_ancestry(&mut conn, app_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let rows = repo::user_grants_in_org(&mut conn, auth.user_id, org_id).await?;
    if rows.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }
    let grants = grants_from_rows(rows);
    let reach = reach_for(&grants, perm::ENV_READ);
    let all = repo::list_environments(&mut conn, app_id, false).await?;
    if reach.org || reach.projects.contains(&project_id) || reach.apps.contains(&app_id) {
        return Ok(Json(all));
    }

    let allowed: HashSet<Uuid> = reach.envs.into_iter().collect();
    let mine: Vec<Environment> = all.into_iter().filter(|e| allowed.contains(&e.id)).collect();
    if mine.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }
    Ok(Json(mine))
}
```

- [ ] **Step 4: Add the `env` arm everywhere `orgs.rs` matches a scope type**

Six places, all of which currently spell the three literals by hand:

1. `normalize_scopes` (`orgs.rs:231`) — `"org" | "project" | "app" | "env"`.
2. `validate_scopes_in_org` (`orgs.rs:257-315`) — a new arm resolving env ids via
   a batched `repo::env_ancestries(&env_ids)` (write it beside `app_ancestries`,
   same shape, returning `(env_id, app_id, project_id, org_id)`), filtered to
   `owner_org == org_id`. Keep the two-query budget: one batch call for apps, one
   for envs.
3. `validate_scope_in_org` (`orgs.rs:362-388`) — the single-scope counterpart.
4. `update_grant_handler` (`orgs.rs:771-773`) — the same literal list.
5. `access` (`orgs.rs:109-113`) — **replace the hand-rolled match with
   `grant.scope.parts()`**, so this cannot drift again.
6. `delete_grant` (`orgs.rs:611`) — resolve `project_of_app`/`app_of_env` for an
   env grant so the escalation check evaluates at the right level.

`ResolvedScope` gains an `app_of_env: Option<Uuid>` field alongside
`project_of_app`, and `target()` returns the 4-tuple Task 1 introduced.

- [ ] **Step 5: Run the tests**

Run: `cd backend && TEST_DATABASE_URL=... TEST_REDIS_URL=... cargo test -p sauron-api`
Expected: PASS.

- [ ] **Step 6: Deliberate-break proof**

Remove `"env"` from `normalize_scopes`' `matches!`, run
`an_env_scoped_grant_can_be_created_over_http`, paste the 400, restore. Then
remove the `owner_org == org_id` filter from the env arm of
`validate_scopes_in_org`, run `an_env_scope_from_another_org_is_refused`, paste
the failure, restore. The second is the cross-tenant boundary and matters more.

- [ ] **Step 7: Run the verification gate**

---

## Phase 3 — Data integrity (Tasks 8–10)

Two defects that were merely confusing under Slice 2 become access defects once
Phase 2 lands: a member confined to staging reads strings and crash counts
produced by production.

---

### Task 8: `error_events` carries the issue strings

**Files:**
- Create: `backend/migrations/2026-07-29-000030_error_event_title_culprit/up.sql`
- Create: `backend/migrations/2026-07-29-000030_error_event_title_culprit/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs` (**by hand**, `error_events` block only)
- Modify: `backend/crates/sauron-db/src/models.rs` (`NewErrorEvent`)
- Modify: `backend/crates/sauron-pipeline/src/process.rs:138-139, ~209`
- Test: `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Produces: `error_events.title` / `error_events.culprit`, both `Nullable<Text>`; `NewErrorEvent { title: Option<&'a str>, culprit: Option<&'a str>, .. }`.

- [ ] **Step 1: Write the migration**

`up.sql`:

```sql
-- `issues.title`/`culprit` are one row per (app_id, fingerprint) and are
-- overwritten by whichever environment sent the most recent occurrence, so a
-- caller scoped to staging reads strings written by production — beside a
-- correctly staging-scoped `last_seen`. Storing the per-occurrence strings
-- lets an environment-scoped read pull the newest occurrence *in that
-- environment*.
--
-- These are computed at ingest already (sauron-pipeline `build_title` /
-- `build_culprit`, called immediately before `upsert_issue`), so persisting
-- them costs no extra computation. The alternative — recomputing them in SQL —
-- works for `title` but not for `culprit`, whose value comes from selecting a
-- stack frame out of the JSONB stacktrace; that would be a second
-- implementation of `build_culprit` that drifts the first time the Rust
-- changes.
--
-- NULLABLE ON PURPOSE, and NOT backfilled. Rows written before this migration
-- keep NULL, and the read path COALESCEs to the app-wide `issues` column, so
-- old data degrades to exactly today's behaviour instead of to empty strings.
-- A backfill would rewrite every partition of the largest table in the system
-- to produce values it can already fall back to.
--
-- ADD COLUMN with no DEFAULT is catalog-only on a partitioned parent: no
-- rewrite, no long lock.
ALTER TABLE error_events ADD COLUMN title TEXT;
ALTER TABLE error_events ADD COLUMN culprit TEXT;
```

`down.sql`:

```sql
ALTER TABLE error_events DROP COLUMN culprit;
ALTER TABLE error_events DROP COLUMN title;
```

- [ ] **Step 2: Hand-edit `schema.rs`**

Add exactly two lines to the `error_events` block, after `handled`:

```rust
        title -> Nullable<Text>,
        culprit -> Nullable<Text>,
```

**Do not run `diesel print-schema` or any other `diesel` command.** Verify
immediately: `grep -c '^diesel::table!' backend/crates/sauron-db/src/schema.rs`
must print **27**.

- [ ] **Step 3: Write the failing test**

```rust
/// Ingest must persist the same title/culprit it hands to `upsert_issue`. If
/// these are NULL, the per-environment derivation in Task 9 silently falls back
/// to the app-wide string for every row and the whole fix is inert.
#[tokio::test]
async fn ingested_error_events_carry_their_own_title_and_culprit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs(&mut conn).await;

    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        title: Option<String>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        culprit: Option<String>,
    }

    let rows: Vec<Row> = diesel::sql_query(
        "SELECT title, culprit FROM error_events WHERE app_id = $1 AND title IS NOT NULL",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .load(&mut conn)
    .await
    .unwrap();

    assert!(
        !rows.is_empty(),
        "the seed must write per-occurrence titles, or Task 9 cannot be tested"
    );
}
```

- [ ] **Step 4: Run to verify failure**

Expected: FAIL — `column "title" does not exist` before the migration, then
`rows.is_empty()` after it but before the seed writes the columns.

- [ ] **Step 5: Widen `NewErrorEvent` and the pipeline**

In `models.rs`, add to `NewErrorEvent<'a>`:

```rust
    pub title: Option<&'a str>,
    pub culprit: Option<&'a str>,
```

In `process.rs`, `title` and `culprit` are already bound at lines 138-139,
immediately before `upsert_issue`. They are still in scope at the
`insert_error_event` call; pass them:

```rust
            title: Some(title.as_str()),
            culprit: Some(culprit.as_str()),
```

Note `build_culprit` returns an empty string when there is no exception. Store
it as-is — `Some("")` is a real, meaningful "this occurrence had no culprit",
distinct from `None` meaning "written before migration 30".

- [ ] **Step 6: Update the test seed**

`seed_two_envs` inserts `error_events` rows directly. Give them explicit
`title`/`culprit` values, and — critically for Task 9 — make the **same issue
carry different titles in the two environments**, with a known ordering:

```
issue_shared, env_a, occurred_at = pinned_now - 240s,
    title = 'TypeError: staging cart is empty'
    culprit = 'checkout (staging/cart.ts)'
issue_shared, env_b, occurred_at = pinned_now -  30s,   <- newer, so it is what
    title = 'TypeError: prod cart is empty'                 upsert_issue leaves
    culprit = 'checkout (prod/cart.ts)'                     on the issues row
```

Expose the issue's id on `SeedIds` as `issue_shared: Uuid` — Task 9's tests
address it by name, and it is not currently surfaced.

Record in `SeedIds`' doc comment that `issues.title` for `issue_shared` is
therefore the **env_b** string, since env_b's occurrence is the newer one. That
is the fact the Task 9 assertions turn on.

- [ ] **Step 7: Run the tests**

Run: `cd backend && TEST_DATABASE_URL=... cargo test -p sauron-db --test env_scoping`
Expected: PASS, and every pre-existing test still passing. If a row-count
assertion moved, name it in the report — a seed change that silently invalidates
an assertion is how S2 lost time twice.

- [ ] **Step 8: Run the verification gate**

Confirm `grep -c '^diesel::table!' crates/sauron-db/src/schema.rs` prints 27.

---

### Task 9: Derive `title`, `culprit` and `level` per environment

**Files:**
- Create: `backend/migrations/2026-07-29-000031_issue_env_latest_index/up.sql`
- Create: `backend/migrations/2026-07-29-000031_issue_env_latest_index/down.sql`
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_issues` (~1404-1717), `get_issue` (~1729-1771), `top_issues` (~3288-3340), `issue_stats` (~3374-3402)
- Test: `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Consumes: `error_events.title`/`culprit` from Task 8.
- Produces: `IssueRow` whose `title`/`culprit`/`level` are per-environment under `One`/`Subset`/`Unattributed`.

- [ ] **Step 1: Write the failing test**

Mirror `get_event_user_seen_is_derived_per_environment_not_app_wide`'s structure:

```rust
/// `upsert_issue`'s ON CONFLICT (app_id, fingerprint) has no environment in the
/// key, so `title`/`culprit`/`level` are whatever the most recent occurrence in
/// ANY environment wrote. A caller scoped to staging must see staging's own
/// strings — not production's, sitting beside a correctly staging-scoped
/// `last_seen` that says the issue has been quiet in staging.
#[tokio::test]
async fn issue_title_culprit_and_level_are_derived_per_environment() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs(&mut conn).await;

    let all = repo::get_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::All),
        ids.issue_shared,
    )
    .await
    .unwrap()
    .unwrap();
    let a = repo::get_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        ids.issue_shared,
    )
    .await
    .unwrap()
    .unwrap();
    let b = repo::get_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        ids.issue_shared,
    )
    .await
    .unwrap()
    .unwrap();

    // The two environments must disagree — under the old code all three of
    // these were byte-identical.
    assert_ne!(a.title, b.title, "each environment must show its own title");
    assert_ne!(a.culprit, b.culprit, "each environment must show its own culprit");

    assert_eq!(a.title, "TypeError: staging cart is empty");
    assert_eq!(a.culprit, "checkout (staging/cart.ts)");
    assert_eq!(b.title, "TypeError: prod cart is empty");
    assert_eq!(b.culprit, "checkout (prod/cart.ts)");

    // `All` keeps the durable column — the fast-path convention every fix in
    // this series follows. env_b's occurrence is the newer one, so the stored
    // row carries env_b's string.
    assert_eq!(all.title, b.title, "All must read the stored issues column");

    // And the staging-scoped title sits beside a staging-scoped last_seen.
    assert_eq!(a.last_seen, ids.pinned_now - chrono::Duration::seconds(240));
}

/// `issue_stats` counts `FILTER (WHERE level = ...)` — under the old code an
/// environment's fatal/error/warning split reflected whichever environment sent
/// the last event, so the numbers on the Issues page header disagreed with the
/// list beneath them.
#[tokio::test]
async fn issue_stats_level_breakdown_is_per_environment() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs(&mut conn).await;

    let a = repo::issue_stats(&mut conn, ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)))
        .await
        .unwrap();
    let b = repo::issue_stats(&mut conn, ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)))
        .await
        .unwrap();

    assert_ne!(
        (a.fatal, a.error, a.warning),
        (b.fatal, b.error, b.warning),
        "the level breakdown must differ between environments"
    );
    // Each environment's breakdown must sum to the issues it can actually see.
    assert_eq!(a.fatal + a.error + a.warning + a.info, a.total);
    assert_eq!(b.fatal + b.error + b.warning + b.info, b.total);
}

/// `list_issues` filtered on the STORED columns inside its paging subquery
/// while returning DERIVED values, so `?level=error` and the level shown on the
/// row could disagree. Filter and display must now agree.
#[tokio::test]
async fn list_issues_filters_agree_with_what_it_displays() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs(&mut conn).await;

    let scope = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));
    let filters = IssueFilters {
        level: Some("error".into()),
        ..Default::default()
    };
    let rows = repo::list_issues(&mut conn, scope, &filters, 100, 0).await.unwrap();

    assert!(!rows.is_empty(), "the seed must produce a matching row");
    for r in &rows {
        assert_eq!(
            r.level, "error",
            "every returned row must actually carry the level that was filtered on"
        );
    }
}
```

Read the real `IssueFilters` field names before writing this — do not guess.

- [ ] **Step 2: Run to verify failure**

Expected: FAIL — `assert_ne!(a.title, b.title)` with both sides showing the
production string.

- [ ] **Step 3: Add the index the derivation needs**

Migration 31 `up.sql`:

```sql
-- Migration 28 created error_events (issue_id, environment_id) INCLUDE
-- (distinct_id, occurred_at) for the aggregate LATERAL. The per-environment
-- title/culprit/level derivation needs the NEWEST row in an environment —
-- ORDER BY occurred_at DESC LIMIT 1 — and occurred_at as an INCLUDE column
-- cannot serve an ordering, so that plan would sort every matching row for a
-- hot issue.
--
-- Promoting occurred_at to a key column makes it a backward index scan
-- stopping at the first row, and also serves migration 28's min()/max()
-- aggregate. distinct_id stays in INCLUDE for the count(DISTINCT ...).
DROP INDEX IF EXISTS error_events_issue_env_covering_idx;
CREATE INDEX error_events_issue_env_time_idx
    ON error_events (issue_id, environment_id, occurred_at DESC)
    INCLUDE (distinct_id);
```

`down.sql` restores migration 28's index exactly. Confirm the real index name
from migration 28 before writing this — do not assume the name above.

- [ ] **Step 4: Add the latest-occurrence LATERAL to all four functions**

For `list_issues`, `get_issue` and `top_issues`, add a second LATERAL beside the
existing `agg` one, and change the three columns in the select list:

```sql
LEFT JOIN LATERAL (
    SELECT e.title, e.culprit, e.level
    FROM error_events e
    WHERE e.issue_id = i.id{env_sql_alias_e}
    ORDER BY e.occurred_at DESC
    LIMIT 1
) latest ON TRUE
```

and

```sql
COALESCE(latest.title, i.title)     AS title,
COALESCE(latest.culprit, i.culprit) AS culprit,
COALESCE(latest.level, i.level)     AS level,
```

`LEFT JOIN` + `COALESCE`, not an inner join: a row written before migration 30
has `NULL` title, and must fall back to the app-wide value rather than
disappearing from the page.

The `env_sql` fragment for this LATERAL reuses the **same bind index** as the
existing `agg` LATERAL's — one bound value referenced in several places, the
idiom `list_issues` already uses for its tag/`q` `EXISTS` legs. Do not allocate
a new bind.

For `issue_stats`, the `FILTER (WHERE level = ...)` clauses move onto a derived
level. Under `All` the function is unchanged; under a scoped filter it becomes:

```sql
SELECT count(*)::bigint AS total,
       count(*) FILTER (WHERE i.status = 'unresolved')::bigint AS unresolved,
       ...
       count(*) FILTER (WHERE lvl.level = 'fatal')::bigint AS fatal,
       ...
FROM issues i
JOIN LATERAL (
    SELECT e.level
    FROM error_events e
    WHERE e.issue_id = i.id{env_sql}
    ORDER BY e.occurred_at DESC
    LIMIT 1
) lvl ON TRUE
WHERE i.app_id = $1
```

The inner join here is correct and replaces the membership `EXISTS`: an issue
with no occurrence in this environment has no row to derive from and must not be
counted — which is exactly what the `EXISTS` was doing.

`status` stays app-wide in both — issue triage is an app-wide act by design.

- [ ] **Step 5: Move `list_issues`' filters onto the derived values**

The `level`, `culprit`, `times_seen` and `users_seen` filter fragments currently
sit inside the paging subquery against `issues`' stored columns. They move to the
outer query against `latest.level` / `latest.culprit` / `agg.times_seen` /
`agg.users_seen`.

This changes paging semantics: the subquery can no longer pre-filter on those
four, so it selects a wider candidate set. `Issues.svelte` sends `limit: 100`
with no offset, so this is acceptable — but **say so in a comment**, because it
is a real trade and the next reader deserves to know it was chosen rather than
overlooked. `status` and `type` filters stay in the subquery; they are genuinely
app-wide columns.

- [ ] **Step 6: Run the tests**

Run: `cd backend && TEST_DATABASE_URL=... cargo test -p sauron-db --test env_scoping issue`
Expected: PASS.

- [ ] **Step 7: Measure**

`EXPLAIN (ANALYZE, BUFFERS)` `list_issues` under `One`, before and after, against
the 210k-row dev app. Record both plans in
`.superpowers/sdd/s3-task-9-report.md`. Confirm the new index produces a
backward index scan with `LIMIT 1`, not a sort. If the derivation costs more
than roughly the covering index bought in S2 (~1485ms → ~230ms), stop and report
rather than shipping it.

- [ ] **Step 8: Deliberate-break proof**

Revert the select list to `i.title, i.culprit, i.level`, run
`issue_title_culprit_and_level_are_derived_per_environment`, paste the failure
showing both environments returning the production string, restore.

- [ ] **Step 9: Run the verification gate**

---

### Task 10: Derive `crashed` per environment — measurement-gated

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `overview_totals` (~3203-3255), `session_stats` (~4072-4098), `bump_session` doc comment (~2277)
- Test: `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `crashed_sessions` / `crashed` counting only sessions with an error **in the selected environment**.

**This task may legitimately end in "measured, did not pay, documented instead."**
That outcome is a success, not a failure, and the report must say which happened.

- [ ] **Step 1: Write the failing test**

```rust
/// `bump_session` folds every signal into one row per (app_id, session_id) and
/// sets environment_id = COALESCE(EXCLUDED..., sessions...) — last non-null
/// wins. So a session labelled env_a can carry an errors_count incremented by
/// an env_b error, and `crashed` counts it under BOTH environments.
#[tokio::test]
async fn crashed_sessions_are_counted_only_in_the_environment_that_crashed() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_cross_env_session(&mut conn).await;
    let since = ids.pinned_now - chrono::Duration::days(30);

    let a = repo::session_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        since,
    )
    .await
    .unwrap();
    let b = repo::session_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        since,
    )
    .await
    .unwrap();

    // The shared session errored ONLY in env_b.
    assert_eq!(b.crashed, 1, "env_b saw the error and must count the crash");
    assert_eq!(
        a.crashed, 0,
        "env_a never saw an error on this session and must not count it as crashed"
    );

    // Same for the overview card, which reads a different query.
    let ov_a = repo::overview_totals(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        since,
    )
    .await
    .unwrap();
    assert_eq!(ov_a.crashed_sessions, 0);
}
```

`seed_cross_env_session` is a **new, separate** seed helper: one session id whose
`environment_id` ends up `env_a` (so `sessions.environment_id` points there) but
whose only `error_events` row carries `env_b`. Keep it out of `seed_two_envs` —
that fixture is depended on by 37 tests and this is a deliberately pathological
shape.

It returns its own struct, not `SeedIds`:

```rust
pub struct CrossEnvSessionIds {
    pub app_id: Uuid,
    pub env_a: Uuid,
    pub env_b: Uuid,
    pub session_id: String,
    pub pinned_now: DateTime<Utc>,
}
```

- [ ] **Step 2: Run to verify failure**

Expected: FAIL — `a.crashed` is 1, because `errors_count > 0` on a row labelled
env_a.

- [ ] **Step 3: Measure before implementing**

`EXPLAIN (ANALYZE, BUFFERS)` both the current predicate and the proposed one
against the dev app:

```sql
-- current
count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2 AND errors_count>0 AND environment_id=$3

-- proposed
count(*) FROM sessions s WHERE s.app_id=$1 AND s.last_event_at>=$2 AND EXISTS (
    SELECT 1 FROM error_events e
    WHERE e.app_id = s.app_id AND e.session_id = s.session_id AND e.environment_id = $3
)
```

Record both plans in `.superpowers/sdd/s3-task-10-report.md` **before** writing
any code. `error_events` has no index on `(app_id, session_id)`; if the plan is a
sequential scan across partitions, add
`error_events (app_id, session_id, environment_id)` as migration 32 and measure
again.

**Decision rule:** if the semi-join with its index costs more than roughly 2×
the column predicate at dev-app scale, stop. Revert to the current predicate,
write the cross-environment behaviour into `bump_session`'s doc comment (which
today does not mention it at all — only `get_session`'s comment implies it) and
into both read sites, mark the test `#[ignore]` with a comment pointing at this
report, and say plainly in the report that the fix was measured and declined.

- [ ] **Step 4: Implement, if the measurement supports it**

Replace the predicate in both `overview_totals` and `session_stats`. The
environment fragment inside the `EXISTS` uses the alias `e` and reuses the
existing bind index, exactly as Task 9's LATERAL does.

Under `EnvFilter::All` the `EXISTS` must not be emitted at all — `All` keeps
`errors_count > 0`, which is correct app-wide and is the fast path.

- [ ] **Step 5: Run the tests**

Run: `cd backend && TEST_DATABASE_URL=... cargo test -p sauron-db --test env_scoping crashed`
Expected: PASS.

- [ ] **Step 6: Document `bump_session` either way**

Regardless of which branch Step 3 took, add to `bump_session`'s doc comment:

```rust
/// `environment_id` is `COALESCE(EXCLUDED.environment_id,
/// sessions.environment_id)` — the most recent non-null value wins — and
/// `events_count`/`errors_count` accumulate across every environment that
/// touched this session id. The row's own environment label therefore cannot
/// disambiguate its counters. Readers that need per-environment truth derive
/// it from the environment-stamped child tables instead; see
/// `events_for_session`, which says the same thing for the same reason.
```

- [ ] **Step 7: Deliberate-break proof (if implemented)**

Revert to `errors_count > 0`, run the test, paste the failure, restore.

- [ ] **Step 8: Run the verification gate**

---

## Phase 4 — Dashboard (Tasks 11–13)

The pure models come first and carry the regression tests, because the bug they
prevent is silent and destructive: opening the grant editor and pressing Save
must never revoke a grant nobody touched.

---

### Task 11: `ScopeType`, the grant plan, and the fourth ancestor

**Files:**
- Modify: `dashboard/src/lib/models/index.ts:119` and its `ScopeType` consumers (`:156`, `:186`, `:226`, `:232`, `:249`)
- Modify: `dashboard/src/lib/models/scope-tree.ts`
- Modify: `dashboard/src/lib/models/grant-plan.ts`
- Create: `dashboard/src/lib/models/scope-type.test.ts`
- Test: `dashboard/src/lib/models/{scope-tree,grant-plan}.test.ts`

**Interfaces:**
- Produces: `ScopeType = 'org' | 'project' | 'app' | 'env'`; `ScopeSelection { org, projects, apps, envs }`; `selectionToScopes(sel, orgId, projectOfApp?, appOfEnv?)`; `isImpliedByAncestor(sel, level, parentId, grandparentId?)`.

- [ ] **Step 1: Write the drift test**

`ScopeType` has no guard against the backend CHECK constraint — it can drift
silently, which is exactly what `permissions.test.ts` exists to prevent one level
up. Create `dashboard/src/lib/models/scope-type.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// Read the backend migration rather than a hand-copied list, for the same
// reason permissions.test.ts reads rbac.rs: a copy only catches drift the
// frontend introduces, never drift the backend introduces.
const MIGRATIONS = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../../backend/migrations',
);

/** The scope types the live CHECK constraint accepts, newest migration wins. */
function backendScopeTypes(): string[] {
  const dirs = readdirSync(MIGRATIONS).sort();
  let found: string[] | null = null;
  for (const d of dirs) {
    const p = path.join(MIGRATIONS, d, 'up.sql');
    let sql: string;
    try {
      sql = readFileSync(p, 'utf8');
    } catch {
      continue;
    }
    const m = sql.match(/CHECK\s*\(\s*scope_type\s+IN\s*\(([^)]*)\)/i);
    if (m) {
      found = [...m[1].matchAll(/'([a-z]+)'/g)].map((x) => x[1]);
    }
  }
  if (!found) throw new Error('no scope_type CHECK constraint found in migrations');
  return found;
}

describe('ScopeType mirrors the backend CHECK constraint', () => {
  it('accepts exactly the scope types role_grants does', () => {
    expect(backendScopeTypes().sort()).toEqual(
      ['app', 'env', 'org', 'project'].sort(),
    );
  });
});
```

Add the missing `readdirSync` import. This asserts against a literal list that a
developer must consciously edit — the value is that the *backend* changing alone
breaks it.

- [ ] **Step 2: Write the failing model tests**

Append to `dashboard/src/lib/models/grant-plan.test.ts`:

```ts
describe('environment grants (the fourth level)', () => {
  it('buckets an env grant into envs, not apps', () => {
    // grantsToBlocks' final branch is `else -> app`, so before this fix an env
    // grant landed in selection.apps and was re-emitted on Save as
    // scope_type:'app' carrying an ENVIRONMENT's uuid — writing a grant that
    // points at nothing.
    const blocks = grantsToBlocks(
      [{ id: 'g1', role_id: 'r1', scope_type: 'env', scope_id: 'env-1' }],
      'org-1',
      new Set(['proj-1']),
      new Set(['app-1']),
      new Set(['env-1']),
    );
    expect(blocks[0].selection.envs).toEqual(['env-1']);
    expect(blocks[0].selection.apps).toEqual([]);
    expect(blocks[0].unmatched).toEqual([]);
  });

  it('is a no-op when an env grant sits UNDER an already-granted app', () => {
    // The regression this coverage-diff exists to prevent, one level lower than
    // the app-under-project case it was originally written for.
    const grants = [
      { id: 'g1', role_id: 'r1', scope_type: 'app', scope_id: 'app-1' },
      { id: 'g2', role_id: 'r1', scope_type: 'env', scope_id: 'env-1' },
    ];
    const blocks = grantsToBlocks(grants, 'org-1', new Set(['proj-1']), new Set(['app-1']), new Set(['env-1']));
    const plan = planGrantChanges(blocks, grants, 'org-1', { 'app-1': 'proj-1' }, { 'env-1': 'app-1' });
    expect(plan.additions).toEqual([]);
    expect(plan.revocations).toEqual([]);
  });

  it('is a no-op when an env grant sits under an already-granted project', () => {
    const grants = [
      { id: 'g1', role_id: 'r1', scope_type: 'project', scope_id: 'proj-1' },
      { id: 'g2', role_id: 'r1', scope_type: 'env', scope_id: 'env-1' },
    ];
    const blocks = grantsToBlocks(grants, 'org-1', new Set(['proj-1']), new Set(['app-1']), new Set(['env-1']));
    const plan = planGrantChanges(blocks, grants, 'org-1', { 'app-1': 'proj-1' }, { 'env-1': 'app-1' });
    expect(plan.revocations).toEqual([]);
  });

  it('still revokes an env grant the user actually unticked', () => {
    const grants = [{ id: 'g1', role_id: 'r1', scope_type: 'env', scope_id: 'env-1' }];
    const blocks = grantsToBlocks(grants, 'org-1', new Set(['proj-1']), new Set(['app-1']), new Set(['env-1']));
    blocks[0].selection = { ...blocks[0].selection, envs: [] };
    const plan = planGrantChanges(blocks, grants, 'org-1', { 'app-1': 'proj-1' }, { 'env-1': 'app-1' });
    expect(plan.revocations.map((r) => r.id)).toEqual(['g1']);
  });

  it('collapses an env under a ticked app to a single app scope', () => {
    const sel = { org: false, projects: [], apps: ['app-1'], envs: ['env-1'] };
    expect(selectionToScopes(sel, 'org-1', { 'app-1': 'proj-1' }, { 'env-1': 'app-1' })).toEqual([
      { scope_type: 'app', scope_id: 'app-1' },
    ]);
  });
});
```

And to `scope-tree.test.ts`:

```ts
describe('isImpliedByAncestor with environments', () => {
  it('treats an env as implied by its app, its project, or the org', () => {
    expect(isImpliedByAncestor({ org: true, projects: [], apps: [], envs: [] }, 'env', 'app-1', 'proj-1')).toBe(true);
    expect(isImpliedByAncestor({ org: false, projects: ['proj-1'], apps: [], envs: [] }, 'env', 'app-1', 'proj-1')).toBe(true);
    expect(isImpliedByAncestor({ org: false, projects: [], apps: ['app-1'], envs: [] }, 'env', 'app-1', 'proj-1')).toBe(true);
    expect(isImpliedByAncestor({ org: false, projects: [], apps: [], envs: [] }, 'env', 'app-1', 'proj-1')).toBe(false);
  });
});
```

- [ ] **Step 3: Run to verify failure**

Run: `cd dashboard && npm test -- grant-plan scope-tree scope-type`
Expected: FAIL on all of them.

- [ ] **Step 4: Widen the models**

`index.ts:119`: `export type ScopeType = 'org' | 'project' | 'app' | 'env';`

`scope-tree.ts`:
- `ScopeSelection` gains `envs: string[]`; `EMPTY_SELECTION` gains a frozen
  `envs: NO_IDS`.
- `selectionToScopes` gains an `appOfEnv?: Record<string, string>` parameter and
  a fourth emission block: an env emits only if its parent app is not ticked
  **and** its grandparent project is not ticked. Emission order stays stable:
  org, projects, apps, envs.
- `isImpliedByAncestor` gains the `'env'` level and a `grandparentId` argument.
- `describeSelection` counts environments.

`grant-plan.ts`:
- `grantsToBlocks` gains a `knownEnvs: Set<string>` parameter and an explicit
  `'env'` branch **before** the `else`. Replace the bare `else` with
  `else if (grant.scope_type === 'app')` plus a final `else -> unmatched`, so a
  fifth scope type added later lands in `unmatched` rather than being silently
  mis-bucketed as an app. That is the actual defect here, not just the missing
  branch.
- `isCovered` gains the fourth case: an env grant is covered by its app's key, by
  its project's key, or by the org key. It needs `appOfEnv` to walk up.
- `planGrantChanges` threads `appOfEnv` through.

- [ ] **Step 5: Run the tests**

Run: `cd dashboard && npm test`
Expected: PASS — all pre-existing tests too.

- [ ] **Step 6: Deliberate-break proof**

Restore `grantsToBlocks`' bare `else -> app` branch, run
`buckets an env grant into envs, not apps` and
`is a no-op when an env grant sits UNDER an already-granted app`, paste both
failures into `.superpowers/sdd/s3-task-11-report.md`, restore.

- [ ] **Step 7: Run the dashboard gate**

`npm test`, `npx svelte-check --tsconfig ./tsconfig.json`, `npm run build`.

---

### Task 12: `ScopeTree` grows a third nested level

**Files:**
- Modify: `dashboard/src/lib/components/members/ScopeTree.svelte`
- Modify: `dashboard/src/lib/components/members/EditMemberDialog.svelte`
- Modify: `dashboard/src/lib/components/members/CreateMemberDialog.svelte`
- Modify: `dashboard/src/lib/components/members/MembersTable.svelte:43-60`
- Modify: `dashboard/src/lib/api/orgs.ts` (load environments for the tree)

**Interfaces:**
- Consumes: Task 11's `ScopeSelection.envs`, `selectionToScopes`, `isImpliedByAncestor`.
- Produces: a four-level picker; `scopeLabel`/`scopeTone` handling `'env'`.

- [ ] **Step 1: Extend the component's props and toggles**

`ScopeTree.svelte` gains `envsByApp: Record<string, { id: string; name: string }[]>`
and derives `appOfEnv` internally the way it already derives `projectOfApp`.

Add `.lvl-3 { padding-left: 54px }` to match the existing 14/34 progression, and
an app-level disclosure twisty mirroring the project-level one, auto-opening when
any environment inside is ticked.

`toggleApp` must now **absorb** its environments out of `sel.envs`, exactly as
`toggleProject` absorbs apps — and for the same documented reason: otherwise
unticking the app later leaves orphaned environment grants. Copy the existing
comment's reasoning down a level rather than leaving the new code silent.

`toggleProject` must absorb both apps **and** their environments.

- [ ] **Step 2: Load environments for the dialogs**

Both dialogs already load projects and apps. Add environments per app. Fetch them
for the apps in scope only — an org with 500 apps must not issue 500 requests on
dialog open. If a batched endpoint does not exist, load environments lazily when
an app's twisty is first opened, and show a small inline spinner in that row.

State the choice explicitly in the report; a silent N+1 on dialog open is the
failure mode here.

- [ ] **Step 3: Render environment grants in the members table**

`MembersTable.svelte`'s `scopeLabel` gains an `'env'` case
(`"Env: App / name"`, with the same missing-target fallbacks the other levels
have, since `scope_id` has no FK). `scopeTone` gets a fourth tone.

- [ ] **Step 4: Verify in the browser**

Start the dev server via `preview_start`. Then, through the UI:
1. Create a member with a single environment ticked. Confirm the request body
   carries `scope_type: 'env'`.
2. Reopen Edit on that member, change nothing, press Save. **Confirm no DELETE
   is issued** — the coverage-diff regression, checked against the real network
   panel rather than only in vitest.
3. Tick the parent app. Confirm the environment row goes dimmed and disabled.
4. Save, reopen, confirm the tree reseeds to the app-level tick with no
   environment rows ticked.

Capture the network panel for step 2 in `.superpowers/sdd/s3-task-12-report.md`.

- [ ] **Step 5: Run the dashboard gate**

---

### Task 13: The picker and `can()` respect reach

**Files:**
- Modify: `dashboard/src/lib/stores/session.svelte.ts:117-124`
- Modify: `dashboard/src/lib/components/layout/Topbar.svelte`
- Test: `dashboard/src/lib/stores/session.test.ts`

**Interfaces:**
- Consumes: `/v1/apps/{id}/environments` now reach-filtered (Task 7).
- Produces: `can(permission, scope)` accepting an env scope.

- [ ] **Step 1: Write the failing test**

```ts
describe('can() with an environment scope', () => {
  it('grants on an env scope only for that environment', () => {
    seedAccess({
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: ['issue:read'] }],
    });
    store.setEnvironment('env-1');
    expect(store.can('issue:read', 'env')).toBe(true);
    store.setEnvironment('env-2');
    expect(store.can('issue:read', 'env')).toBe(false);
  });

  it('lets an app grant satisfy an env-scoped check', () => {
    seedAccess({
      grants: [{ scope_type: 'app', scope_id: 'app-1', permissions: ['issue:read'] }],
    });
    store.setEnvironment('env-1');
    expect(store.can('issue:read', 'env')).toBe(true);
  });
});
```

Match `seedAccess`/`store` to the file's existing helpers — read them first.

- [ ] **Step 2: Run to verify failure, then implement**

`can()` is a flat three-way OR against `currentOrgId`/`currentProjectId`/
`currentAppId`. Add the environment level, keeping the cascade: an env check is
satisfied by a grant at env, app, project **or** org level — the mirror of
`grant_applies`.

- [ ] **Step 3: The picker needs no filtering change**

`listEnvironments` is already reach-filtered server-side by Task 7, so the store
needs no client-side filter — and must not grow one. Add a comment saying so, or
someone will add a redundant filter that drifts from the backend's rule.

Do confirm the "All environments" entry still makes sense: for a partial-reach
member it now means "all mine", which the backend resolves. Update its label
only if the environment list is a strict subset — otherwise leave the copy alone.

- [ ] **Step 4: Verify in the browser**

Log in as the env-scoped member from Task 6's fixture. Confirm: the picker lists
one environment; the Issues page loads; switching to "All environments" returns
the same rows rather than 403ing; no console errors.

- [ ] **Step 5: Run the dashboard gate**

---

## Phase 5 — Carry-forwards (Tasks 14–15)

---

### Task 14: The router-enumeration test (closes F2, F6, F7)

**Files:**
- Test: `backend/bins/sauron-api/tests/http_env_scoping.rs`
- Test: `dashboard/src/lib/api/scope.test.ts`

**Interfaces:**
- Consumes: the completed Phase 2 routing.

`environment_id` handling has been got wrong four times in four disguises, each
time because two hand-maintained lists had to agree. This is the only mechanism
that makes them check each other.

- [ ] **Step 1: Write the enumeration test**

```rust
/// Every `/v1/apps/{id}/...` GET must either NARROW on `?environment_id=` or
/// reject it with a 400. Silently ignoring it is the defect class that has
/// recurred four times: the caller believes a filter was applied and it was
/// not.
///
/// This walks the real router rather than a hand-written list, so a route
/// added tomorrow is covered without anyone remembering to add it here.
#[tokio::test]
async fn every_app_scoped_get_either_narrows_or_rejects_environment_id() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let f = h.seed_env_scoped_member().await;

    let mut unhandled = Vec::new();
    for path in h.app_scoped_get_paths(f.app_id) {
        let with_bad = h
            .get_as(&f.owner_token, &format!("{path}?environment_id=not-a-uuid"))
            .await;
        // A narrowing route 400s on a malformed value; a rejecting route 400s
        // on any value. Either way, 200 means the parameter was ignored.
        assert_ne!(
            with_bad.status(),
            200,
            "{path} accepted a malformed environment_id — it is neither \
             narrowing nor rejecting, so it is silently ignoring the parameter"
        );
        if with_bad.status() != 400 {
            unhandled.push(format!("{path} -> {}", with_bad.status()));
        }
    }
    assert!(unhandled.is_empty(), "unexpected statuses: {unhandled:?}");
}

/// The set of routes that reject `environment_id` must equal the set the
/// dashboard's interceptor excludes. Maintained in two files, checked in one.
#[tokio::test]
async fn the_backend_rejection_set_matches_the_dashboard_exclusion_list() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let f = h.seed_env_scoped_member().await;

    let mut rejecting = Vec::new();
    for path in h.app_scoped_get_paths(f.app_id) {
        // A rejecting route 400s even on a perfectly VALID value.
        let r = h
            .get_as(&f.owner_token, &format!("{path}?environment_id={}", f.granted_env))
            .await;
        if r.status() == 400 {
            rejecting.push(path);
        }
    }
    rejecting.sort();

    let expected = read_dashboard_exclusions();
    assert_eq!(
        rejecting, expected,
        "backend rejection set and dashboard exclusion list have diverged"
    );
}
```

`app_scoped_get_paths` derives paths from `main.rs`'s router. Prefer
`axum::Router`'s introspection if available in the pinned version; otherwise
parse `main.rs` for `.route("/v1/apps/{app_id}/...", get(...))` — a parse is
acceptable here because the alternative is a hand-written list, which is the
thing being eliminated. Substitute a real `app_id` and any other path parameters
from the fixture.

`read_dashboard_exclusions` reads `dashboard/src/lib/api/scope.ts` and extracts
the exclusion entries, the way `permissions.test.ts` reads `rbac.rs`.

- [ ] **Step 2: Replace the tautological page guard (F6)**

`scope.test.ts`'s `TELEMETRY_PAGES` is a hand-written array that is exactly the
set of pages containing the string `scopeKey`, so `missing` is `[]` by
construction and the companion length assertion is a tautology over a `const`
three lines above.

Derive the set from the filesystem: read `dashboard/src/pages/*.svelte`, and
assert every page that calls an app-scoped telemetry API also keys a load effect
on `scopeKey`. Keep an explicit, commented allow-list of genuinely
non-telemetry pages (Alerts, ChangePassword, Docs, Login, Members, MonitorDetail,
Monitors, Onboarding, Projects, Register, SettingsApp, SourceMaps, Storage) so a
new telemetry page must be consciously added to it to pass.

- [ ] **Step 3: Run, and expect real findings**

Run: `cd backend && TEST_DATABASE_URL=... TEST_REDIS_URL=... cargo test -p sauron-api --test http_env_scoping`

If either test fails, that is a **finding, not a test bug**. Fix the route or the
exclusion list, and record what was found in
`.superpowers/sdd/s3-task-14-report.md`. A green result on first run is
suspicious enough to double-check that `app_scoped_get_paths` actually returned
a non-empty list — assert its length is at least 20.

- [ ] **Step 4: Run the full gate**

---

### Task 15: F8, F9 and the fresh-login race

**Files:**
- Test: `backend/crates/sauron-db/tests/env_scoping.rs`
- Modify: `dashboard/src/App.svelte`, `dashboard/src/pages/Login.svelte`
- Test: `dashboard/src/lib/stores/session.test.ts`

- [ ] **Step 1: Cover `top_issues`' untested branches (F8)**

`top_issues`' `All` arm is a separate boxed-diesel branch ranking by the stored
`issues.times_seen`, and nothing executes it. `Unattributed` takes the raw-SQL
path with no `$4` bind, so a bind-index regression there surfaces only at
runtime. Add one test per branch asserting the ranking order, using seed values
where the app-wide and per-environment orders genuinely differ — otherwise the
test cannot tell the two branches apart.

- [ ] **Step 2: Strengthen the swap-blind assertions (F9)**

Each currently catches "the filter was dropped" but not "the wrong filter was
applied". Use the discriminating values already in the seed: assert the exact
expected value rather than merely `> 0` or `!= total`. Name each strengthened
assertion in the report.

- [ ] **Step 3: Fix the fresh-login double `load()`**

`App.svelte`'s post-auth redirect and `Login.svelte`'s forced load can both
start a full `sessionStore.load()` chain. It predates this feature and fires
identically without environments; the worst case is a duplicated bootstrap fetch
chain. Make `load()` idempotent while in flight — return the existing promise
rather than starting a second chain — and assert the call count in a test, the
way `s2-task-13-dupe-fetch-fix.md` did for `loadAppEnvironments`.

- [ ] **Step 4: Run the full gate, both suites**

---

## Final review

After Task 15, before declaring the slice done:

- [ ] Run the complete verification gate one more time from a clean `cargo build`.
- [ ] Confirm `grep -c '^diesel::table!' backend/crates/sauron-db/src/schema.rs` = **27**.
- [ ] Confirm `git status` shows changes but **no commits and no new branch**.
- [ ] Re-read the spec's "Locked decisions" and point at the task implementing
      each. Any decision without a task is a gap — say so rather than assuming.
- [ ] **Decision 5 has no task by design** (it confirms existing behaviour).
      Verify it anyway: `PersonRow::properties` and `DeviceRow`'s descriptor
      fields must still be app-wide, and their doc comments — which currently
      say Slice 3 "should make this choice explicitly" — must be updated to
      record that it now HAS been made, citing this spec. A doc comment still
      deferring to a slice that has shipped is a stale comment.
- [ ] Write `.superpowers/sdd/s3-final-review.md` covering: what shipped, what
      was measured and declined (Task 10 may legitimately be in this category),
      and every carry-forward that remains open.
