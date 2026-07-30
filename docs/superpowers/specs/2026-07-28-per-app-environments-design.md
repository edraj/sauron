# Per-app environments — Slice 1: entity, keys, and management

Date: 2026-07-28
Status: design approved, ready for planning

## Problem

Environments already exist as a per-app row, but they are discovered rather than
managed. The pipeline upserts one whenever an SDK sends a new `environment`
string in the envelope header (`sauron-pipeline/src/process.rs:29`,
`sauron-db/src/repo.rs:935`). Four things follow from that:

- **No lifecycle.** There is no create, rename, retire, or merge. A typo (`prod`
  vs `production`) becomes a permanent second environment that nothing can clean
  up. The only endpoint is a list (`routes/apps.rs:85`) and the only UI is a
  read-only card (`dashboard/src/pages/SettingsApp.svelte:210`).
- **The env is client-asserted.** The name is a free string chosen by whoever
  holds the key, capped at 64 chars and 500 envs per app purely because it is
  attacker-controlled (`repo.rs:930`). A staging build can write into
  `production` by changing one config line.
- **No per-env operational control.** `ingest_enabled` lives on `apps`, so
  muting a noisy staging deployment means muting production too.
- **Env is not an access boundary.** `role_grants.scope_type` is
  `('org','project','app')`; there is no way to grant someone staging without
  giving them production.

## Goals

All four drivers are in scope for the programme: access isolation, trustworthy
env tagging, per-env operational control, and a consistent
org → project → app → env navigation context.

**This spec covers Slice 1 only.** See "Decomposition" below.

## Decomposition

Env-as-RBAC-scope is not additive: `effective_at` / `authorize_app` take
`(org, project, app)`, and widening that signature touches every
`authorize_app` / `authorize_project` call site across the route layer, while an
"All environments" default means every read endpoint (11 sites in
`routes/analytics.rs` alone, plus sessions, devices, screens, journeys,
performance, funnels, issues) must resolve a *readable env set* and filter on it.
Three sequential slices, each shippable on its own:

| Slice | Contents |
|---|---|
| **S1 (this spec)** | `environments` gains `public_key`, `ingest_enabled`, `is_default`, `retired_at`; CRUD API + Settings UI; app creation seeds `dev`; ingest resolves env from the key and ignores the client string; per-env key rotation; SDKs drop the `environment` option |
| **S2** | 4th topbar switcher + `sessionStore.currentEnvId`; "All environments" as the default; `environment_id` threaded through every read endpoint and the query plan; the Events env-filter chip retired |
| **S3** | `Scope::Env`, `scope_type` CHECK += `'env'`, `env_ancestry`-based `authorize_env`, readable-env-set derivation, 4-level `ScopeTree` + `grant-plan` coverage |

Order matters. S3 depends on S2's readable-env plumbing; doing S3 second would
leave an env-only grant 403-ing everywhere — fail-closed and safe, but useless.

## Locked decisions

1. **Clean break.** No live installs to preserve. One key path, one scope model,
   no legacy branch, no dual-key window.
2. **"All environments" is the default** in the eventual picker (S2), because
   issues are grouped per-app by fingerprint and their events span envs.
3. **App creation seeds exactly one env, `dev`, marked default**, replacing the
   current `production` seed at `routes/projects.rs:151`.
4. **Ingest-time auto-creation is removed entirely.** Envs exist only when an
   admin creates one. This is what kills typo-envs at the source.
5. **Delete means retire, never hard-delete.** The env row is always preserved,
   so cold Parquet rows keep a valid `environment_id`. See "Archival" below.
6. **Environments get their own permission set** — `env:read`, `env:create`,
   `env:update`, `env:delete`, `env:rotate_key` — rather than borrowing the
   app-level ones. See "Permissions" below.

## Archival — why retire, not delete

The intent is that a removed environment's data ends up in cold Parquet storage.
The existing tier worker gets there on its own schedule, but it cannot be
directed at an environment:

- It is **partition-granular** — it exports a whole child partition (every app,
  every env) once it ages past `TIER_HOT_DAYS`, then drops it
  (`bins/sauron-tier/src/main.rs:78-176`). The binary takes no arguments and has
  no on-demand entry point.
- It **never row-deletes**. Reclaim happens only through `DETACH PARTITION` +
  `DROP TABLE` (`repo.rs:3489`).
- Parquet is keyed `{table}/app_id=…/year=…/month=…`
  (`crates/sauron-tier/src/layout.rs:85`). `environment_id` rides along in the
  file data because the export is `SELECT *`, but it is not a hive key and
  appears in no glob or `WHERE` clause — cold data cannot currently be filtered
  by env.
