# Slice 3 — Environment as an RBAC scope

Date: 2026-07-29
Status: approved, not yet implemented
Predecessors: `2026-07-28-per-app-environments-design.md` (S1),
`2026-07-28-environment-scoped-reads-design.md` (S2)

## The problem

S1 made environments real objects under an app, each owning its own ingest key.
S2 made every telemetry read filterable by `?environment_id=`. Both stopped short
of the thing the feature was asked for: **an environment a member cannot see.**

Today `?environment_id=<uuid>` is a display filter. `parse_env` validates only
that the value is a well-formed UUID; a foreign environment's id ANDs against
`app_id` and matches nothing. That is the safe direction, but "matches nothing"
is not "you may not ask" — as `routes/scope.rs:74-85` already says in a comment
written during S2 in anticipation of this slice.

This slice turns *which environment am I looking at* into *which environments may
I look at*.

## Goals

- `Scope::Env` as a fourth grant level, with the existing cascade semantics.
- An environment a member holds no grant on is unreadable, not merely unselected.
- Reads narrow automatically to the caller's readable environments.
- The two cross-environment data defects that become access defects once the
  boundary is real: `issues` string/level fields, and `sessions` counters.
- The Slice 2 review's remaining carry-forwards (F6–F9).

## Non-goals, and why

| Excluded | Reason |
|---|---|
| **Alert rules per environment** | `alert_new_issues` / `alert_regressed_issues` have no environment predicate, so a staging event advances `last_event_at` on a prod-resolved issue and fires a false regression. Fixing it properly needs an `environment_id` on `alert_rules`, UI, and evaluation changes — a feature, not a fix. Flagged for S4. |
| **Cold-tier environments** | Still deferred from S2. The three timeseries endpoints continue to `400`. |
| **F10 fresh-install index churn** | Cosmetic; squashing the two migrations would destroy the measurement record their comments carry. |
| **`PersonRow::properties` per environment** | `event_users` has no `environment_id`, so unlike `first_seen`/`last_seen` there is no per-environment copy to fall back to — the bag either is app-wide or does not exist. Membership already gates whether the row is visible at all. Decided explicitly here rather than inherited silently; see "Decision 5". |
| **`issues.title`/`culprit` under `EnvFilter::All`** | `All` keeps reading the durable columns. Same fast-path convention as every other fix in this series. |

## Locked decisions

### Decision 1 — Partial reach auto-narrows

A member holding grants on some of an app's environments sees, for an **absent**
`environment_id`, exactly the union of the environments they hold. The topbar
never offers an option that would `403`.

Rejected: requiring an explicit environment (no combined view — a two-of-three
member would have to toggle one at a time), and falling back to a default
environment (shows one environment under a label that says "all", which is the
same fail-open shape this feature has now hit four separate times).

### Decision 2 — `EnvFilter` gains `Subset(Vec<Uuid>)`

```rust
pub enum EnvFilter { All, One(Uuid), Subset(Vec<Uuid>), Unattributed }
```

`Subset` emits `AND environment_id = ANY($n)` and consumes **exactly one bind,
like `One`**, so no bind index shifts anywhere in the 25 raw statements. `= ANY`
naturally excludes `NULL`, which is the correct semantics: unattributed rows
belong to no environment and so belong to nobody's readable set.

`EnvFilter` stops being `Copy` (it now owns a `Vec`). `ReadScope` follows. This
is a compile error at every call site that assumed copy semantics, which is the
point — the 38 `ReadScope`-taking functions all get looked at.

### Decision 3 — Issues: derive `title`, `culprit`, `level` per environment

`upsert_issue`'s `ON CONFLICT (app_id, fingerprint)` has no environment in the
conflict key and overwrites `title`/`culprit`/`level` from `EXCLUDED.*` on every
occurrence. A caller scoped to staging sees strings written by a production
occurrence, directly beside a correctly staging-scoped `last_seen`.

