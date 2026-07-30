# Workflow Grouping — Design

**Date:** 2026-07-29
**Status:** Approved for planning

## Problem

Sauron groups telemetry by session (implicit, inferred from a `session_id` stamped on
items) and reconstructs multi-step behaviour on read via funnels and journeys. Neither
mechanism lets an app *declare* "the user is now doing X". So you cannot ask:

- How many users started checkout, and how many finished it?
- Which errors happen *during* onboarding, as opposed to anywhere in the session?
- How long does the signup flow take, and where do people drop it?

## Solution

A **workflow** is a named, explicitly-bounded span of activity inside one session,
opened and closed by the app through three SDK calls. Telemetry produced while a
workflow is open is attributed to it. The dashboard then reports outcomes, durations,
and contained events/issues per workflow name.

Workflows are **entirely optional**. `workflow_id` is nullable on every item and every
row. An app that never calls `startWorkflow` behaves exactly as it does today; the
workflow dashboard surfaces are simply empty.

## Locked decisions

1. **Session-scoped.** At most one workflow is active per session at a time.
2. **Non-throwing, status-returning.** Every call returns a status enum. Telemetry never
   throws into host app code.
3. **Reserved events + stamped id.** No new `EnvelopeItem` variant. Lifecycle travels as
   three reserved analytics events; attribution travels as two nullable fields stamped
   on existing items — exactly like the existing `screen` field.
4. **Abandonment is derived on read.** No sweeper job, no durable SDK state.
5. **Server SDKs bind the workflow to their scope primitive**, not to a module global.
6. All four dashboard surfaces are in v1: list page, detail page, filter chip, timeline
   lane.

## SDK API

Three calls, present in all five SDKs, named per-language convention
(`startWorkflow` / `start_workflow` / `StartWorkflow`).

| Call | Behaviour |
|---|---|
| `startWorkflow(name, { force })` | Opens a workflow. Returns `{ status, workflowId? }`. |
| `endWorkflow(name?)` | Closes the active workflow with outcome `completed`. |
| `cancelWorkflow(name?, { reason })` | Closes the active workflow with outcome `cancelled`. |

`getWorkflow()` returns the active workflow (`{ workflowId, name, startedAt }`) or null.

### Status enum

`ok` · `already_active` · `not_active` · `name_mismatch` · `invalid_name` · `disabled`

- `startWorkflow` while one is active and `force` is falsy → `already_active`; nothing
  changes, a debug-logger warning is emitted.
- `startWorkflow` with `force: true` → the active workflow is cancelled with
  `reason: "superseded"`, then the new one starts. Returns `ok`.
- `endWorkflow`/`cancelWorkflow` with no active workflow → `not_active`, no-op.
- The optional `name` on end/cancel is a **guard**: if supplied and it does not match the
  active workflow's name, the call is a no-op returning `name_mismatch`, so a stale
  `endWorkflow("onboarding")` cannot close an unrelated `"checkout"` span.
- `name` is trimmed and validated: non-empty, ≤ 120 chars. Invalid → `invalid_name`,
  no workflow started.
- Called before `init` / after `close` → `disabled`.

No call throws. Internal failures are caught and routed to the SDK's existing debug
logger.

### State placement

| SDK | Where the active workflow lives | Precedent |
|---|---|---|
| js | module-level state in `src/workflow.ts` | `src/screen.ts` |
| flutter | private field on `SauronClient` | `_currentScreen` in `lib/src/client.dart` |
| node | field on `Scope`, applied in `applyToEvent` | `src/scope.ts` (`AsyncLocalStorage`) |
| python | field on `Scope`, applied in `apply_to_event` | `sauron/_scope.py` (`ContextVar`) |
| csharp | field on `Scope`, applied in `ApplyToEvent` | `Sauron/Scope.cs` (`AsyncLocal`) |