- The `ON DELETE SET NULL` FK cannot reach Parquet. A hard delete would null the
  hot rows and leave cold rows pointing at a dead UUID.

So retiring sets `retired_at` and forces `ingest_enabled = false`: the key stops
working, the env leaves the picker, the data stays queryable and attributable,
and the tier worker carries it to Parquet on its normal schedule. An on-demand
env-scoped exporter with storage reclaim (new exporter, row-level deletes inside
live partitions, `environment_id` as a hive key) is a later slice; it composes
cleanly precisely because the env row is retained either way.

## Data model

```sql
ALTER TABLE environments
  ADD COLUMN public_key     TEXT,                      -- backfilled, then NOT NULL UNIQUE
  ADD COLUMN ingest_enabled BOOLEAN NOT NULL DEFAULT true,
  ADD COLUMN is_default     BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN retired_at     TIMESTAMPTZ,               -- NULL = active
  ADD COLUMN updated_at     TIMESTAMPTZ NOT NULL DEFAULT now();
```

Backfill runs in the same migration, before `SET NOT NULL` and the unique
constraint:

- every existing env gets `'pk_' || replace(gen_random_uuid()::text, '-', '')` —
  exactly the `pk_` + 32-hex shape `ids::public_key()` produces
  (`sauron-core/src/ids.rs:19`), with no pgcrypto dependency. 122 bits of
  entropy rather than 128; acceptable for a one-time backfill, and every key
  minted afterwards uses `getrandom`.
- each app gets exactly one `is_default` env, chosen deterministically: an env
  named `production` if one exists, else one named `dev`, else the
  alphabetically first. Apps with no env at all get a seeded `dev`. Picking
  `production` first matters because existing apps were seeded with it at
  creation and are actively reporting into it — defaulting them to a
  lexicographically-earlier env would silently change which env the dashboard
  treats as primary.
- `apps.public_key` is dropped.

Index changes:

- `UNIQUE (app_id, name)` becomes partial, `WHERE retired_at IS NULL` — retiring
  `staging` must not block creating a fresh `staging` later. The existing
  constraint also backs `upsert_environment`'s `ON CONFLICT`, which is being
  deleted anyway.
- new partial unique index on `(app_id) WHERE is_default` — exactly one default
  per app, enforced by the database rather than by application code.

`MAX_ENVIRONMENTS_PER_APP` stays at 500 but now counts only active envs. With
admin-only creation the cap is just a sanity bound.

The `environment_id` columns on `error_events` / `analytics_events` /
`transactions` stay nullable. Every new write populates them; backfilling
historical NULLs would mean a rewrite across every partition of the two largest
tables for no functional gain.

## Ingest path

`resolve_app` becomes `resolve_env` (`bins/sauron-ingest/src/main.rs:302`),
returning `{ env_id, app_id, project_id, org_id, app_ingest_enabled,
env_ingest_enabled }` from a single `environments ⨝ apps ⨝ projects` lookup on
`public_key WHERE retired_at IS NULL`.

No wire-format change is required. The DSN path segment is already parsed and
discarded — `Path(_project_id)` at `main.rs:174` is never read, and the app is
resolved purely from `X-Sauron-Key` (or the `?k=` sendBeacon fallback). The key
alone has always determined tenancy; it now determines the environment too.

Three consequences:

1. **The Redis cache key prefix must be bumped.** `keys::dsn_cache`
   (`sauron-redis/src/lib.rs:36`) holds a serialized app-shaped value with a 300s
   TTL. Bumping the prefix invalidates the old format atomically at deploy
   instead of serving five minutes of garbage.
2. **`EnvelopeHeader.environment` is deleted, not ignored.** Serde already
   ignores unknown fields, so a stale SDK still ingests — its env just comes from
   its key. `IngestJob.environment: Option<String>` becomes
   `environment_id: Uuid`; `upsert_environment` and `MAX_ENVIRONMENT_NAME_LEN`
   are deleted. This is the change that makes the env trustworthy.
3. **The two rate limiters keep their current shape.** The pre-auth limiter keyed
   on the raw key (`main.rs:192`) now *is* the per-env limit; the post-auth one
   stays on `app_id` as the aggregate ceiling.

`403 ingest_disabled` is returned if *either* the app or the env is muted, so
muting one env leaves the others reporting.

## Permissions

Five new permission strings are added to `perm::ALL`
(`sauron-auth/src/rbac.rs:25-81`): `env:read`, `env:create`, `env:update`,
`env:delete`, `env:rotate_key`.