**`error_events` gains `title` and `culprit` columns**, written at ingest from
the values `process.rs:138-139` already computes immediately before
`upsert_issue`. The per-environment read then *pulls* the newest in-environment
occurrence's strings rather than recomputing them.

This is the load-bearing part of the decision. The alternative — reconstructing
the strings in SQL from `exception_type`/`exception_value`/`message`/`stacktrace`
— works for `title` but not for `culprit`, whose value comes from selecting a
stack frame (reverse-find `in_app == Some(true)`, else the last frame, rendered
`"{func} ({filename|module})"` with `"?"` as the function default). Expressing
that as JSONB path logic would be a second implementation of `build_culprit`
that drifts silently the first time the Rust changes. Persisting the string
costs no extra computation and cannot drift.

`level` is already a real column on `error_events`; no new storage needed.

**No backfill.** Pre-migration rows have `NULL` `title`/`culprit`, and the
derivation `COALESCE`s to the app-wide `issues` column — so old data degrades to
exactly today's behaviour rather than to empty strings. This avoids a rewrite of
the hottest partitioned table.

Also fixed, and arguably the sharper bug: **`list_issues`' filters read stored
columns inside the paging subquery while the outer select returns derived
values.** `level = $n`, `culprit = $n`, `times_seen > $n`, `users_seen > $n` all
filter app-wide while displaying per-environment. Filters move onto the derived
values so the two agree. And `issue_stats`' `count(*) FILTER (WHERE level=…)`
breakdown becomes per-environment — today an environment's fatal/error/warning
split reflects whichever environment sent the last event.

**`updated_at` stays app-wide, documented.** It is bumped by both ingest and
`update_issue_status`, so it is not a per-occurrence fact. Deriving it from the
newest in-environment occurrence would make it numerically identical to the
derived `last_seen` — a duplicate field pretending to mean something else. Its
only consumer is `IssueDetail.svelte:109`, which assigns it from the
status-update response and never renders it. (Marking it `#[serde(skip_serializing)]`
alongside `last_event_at` would remove the incoherence outright; noted as a
follow-up rather than done here, since it is an API-shape change.)

### Decision 4 — Sessions: derive `crashed` per environment, gated on measurement

`bump_session` folds every signal into one row per `(app_id, session_id)` and
sets `environment_id = COALESCE(EXCLUDED.environment_id, sessions.environment_id)`
— last non-null wins. `overview_totals.crashed_sessions` and
`session_stats.crashed` both count `errors_count > 0`, a lifetime
cross-environment counter, under whichever environment label last stamped the
row.

Replace the counter predicate with an `EXISTS` against `error_events` correlated
on `session_id` + `app_id` + environment — the child tables *are* environment
stamped, and `events_for_session` already reads them exactly this way for the
same stated reason.

**This is gated on `EXPLAIN (ANALYZE, BUFFERS)` before and after**, the same way
the S2 covering index was justified. It converts a column predicate into a
semi-join on a partitioned table. If it does not pay, it is dropped and the
behaviour documented instead — the decision is the measurement, not the
intention.

Rejected: changing the conflict key to `(app_id, session_id, environment_id)`.
Exact by construction, but it redefines session identity — `UNIQUE (app_id,
session_id)` is what `get_session` and three membership `EXISTS` legs rely on,
and a device that switches environments mid-session would become two sessions in
every duration and count metric.

### Decision 5 — `PersonRow::properties` and `DeviceRow` descriptors stay app-wide

Confirmed rather than deferred again. A person has one property bag and a device
one descriptor; there is no per-environment copy. Membership already gates
visibility, so the caller is looking at someone genuinely active in their
environment. The existing doc comments stand; this spec is the explicit decision
the S2 review asked for.

## The RBAC core

### `Scope::Env`

```rust
pub enum Scope { Org(Uuid), Project(Uuid), App(Uuid), Env(Uuid) }
```