The three server SDKs must **not** use a module global: concurrent requests would
cross-contaminate. This is the same reason they never implemented `setScreen`. Their
workflows are therefore request-isolated, and — since those SDKs have no session id —
their workflow rows carry `session_id = null`. The rollup keys on
`(app_id, workflow_id)`, so this works unchanged.

## Wire contract

### Lifecycle: three reserved analytics events

Emitted as ordinary `event` items, so no envelope schema change is needed for them:

| Event name | Properties |
|---|---|
| `$workflow_start` | `workflow_id`, `workflow_name` |
| `$workflow_end` | `workflow_id`, `workflow_name`, `duration_ms` |
| `$workflow_cancel` | `workflow_id`, `workflow_name`, `duration_ms`, `reason` |

`reason` is one of `superseded`, `user` (explicit cancel with no reason), or a
caller-supplied string capped at 120 chars. `$`-prefixed names follow the existing
`$screen` convention emitted by `setScreen`.

### Attribution: two stamped fields

`workflow_id` (uuid string, nullable) and `workflow_name` (text, nullable) are added to:

- `ErrorItem`
- `AnalyticsItem` (event)
- `TransactionItem`

They are omitted from the JSON when null. `BreadcrumbBatch` is **not** stamped —
breadcrumbs are a ring buffer attached to the next captured event, which already carries
the attribution.

The Rust definitions in `backend/crates/sauron-core/src/envelope.rs` are the source of
truth; the four duplicated SDK type definitions and five golden fixtures mirror them.

## Storage

### New table `workflows` (not partitioned, mirrors `sessions`)

| Column | Type | Notes |
|---|---|---|
| `id` | uuid pk | |
| `app_id` | uuid not null | |
| `environment_id` | uuid not null | |
| `workflow_id` | text not null | client-generated uuid; `UNIQUE (app_id, workflow_id)` |
| `name` | text not null | |
| `session_id` | text null | null for server SDKs |
| `distinct_id` | text null | |
| `device_key` | text null | |
| `release` | text null | |
| `status` | text not null default `'active'` | `active` \| `completed` \| `cancelled` |
| `cancel_reason` | text null | |
| `started_at` | timestamptz not null | |
| `ended_at` | timestamptz null | |
| `last_event_at` | timestamptz not null | |
| `events_count` | integer not null default 0 | |
| `errors_count` | integer not null default 0 | |
| `created_at` / `updated_at` | timestamptz not null | |

Indexes: `(app_id, name, started_at DESC)`, `(app_id, status, last_event_at DESC)`,
`(app_id, session_id)`.

### Stamped columns

`workflow_id text null` and `workflow_name text null` added to `analytics_events`,
`error_events`, and `transactions`. Partial indexes
`(app_id, workflow_name, occurred_at DESC) WHERE workflow_id IS NOT NULL` keep the cost
off apps that never use the feature.

### Pipeline

`repo::bump_workflow(app_id, workflow_id, …, events_delta, errors_delta)` mirrors
`bump_session` exactly: `INSERT … ON CONFLICT (app_id, workflow_id) DO UPDATE` with
`GREATEST(last_event_at)`, `LEAST(started_at)`, and counter increments. Called from
`process.rs` for any item that carries a `workflow_id`, with the same
`(events, errors)` deltas as the session bump.

Lifecycle events additionally transition status:

- `$workflow_start` → upsert row with `status='active'`, `started_at = occurred_at`
- `$workflow_end` → `status='completed'`, `ended_at = occurred_at`
- `$workflow_cancel` → `status='cancelled'`, `ended_at = occurred_at`,
  `cancel_reason = properties.reason`

A terminal status is never overwritten by a later `active` upsert (out-of-order arrival
is normal), and `$workflow_start` arriving after a terminal event only backfills
`started_at`/`name`.

### Abandonment

Derived, never stored. A row with `status = 'active'` and
`last_event_at < now() - interval '30 minutes'` is reported as **abandoned**. The
threshold matches the existing breadcrumb-buffer TTL and lives as one constant in the
repo layer.