One is removed in the same change. Dropping `apps.public_key` makes
`app:rotate_key` and its route `POST /v1/apps/{app_id}/rotate-key`
(`routes/apps.rs:71`) dead — apps no longer hold a credential to rotate. Both go,
along with `repo::rotate_app_key` (`repo.rs:878`) and the dashboard's
`rotateAppKey` (`lib/api/apps.ts:35`). `perm::ALL` therefore goes from 23 entries
to 27.

Preset assignment mirrors the existing app-level ladder, so the
Viewer ⊂ Developer ⊂ Admin ⊂ Owner invariant (`rbac.rs:499-508`) still holds:

| Role | env permissions |
|---|---|
| Owner | all five (`perm::ALL`) |
| Admin | all five (`ALL` minus `org:manage`) |
| Developer | `env:read`, `env:create`, `env:update`, `env:rotate_key` — **not** `env:delete`, mirroring how it holds `app:update` but not `app:delete` |
| Viewer | `env:read` only |

`env:read` ends in `:read`, so the "Viewer is read-only" test (`rbac.rs:455`)
continues to pass unmodified.

Three follow-on obligations:

- `ensure_preset_roles` re-syncs presets at every API boot (`rbac.rs:361`), so
  the new sets take effect without a data migration for system roles.
- **Custom roles need one.** Org-scoped roles (`is_system = false`) may carry
  `app:rotate_key` in their `permissions` JSONB. Left in place, the string
  matches nothing, but `check_no_escalation` still requires the *caller* to hold
  it — and nobody can, since it no longer exists — which would make that role
  permanently ungrantable. The migration must strip `app:rotate_key` from every
  `roles.permissions` array.
- The dashboard mirrors `perm::ALL` in the same order in
  `lib/models/permissions.ts:11-35`, with groups at `:43` and labels at `:63`.
  All three need updating, and `permissions.test.ts` asserts the parity.

## API

Env routes hang off `/v1/environments/{env_id}` at the top level, mirroring the
existing `/v1/grants/{grant_id}` shape. This needs one new repo function,
`env_ancestry(env_id) -> (app_id, project_id, org_id)` — the same shape as
`app_ancestry` (`repo.rs:898`), and exactly what S3's `authorize_env` will reuse.

| Route | Perm | Notes |
|---|---|---|
| `GET /v1/apps/{app_id}/environments?include_retired=<bool>` | `env:read` | returns `public_key`, `ingest_enabled`, `is_default`, `retired_at`; `include_retired` defaults to `false` |
| `POST /v1/apps/{app_id}/environments` | `env:create` | `{name}`; key generated server-side |
| `PATCH /v1/environments/{env_id}` | `env:update` | `{name?, ingest_enabled?, is_default?}`, all optional |
| `POST /v1/environments/{env_id}/rotate-key` | `env:rotate_key` | |
| `DELETE /v1/environments/{env_id}` | `env:delete` | retires: sets `retired_at`, forces `ingest_enabled = false` |

Returning `public_key` to `env:read` holders (which includes Viewer) continues
the precedent set by `GET /v1/apps/{id}`, which exposes the app-level key to the
same audience today. The key is documented as non-secret and write-only
(`ids.rs:18`, `wiki/Ingest-Wire-Contract.md:17`); it is an ingest credential, not
a read credential.

Because these are app-scoped resources, the checks still run through
`authorize_app` against the env's parent app — `env:*` describes *what* is being
managed, not a new scope level. Scope stays `(org, project, app)` until S3.

`PATCH` semantics:

- `is_default: true` promotes this env. It is a two-statement transaction (clear
  the app's old default, then set the new one), because the partial unique index
  rejects the naive order.
- `is_default: false` is rejected with `400`. A default is never unset, only
  moved, so that every app always has exactly one.
- A retired env accepts no `PATCH` and no `rotate-key` — both return `409`. The
  row exists to keep historical data attributable, not to be edited.

Cache invalidation follows the existing pattern of DELETEing the *old* key's
slot (`routes/apps.rs:81`): required on rotate, mute and retire, since
`ingest_enabled` is part of the cached value; not required on rename, since the
cache holds only ids.

## Dashboard

The `Environments` card in `pages/SettingsApp.svelte:210` becomes the management
surface: per-row DSN with `CopyButton`, mute toggle, rename, rotate, retire, a
default badge, a "New environment" button, and retired envs behind a collapsed
section. The app-level public key block and its "Regenerate public key" button
are deleted along with the column.

`buildDsn(publicKey, appId)` becomes `buildDsn(publicKey, envId)`
(`lib/utils/format.ts:68`). The backend discards that path segment
(`main.rs:174`), so emitting the env id costs nothing and makes the string
self-documenting. Its three callers — `Onboarding.svelte:34`,
`SettingsApp.svelte:45`, `Docs.svelte:19` — each switch to the relevant env.
Onboarding shows the seeded `dev` DSN; `first-event` polling is untouched.

Note the pre-existing fallback at `format.ts:76` emits `{base}/{key}/{appId}`,
which no SDK can parse (no userinfo `@`). It only fires on a malformed
`ingestBaseUrl`, but the env work touches this function and it is worth fixing
in passing.

The Events env-filter chip (`lib/components/filters/filters.ts:80`, options
loaded at `pages/Events.svelte:68`) stays as-is in S1; S2 retires it. Its options
will simply stop including retired envs.

## SDKs and docs

The `environment` init option and its envelope header field are removed from all
five SDKs (js, node, python, flutter, csharp) and from the three `examples/` apps
that set it.

Docs are corrected in the same pass. The wiki calls the DSN path segment
`<project_id>` (`wiki/Ingest-Wire-Contract.md:15`, `wiki/Getting-Started.md:22`)
when it has actually carried the *app* id all along — only
`bins/crebain/src/dsn.rs:4` gets it right today — and it now becomes the env id.

All five SDKs go to **`1.1.0`**, in lockstep. Four are at `1.0.0` today
(`sdks/js`, `sdks/node`, `sdks/python`, `sdks/flutter`); the C# SDK is at
`0.3.0` (`sdks/csharp/Sauron/Sauron.csproj`) and jumps to `1.1.0` with the rest
to bring it into line.

Note for the record: removing the `environment` option is a source-breaking
change for anyone who set it, so strict semver would call for a major bump. A
minor bump is the deliberate choice here — the removal is caught at compile time
in the typed SDKs and is inert in the untyped ones (the option is simply
ignored), and the DSN must be re-issued regardless, so no consumer can upgrade
without touching their init code anyway. The CHANGELOG entries must lead with
the breaking change rather than burying it under "Changed".

## Error handling

Retired envs are excluded from the key lookup, so a retired key falls through to
the existing `401 invalid_key` path — no new ingest error code, and the 30s
negative cache applies as normal. Redis behaviour is unchanged: rate limiting
still fails open (`main.rs:203`), and a cache miss falls through to Postgres.

| Condition | Response |
|---|---|
| Empty or >64-char name | `400` |
| `is_default: false` in a `PATCH` | `400` |
| Name collides with an active env | `409` |
| Retire the default env | `409` — promote another default first |
| Retire the last active env | `409` — an app always has somewhere to report |
| Make a retired env default | `409` |
| `PATCH` or `rotate-key` on a retired env | `409` |
| Active env cap reached | `409` |
| Unknown env id | `404` |
| Caller lacks the permission | `403` |

Retire is one-way in S1. The row is preserved, so un-retire is addable later,
but it needs a name-collision rule (a new env may have taken the name) that is
not worth designing now.

## Testing

Unit tests: key format, `env_ancestry`, the retire guards, the exactly-one-default
invariant, and name reuse after retire.

Permission tests: the existing preset invariants (`rbac.rs:425-508`) must still
pass with `perm::ALL` at 27 — the Viewer ⊂ Developer ⊂ Admin ⊂ Owner ladder,
Admin = `ALL` − `org:manage`, and Viewer being read-only. Add a case asserting
Developer holds `env:update` but not `env:delete`, and a dashboard test that
`ALL_PERMISSIONS` still matches `perm::ALL` exactly. The migration test must
confirm `app:rotate_key` is gone from every custom role's `permissions` array.

Ingest integration tests: key→env resolution, muted env vs muted app, retired
key, and that a stale envelope still carrying `environment` is ingested with the
key's env rather than the string's.

Migration test against a database seeded with existing apps, envs and events:
every env ends with a unique key, every app has exactly one default, apps with
no env get a seeded `dev`, and existing events keep their `environment_id`. The
default-selection rule gets its own cases — an app with `production` + `dev`
must default to `production`, an app with only `dev` + `staging` must default to
`dev`, and an app with neither must fall back to alphabetical.

Live end-to-end verification: create app → `dev` env and DSN appear → point an
example SDK at it → event lands with the right `environment_id` → mute → 403 →
rotate → old key 401, new key works → add a `prod` env → its own DSN → events
separate by env → retire → key dies, historical data still visible.

## Deployment

This drops `apps.public_key`, so it is a hard schema break — the old API binary
cannot run against the new schema. Given the known RPM gap where upgrades do not
re-run `sauron-migrate`, `sauron-migrate` must run before the new binaries start,
and this must be called out in the release notes.

Every deployed SDK stops reporting until its DSN is updated to an env key. This
is accepted and needs no transition window: there is no meaningful production
traffic on the current build yet, and integrators can swap their DSN whenever
they next deploy. No dual-key path is to be built.