Cascade extends unchanged: **org ⊇ project ⊇ app ⊇ env**, union down the tree,
strict sibling isolation. An app grant covers every environment including ones
created later. An env grant covers only that environment.

`effective_permissions` and `has_permission` **gain a fourth parameter** rather
than acquiring `_env` siblings. A parallel function set is precisely the
two-lists-kept-in-sync-by-hand shape that has caused four defects in this
feature; changing the signature makes all 36 call sites a compile error that has
to be read.

`Reach` gains `envs: Vec<Uuid>`, and `Scope::parts()` gains `("env", id)`.

### Two silent landmines

Both were found by mapping and both fail invisibly:

1. **`grants_from_rows` drops unknown `scope_type`** (`rbac.rs:291`,
   `_ => return None`). Adding `'env'` to the DB CHECK without the matching arm
   makes every environment grant vanish at read time. Fail-closed, but with no
   signal at all.
2. **`guard.rs:108` `scope_parts` falls back to `(None, None)`** — org scope —
   for unknown strings. An env grant reaching the escalation check without a new
   arm gets evaluated at the wrong level.

Both get an arm and a test that fails if the arm is removed.

### Enforcement

`authorize_env` reuses `repo::env_ancestry`, which already exists and whose doc
comment already says "Slice 3's `authorize_env` reuses this."

The complete wire contract:

| caller's reach on the app | `environment_id` absent | `=<uuid>` | `=none` |
|---|---|---|---|
| app-wide (org/project/app grant) | `All` | `One` after existence + ownership check | `Unattributed` |
| partial (env grants only) | `Subset(their envs)` | `One` if granted, else `403` | `403` |
| none | `403` | `403` | `403` |

`Unattributed` requires app-wide reach. `parse_env`'s long-standing existence +
ownership gap closes here: `One(uuid)` resolves against `environments WHERE
app_id = $1`, folded into the authorize path rather than added as a separate
round-trip.

**Back-compatibility:** no environment grants exist today, so every current
member has app-wide reach on every app they can reach, and every resolution in
the table above lands on the row that reproduces today's behaviour exactly.

## Discovery and the dashboard

`list_environments` gets the `reach_for` treatment, mirroring `list_apps`
(`projects.rs:138-170`) line for line: resolve the org, load grants once, return
everything on `reach.org || reach.projects.contains(p) || reach.apps.contains(a)`,
otherwise filter by `reach.envs`.

`GET /orgs/{org_id}/access` serializes grants through a hand-rolled match at
`orgs.rs:109-113` that duplicates `Scope::parts()`. It gets the env arm — by
being replaced with a call to `Scope::parts()`, so the next level cannot drift.

### A latent corruption bug in the grant editor

`grant-plan.ts:100-103` buckets grants `if org … else if project … else → app`.
An env grant lands in `selection.apps` and is re-emitted on Save as
`scope_type: 'app'` carrying an environment's UUID — writing a grant that points
at nothing. `isCovered` (`grant-plan.ts:109-132`) and `isImpliedByAncestor`
(`scope-tree.ts:142-149`) each need their fourth ancestor case, or opening the
dialog and pressing Save silently revokes environment grants — the exact
regression the coverage-diff was built to prevent, one level down.

`ScopeTree` becomes three nested levels under the org row. `session.svelte.ts`'s
`can()` gains the environment level.

`models/index.ts:119`'s `ScopeType` union has **no drift test** against the
backend CHECK constraint. It gets one, modelled on `permissions.test.ts`, which
already parses `rbac.rs` directly for exactly this reason.

## Carry-forward cleanup

- **F7 — router-enumeration test.** One test that walks `main.rs`'s route table
  and asserts every `/v1/apps/{id}/…` GET either narrows or `400`s on
  `?environment_id=`, and that the set which `400`s equals the dashboard's
  exclusion list. This closes F6 and F7 and makes F2's hand-maintained
  correspondence self-checking. F2 has recurred four times in four disguises;
  this is the only mechanism that stops a fifth.