## API

All under `/v1/apps/{app_id}/`, guarded by `authorize_app(..., perm::EVENT_READ)` and
environment-scoped via `ReadScope` — matching `sessions.rs` and `screens.rs`. The
dashboard's axios interceptor auto-injects `environment_id` for any
`/v1/apps/{id}/…` URL, so these routes must **accept** it, not reject it.

| Endpoint | Returns |
|---|---|
| `GET /workflows?since_days&search&limit&offset` | one row per workflow **name**: started, completed, cancelled, abandoned, completion rate, median/p95 duration, unique users, last seen |
| `GET /workflows/{name}?since_days` | outcome breakdown, duration histogram, top contained events, top contained issues, unique users |
| `GET /workflows/{name}/runs?since_days&status&limit&offset` | individual runs (workflow_id, session_id, distinct_id, status, duration, counts) linking to session detail |
| `GET /sessions/{session_id}/workflows` | workflow spans within one session, for the timeline lane |

Issues and Events list endpoints gain a `workflow` filter field, predicating on
`workflow_name` through the existing filter pipeline.

## Dashboard

Plain Vite + Svelte 5 runes + `svelte-spa-router`, no charting library — hand-rolled
components only.

1. **Workflows list** (`#/workflows`) — `StatTiles` summary row + `DataTable` of names
   with outcome columns and completion rate, `SearchInput` + `DateRange`, `Pagination`.
   Sidebar entry in the Explore group, `workflow` Lucide icon added to the registry.
2. **Workflow detail** (`#/workflows/:name`) — `FunnelChart` for the outcome breakdown,
   `DurationHistogram`, `BarList` of contained events, a table of top issues, and a
   recent-runs table linking to `#/sessions/:id`.
3. **Filter chip** — `workflow` added to `ISSUE_FIELDS` and `EVENT_FIELDS`, flowing
   through the existing `encodeFilters`/`parseFilters` codec unchanged.
4. **Timeline lane** — new `TimelineItem` variant `{ kind: 'workflow' }` handled by the
   four dispatch helpers in `Timeline.svelte`, rendered as a distinct band so you can see
   which items fell inside which workflow.

Every page reads `sessionStore.currentAppId` and touches `sessionStore.scopeKey` inside
its `$effect` so it refetches on environment switch.

## Testing

- **Rust:** golden envelope fixture updated for the two new fields; `sauron-db` tests for
  `bump_workflow` upsert semantics, terminal-status protection, and out-of-order arrival;
  `sauron-api` http tests for the four endpoints including env-scoping isolation.
- **SDKs:** per-SDK unit tests cloning the existing `screen` test (js `test/screen.test.ts`,
  flutter `test/screen_test.dart`) — start/end/cancel happy paths, `already_active`,
  `force`, `name_mismatch`, `not_active`, `invalid_name`, and that the stamp appears on
  captured items. Server SDKs additionally test scope isolation across concurrent
  contexts. All five golden fixtures stay byte-identical to the Rust one.
- **Dashboard:** vitest unit tests for the `WORKFLOW_FIELDS` filter round-trip and the
  row-shaping helpers (there is no jsdom, so keep logic in `.ts` modules); `npm run check`
  must pass with no unused imports.

## Out of scope

- Nested or concurrent workflows.
- Cross-session / cross-device workflows.
- Server-side workflow definitions, expected-step validation, or alerting on completion
  rate.
- Durable SDK persistence across app restarts.
- Backfilling workflow attribution onto historical events.

## Versioning

SDK minor bumps, in both the manifest and the in-code constant per
`sdks/PUBLISHING.md`: js/node/python/csharp `1.2.0 → 1.3.0`, flutter `1.3.0 → 1.4.0`.
`wiki/Capabilities.md` gains a workflow row (and its stale "v0.3.0" header is corrected).
