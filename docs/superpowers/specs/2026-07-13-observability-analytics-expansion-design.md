# Sauron — Observability + Analytics Dashboard Expansion

**Date:** 2026-07-13
**Status:** Approved for full-stack build ("capture everything")
**Owner:** freelance / splimter

## Goal

Grow Sauron from the shipped wedge (error→issue + track/identify analytics + person
profile) into a developer-first observability & product-analytics suite: 12 interconnected
screens backed by real data. Everything is app-scoped (`Org → Project → App → signals`),
enforced with the existing RBAC (`authorize_app(conn, user_id, app_id, perm)`).

Cross-cutting UX: dense Linear-style tables, Datadog-style stat tiles, color-coded latency,
expandable JSON, and cross-entity links (issue↔user↔device↔session↔event everywhere).

## Slice plan (each is migration + pipeline + API + dashboard, ordered by dependency)

| # | Slice | Screens |
|---|-------|---------|
| 1 | Sessions | Sessions List, **Session Details** (flagship timeline) |
| 2 | Devices | Devices Inventory, Device Details |
| 3 | Users + Overview | Users Explorer, User Details (enhance), Overview Dashboard |
| 4 | Exceptions + Event Explorer | Exceptions Dashboard (enhance Issues), Event Explorer (enhance Events) |
| 5 | Funnel Builder | Funnel Builder |
| 6 | Journey Explorer | Journey Explorer (step-indexed Sankey) |
| 7 | Performance | Performance Monitoring (+ SDK/ingest capture) |

## Data model additions (Postgres, via diesel migrations)

Conventions reused: `TEXT + CHECK` enum-like columns (map to `String`), `Jsonb` for
free-form context, uuid_v7 PKs, `on_conflict` upserts in `repo.rs`, `date_trunc` series.

### Migration `2026-07-13-000004_sessions_devices`
```
sessions (
  id uuid pk,
  app_id uuid not null,
  session_id text not null,             -- SDK-provided
  distinct_id text,                     -- person, if known
  device_key text,                      -- links to devices.device_key
  started_at timestamptz not null,
  last_event_at timestamptz not null,
  events_count bigint not null default 0,
  errors_count bigint not null default 0,
  context jsonb not null default '{}',  -- snapshot: device/os/ua/app/runtime
  release text, environment_id uuid, ip_address text,
  created_at timestamptz default now(), updated_at timestamptz default now(),
  unique (app_id, session_id)
)  -- indexes: (app_id, last_event_at desc), (app_id, distinct_id), (app_id, device_key)

devices (
  id uuid pk,
  app_id uuid not null,
  device_key text not null,             -- stable: device_id (SDK install id) or hash(family|model|os)
  family text, model text, os_name text, os_version text, arch text, browser text,
  last_distinct_id text,
  first_seen timestamptz not null, last_seen timestamptz not null,
  events_count bigint default 0, errors_count bigint default 0, sessions_count bigint default 0,
  created_at, updated_at,
  unique (app_id, device_key)
)  -- indexes: (app_id, last_seen desc)
```
Also: `ALTER TABLE error_events ADD COLUMN session_id text;` (+ index (app_id, session_id))
and `ALTER TABLE analytics_events ADD COLUMN device_key text;` — so both signal streams
join into sessions/devices and the flagship timeline.

### Migration `2026-07-13-000005_transactions`
```
transactions (
  id uuid pk,
  app_id uuid not null, environment_id uuid,
  name text not null,                   -- route/screen/operation label
  op text not null,                     -- 'navigation'|'http'|'resource'|'screen_load'|'custom'
  duration_ms double precision not null,
  status text,                          -- 'ok'|'error'|http status class
  http_method text, http_status integer, url text,
  distinct_id text, session_id text, device_key text,
  release text, ip_address text,
  occurred_at timestamptz not null, received_at timestamptz default now()
)  -- indexes: (app_id, occurred_at desc), (app_id, op, name)
```

## Envelope / ingest changes (`sauron-core::envelope`)

- `ErrorItem`: add `#[serde(default)] session_id: Option<String>`.
- New `EnvelopeItem::Transaction(TransactionItem)` with tag `"transaction"`:
  `{ name, op, duration_ms: f64, status?, http_method?, http_status?, url?, distinct_id?, session_id?, timestamp }`.
- Golden envelope test + both SDK golden fixtures updated in lockstep (keep parity).

Pipeline (`process.rs`):
- `process_event`: after insert, `upsert_session` (if session_id) and `upsert_device`
  (from enriched context + device_key), bumping counts/last_event_at.
- `process_error`: same, incrementing `errors_count`; persist `error_events.session_id`.
- new `process_transaction`: insert row + upsert_session/device counters.
- `enrich_context`: also compute `device_key` (prefer `context.device.device_id`, else a
  stable hash of family|model|os_name|arch) and stamp `context.device_key`.

## API endpoints (all `authorize_app` with the noted permission)

New permission strings reuse existing ones: `event:read` for analytics/sessions/devices/
performance reads; `issue:read` for exceptions. No new perms needed for reads.

