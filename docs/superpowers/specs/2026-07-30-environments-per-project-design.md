# Environments are defined per project, not per app

Date: 2026-07-30
Status: approved, implementing

## Problem

`environments` is parented to `apps` (`environments.app_id`). An admin who runs
five apps in one project has to define `staging` five times, once per app, and
the five rows are unrelated to each other — nothing stops them drifting into
`staging`, `Staging`, and `stage`.

The environment *catalogue* is a project-level concept. What stays per app is
the *data*: events, issues, sessions and transactions remain keyed by
(app, env), and each app+env pair keeps its own ingest credential.

Note this reverts a rename made in `2026-07-12-000002_projects_apps_rbac`,
which turned the init migration's `environments.project_id` into `app_id` when
apps were introduced between orgs and projects.

## Decisions

Four forks were settled before design:

1. **Ingest key lives on an `app_environments` junction.** The env catalogue is
   project-level; the credential and per-app switches belong to the (app, env)
   pair. Both the app and the env stay *provable* from the key alone, which is
   the property `2026-07-28-000026_env_keys` was written to establish.
2. **`env` role grants stay per (app, env).** `scope_id` continues to name a
   junction row, so "Alice can read staging of the mobile app" keeps meaning
   exactly that. Grants are additive, so widening an env grant to span every app
   in a project would have silently expanded access at migration time.
3. **Auto-enroll.** Adding an env to a project mints a key for every app in it;
   creating an app mints keys for every env the project already has. Per-pair
   `ingest_enabled` mutes without deleting.
4. **Data FKs point at the junction.** Achieved by renaming rather than
   remapping — see below.

## Data model

Migration `2026-07-30-000033_env_per_project`.

The pivot: today's `environments` table *already is* the (app, env) pair. It
carries `app_id, name, public_key, ingest_enabled, is_default, retired_at`. So
it is renamed into place rather than rebuilt:

1. `ALTER TABLE environments RENAME TO app_environments`. Postgres foreign keys
   bind to the table OID, so `error_events.environment_id`,
   `analytics_events.environment_id`, `sessions`, `transactions` and `workflows`
   keep referencing the same rows with **zero rows rewritten**. This matters:
   `error_events` and `analytics_events` are partitioned and the hottest-write
   tables in the schema; a remap `UPDATE` across them would need a maintenance
   window (see the warnings in migrations 25 and 27).
2. `CREATE TABLE environments (id, project_id → projects ON DELETE CASCADE,
   name, created_at, updated_at, UNIQUE (project_id, name))` — the new
   project-level catalogue.
3. Backfill the catalogue with `SELECT DISTINCT a.project_id, ae.name`, over
   **all** junction rows including retired ones, so no retired row is left
   without a parent when the FK is enforced.
4. `app_environments.environment_id` added nullable, backfilled by
   `(project_id, name)` match, then `SET NOT NULL`. Columns are added nullable,
   backfilled, then constrained, exactly as migration 26 documents.
5. `ALTER TABLE app_environments DROP COLUMN name` — the name now has exactly
   one home and cannot drift between the catalogue and the enrollment.
6. Auto-enroll backfill: every (app, project-env) pair without a junction row
   gets one, with a fresh `'pk_' || replace(gen_random_uuid()::text,'-','')`
   key. Same construction as migration 26, so no pgcrypto dependency.
7. Constraints: `UNIQUE (app_id, environment_id) WHERE retired_at IS NULL`
   replaces `environments_app_name_active_key`; the one-default-per-app partial
   index survives, renamed to `app_environments_default_key`. Index and
   constraint names are renamed explicitly — a rename leaves them spelling
   `environments_*`, which would misdescribe the schema.

`role_grants` needs **no migration**. Existing `scope_type='env'` rows hold what
is now an `app_environments.id`, which is precisely the per-(app, env) meaning
decision 2 selected.

## Ingest and RBAC

`find_env_by_public_key` ([repo.rs:1158]) changes only its table name. It still
joins → `apps` → `projects` and returns an unchanged `EnvRef`. Consequently:

- every existing SDK key keeps working
- no DSN change, no key rotation, no SDK release
- the Redis `dsn_cache` keying and every `query_plan` filter on
  `environment_id` are untouched

`env_ancestry`, `env_ids_for_app` and `Scope::Env` likewise change table name
only. `authorize_env_read` already takes an `app_id` and a requested env, which
remains well-formed.

## API surface

| Today | After |
|---|---|
| `POST /v1/apps/{app_id}/environments` | removed — envs are not created per app |
| — | `GET/POST /v1/projects/{project_id}/environments` — the catalogue; POST auto-enrolls every app in one transaction |
| `GET /v1/apps/{app_id}/environments` | kept — the app's enrolled rows with keys/DSNs, joined to catalogue names |
| `PATCH/DELETE /v1/environments/{env_id}` | project-level rename/retire; retire cascades `retired_at` to enrolled rows |
| `PATCH /v1/environments/{env_id}` (mute/promote) | `PATCH /v1/app-environments/{id}` — per-app mute and default |
| `POST /v1/environments/{env_id}/rotate-key` | `POST /v1/app-environments/{id}/rotate-key` |

There is deliberately **no `DELETE /v1/app-environments/{id}`**. Withdrawing one
app from an environment was implemented and then removed: enrollment happens
only when an environment or an app is created, so a withdrawal is a one-way door
with no path back short of retiring the environment project-wide and re-keying
every sibling app. `PATCH { ingest_enabled: false }` expresses the same intent
reversibly. `repo::retire_app_environment` survives as the single-row primitive
that lets the cascade's end state be constructed in tests.

Provisioning inverts: project create mints the default `dev` catalogue entry;
app create enrolls the new app in every env the project already has
([projects.rs:242] moves accordingly). The existing guard rails — cannot retire
the last live environment, cannot retire the default, `MAX_ENVIRONMENTS_PER_APP`
— move up to the project level where they now belong.

## Dashboard

`EnvironmentsCard.svelte` splits: catalogue CRUD moves to project settings, app
settings keeps the keys/DSNs table and the per-app mute/default toggles. The
Topbar env selector lists project envs. `scope-tree.ts` and `grant-plan.ts` are
unchanged — env stays nested under app, matching decision 2.

## Testing

- Rework `crates/sauron-db/tests/env_scoping.rs` and
  `bins/sauron-api/tests/http_env_scoping.rs` for the new parentage.
- New coverage: auto-enroll from both directions (add env → all apps; add app →
  all envs), `UNIQUE (app_id, environment_id)`, retire cascade, and the
  last-env/default guards at project level.
- A post-migration assertion that a pre-existing `public_key` still resolves to
  the same `EnvRef` — this is the property that makes the change invisible to
  deployed SDKs, so it is worth a test rather than an argument.

## Risks

- The rename leaves index and constraint names misspelled; handled explicitly in
  step 7 rather than left to drift.
- The untracked `2026-07-29-000032_workflows` migration references
  `environments(id)`. Its FK follows the rename to `app_environments`, which is
  consistent with its own `(app_id, environment_id)` index, but the file needs
  reconciling as part of this work.
- `MAX_ENVIRONMENTS_PER_APP` becomes a per-project cap; the constant is renamed
  so the limit it enforces and the name it carries do not disagree.