- **F8** — `top_issues`' `All` and `Unattributed` branches get tests.
- **F9** — swap-blind assertions strengthened using discriminating values that
  already exist in the seed.
- **Fresh-login double `load()` race** — `App.svelte`'s post-auth redirect versus
  `Login.svelte`'s forced load.

## Migrations

Starting at `2026-07-29-000029`. Two:

1. `role_grants` CHECK constraint: drop and recreate including `'env'`. The
   constraint is unnamed, so it carries Postgres's auto-name
   `role_grants_scope_type_check`. **`down.sql` must delete env grants before
   restoring the old constraint**, or the rollback fails against its own data.
2. `error_events` gains nullable `title` and `culprit`. No backfill, no rewrite;
   the read path `COALESCE`s to the `issues` column for pre-migration rows.

Both partitioned-table changes are `ADD COLUMN` with no default, so they are
catalog-only and do not rewrite. The `role_grants` CHECK swap takes a brief
`ACCESS EXCLUSIVE` on a small table.

## Testing

The enforcement suite (`env_scoping.rs`, currently 37 tests) is what makes this
slice's guarantees real, and CI now runs it (F1, fixed in S2).

- **`rbac.rs` unit tests** for the four-level cascade: an env grant satisfies
  only that environment; an app grant satisfies every environment under it; a
  sibling environment is denied; `reach_for` decomposes all four levels. Plus a
  test that fails if `grants_from_rows`' `"env"` arm is removed.
- **`env_scoping.rs`**: `Subset` across all affected functions; per-environment
  `title`/`culprit`/`level`; the `issue_stats` level breakdown; `crashed`. The
  issues test mirrors
  `get_event_user_seen_is_derived_per_environment_not_app_wide` — assert the
  scoped values differ from each other *and* from `All`, using `SeedIds`'s
  `pinned_now` for exact expected values rather than relative orderings.
- **`http_env_scoping.rs`**: the F7 enumeration, plus the `403` rows of the wire
  contract table driven over the real router.
- **Seed extension**: `seed_two_envs` needs one issue recurring in both
  environments with *different* titles and a known ordering, or the derivation
  cannot be discriminated from the app-wide read. Row counts in `SeedIds`'
  table must be updated in the same commit as the assertions they feed.
- **Deliberate-break proof for every fix** — revert it, confirm the test fails,
  restore. Standard for this series.

## Verification gate

Per `.superpowers/sdd/s2-get-event-user-fix.md`: `cargo fmt --all --check`;
`cargo clippy --workspace --all-targets -- -D warnings`; `cargo test -p sauron-db
--test env_scoping` with `TEST_DATABASE_URL`; `cargo test --workspace` both with
and without `TEST_DATABASE_URL`; `grep -c '^diesel::table!' schema.rs` = **27**;
dashboard `npm test`, `svelte-check`, `npm run build`.

Constraints that bind every task: **no `diesel` CLI command, ever** (it rewrites
`schema.rs` from 27 table blocks to 87 and still compiles — the count is the only
detector). **No commit, no branch.** DuckDB env vars from the repo-root
`.cache/duckdb/…`; never `--all-features`. `localhost:5432` is an unrelated
container — use the container IP.

## Principal risks

1. **`EnvFilter` losing `Copy`** ripples further than the 25 bind sites. Expected
   to be mechanical, but it is the change most likely to sprawl; if it does, the
   fallback is an explicit `.clone()` at the boundary rather than reshaping call
   sites.
2. **The `crashed` semi-join may not pay.** Gated on measurement, with
   documenting the behaviour as the accepted fallback.
3. **Seed changes invalidate existing assertions.** S2 hit this twice, both times
   because a snippet in the plan went stale. Every seed edit names the assertions
   it moves, in the same task.
4. **The four-level `ScopeTree` is the largest untested-by-machine surface.** The
   coverage-diff regression it must not reintroduce is silent and destructive,
   so `grant-plan.test.ts` gets the fourth-level cases before the component
   changes.