```
GET  /v1/apps/{app_id}/sessions            ?since_days&limit&offset&distinct_id&device_key   -> SessionRow[]
GET  /v1/apps/{app_id}/sessions/{sid}      -> { session, timeline: TimelineItem[] }
GET  /v1/apps/{app_id}/devices             ?since_days&limit&offset&search                    -> DeviceRow[]
GET  /v1/apps/{app_id}/devices/{key}       -> { device, sessions: SessionRow[], errors: ErrorEvent[], perf: PerfSummaryRow[] }
GET  /v1/apps/{app_id}/persons             ?search&limit&offset                               -> PersonRow[]  (event_user + counts)
GET  /v1/apps/{app_id}/overview            ?since_days                                        -> Overview
GET  /v1/apps/{app_id}/issues/stats        ?since_days                                        -> IssueStats
GET  /v1/apps/{app_id}/events/list         ?search&name&distinct_id&session_id&limit&offset   -> AnalyticsEvent[]
POST /v1/apps/{app_id}/funnel              body {steps:string[], since_days, breakdown?}       -> Funnel
GET  /v1/apps/{app_id}/journeys            ?since_days&start_event&depth                       -> Journey (step-indexed Sankey)
GET  /v1/apps/{app_id}/performance/summary ?since_days&op                                      -> PerfSummaryRow[]  (p50/p75/p95/p99/avg/count/error_rate per name)
GET  /v1/apps/{app_id}/performance/series  ?since_days&name&op                                 -> PerfSeriesPoint[] (bucket, p50, p95, throughput)
```

Response shape sketches (JSON; serde-serialized structs):
- `SessionRow`: session row + `duration_ms` (last_event_at-started_at) computed.
- `TimelineItem`: `{ kind: 'event'|'error'|'transaction', at, ...payload }` sorted by `at`.
- `PersonRow`: `{ distinct_id, properties, first_seen, last_seen, events_count, errors_count, sessions_count }`.
- `Overview`: `{ totals:{events,errors,sessions,users,new_users}, error_rate, crash_free_sessions,
   events_series:SeriesPoint[], errors_series:SeriesPoint[], top_issues:Issue[], top_events:EventCount[] }`.
- `IssueStats`: `{ total, by_status:{unresolved,resolved,ignored}, by_level:{...}, series:SeriesPoint[] }`.
- `Funnel`: `{ total_entered, steps:[{name, count, conv_from_start, conv_from_prev}] }`.
- `Journey`: `{ steps:[[{event,count}]], links:[{from_step,from_event,to_step,to_event,count}] }`.
- `PerfSummaryRow`: `{ name, op, count, p50, p75, p95, p99, avg, error_rate }` (percentile_cont in SQL).

## Frontend

### Foundation (build first, shared by every screen)
- **Models** (`lib/models/index.ts`): add all shapes above.
- **API modules** (`lib/api/`): `sessions.ts`, `devices.ts`, `overview.ts`, `funnels.ts`,
  `journeys.ts`, `performance.ts`; extend `events.ts` (list) and `persons.ts` (list) and `issues.ts` (stats).
- **Shared components** (`lib/components/`):
  - `DataTable.svelte` — dense, sortable, hoverable rows, optional row-click nav (Linear style).
  - `StatTile.svelte` — Datadog-style KPI (label, value, delta, spark). `StatTileRow` grid wrapper.
  - `LatencyBadge.svelte` — color-codes ms (green <300, amber <1000, red ≥1000; configurable).
  - `JsonTree.svelte` — collapsible JSON viewer (expandable deep inspection).
  - `Timeline.svelte` — vertical event timeline with icon rail, latency chips, expandable JSON.
  - `FunnelChart.svelte` — horizontal step bars with drop-off %.
  - `SankeyChart.svelte` — SVG step-indexed Sankey (nodes per step, weighted links).
  - `Sparkline.svelte` — inline mini line/area.
  - `Distribution.svelte` — latency histogram bars.
  - `DateRange.svelte` — 24h/7d/30d/90d segmented control (reuse Events `.ranges` style).
  - `SearchInput.svelte`, `Pagination.svelte`.
- **Routing** (`routes.ts`) + **Sidebar** grouped nav:
  - Monitor: Overview `#/overview`, Issues `#/issues`, Performance `#/performance`
  - Explore: Events `#/events`, Sessions `#/sessions`, Users `#/users`, Devices `#/devices`
  - Analyze: Funnels `#/funnels`, Journeys `#/journeys`
  - Settings/Members/Projects unchanged.
  - New routes: `#/overview`, `#/sessions`, `#/sessions/:id`, `#/users`, `#/devices`,
    `#/devices/:key`, `#/performance`, `#/funnels`, `#/journeys`. Keep `#/persons/:distinctId`.

### Screens (each its own page file, built against the contract)
Overview, SessionsList, SessionDetail (flagship), DevicesInventory, DeviceDetail,
UsersExplorer, (enhance) PersonProfile, (enhance) Issues→Exceptions, (enhance) Events→EventExplorer,
FunnelBuilder, JourneyExplorer, Performance. Every entity id/link routes to its detail page.

## SDK changes (slice 7 + supporting)
- **JS** (`@sauron/browser`): persistent `device_id` (localStorage) into `context.device.device_id`;
  attach current `session_id` to error items; performance module — PerformanceNavigationTiming
  (page load), `fetch`/XHR wrap (http latency), route-change navigation → `transaction` items.
- **Flutter**: persistent `device_id`; `SauronPerformance` API (`trackTransaction`, screen-load
  timing via navigator observer); attach session_id to errors.
- Keep the golden envelope fixtures in all three languages in sync.

## Build sequence & verification
1. Backend data layer (migrations, schema.rs, models, repo, envelope, pipeline) → `cargo check`.
2. Backend handlers + `main.rs` wiring → `cargo build`.
3. Frontend foundation (models, api, shared components, routes, sidebar) → `svelte-check`.
4. Frontend screens (parallel sub-agents, disjoint page files) → `svelte-check` + `vite build`.
5. SDK perf instrumentation (JS + Flutter) → package tests.
6. End-to-end: `docker compose up`, seed signals, smoke-test endpoints + screens.

## Non-goals (unchanged scope boundary)
No session replay/video, no ClickHouse/Kafka/object storage (Postgres+Redis only), no
symbolication, no SSO/billing. Sessions/devices/perf are derived+materialized from the
existing ingest path, not a new storage tier.
```
