# Design: Screen-level analytics

**Date:** 2026-07-14
**Status:** Approved (brainstorming) — pending spec review
**Area:** `sdks/js` + `sdks/flutter` + `backend/` (migration, core envelope, db, pipeline, api) + `dashboard/`

## Goal

Let the SDKs optionally attach a **screen name** to every event and exception, then surface **per-screen analytics** in the dashboard: how long users stayed on a screen (dwell), and how many views / events / users / exceptions each screen has. Make an event's or exception's screen **clickable**, linking to that screen's detail.

## Decisions (settled in brainstorming)

- **Dwell model = label + auto screen-view.** The SDK stamps the current `screen` on every event/exception **and** auto-emits a lightweight screen-view event (reserved name **`$screen`**) whenever the screen changes. Dwell is computed on read from the event timeline; the `$screen` entries give accurate per-screen entry timestamps.
- **Capture = manual + opt-in auto.** `setScreen(name)` / a `screen` init option always works (framework-agnostic). Auto-detection is **off by default** (like `performance`): web reuses the existing History-API navigation hook; Flutter ships a `SauronNavigatorObserver`.
- **New "Screens" section** in the dashboard (not folded into an existing screen).
- Reserved screen-view event name is **`$screen`**.

## SDK design

Both SDKs follow their existing `session_id` / `device_id` implementation (persist/attach pattern). Bump each SDK a minor version.

### JS — `@sauron/browser`
- **State:** module-level `currentScreen: string | null`, initialized from `init({ screen })`. Public API: `setScreen(name: string)`, `getScreen(): string | null` (exported from `index.ts`).
- **Attach:** the event/error builders (in `client.ts`/`scope.ts`, wherever `session_id`/`distinct_id` are attached) set `screen: currentScreen` on outgoing `AnalyticsItem` and `ErrorItem`. A per-call `{ screen }` on `capture`/`captureException` overrides the current screen for that one payload.
- **Auto screen-view:** `setScreen(name)` — when `name !== currentScreen` — updates `currentScreen` then captures an analytics event named `$screen` with `properties: { screen: name }` (so it flows through the normal transport and carries the new screen).
- **Opt-in auto-detection:** new init option `screenTracking?: boolean` (default `false`). When `true`, reuse the History-API hook already installed by the performance integration (`integrations/performance.ts`) to call `setScreen(pathOf(location.pathname))` on `pushState`/`replaceState`/`popstate`. When `performance:false` and `screenTracking:true`, install a minimal standalone History hook (extract the existing patch into a shared helper so both features share it).
- **Types:** add `screen?: string | null` to `AnalyticsItem` + `ErrorItem` in `types.ts`; add `screen?: string` and `screenTracking?: boolean` to the init-options type.

### Flutter — `sauron_flutter`
- **State + API:** a `currentScreen` on the client; `Sauron.setScreen(String name)` / `Sauron.screen` getter; `screen:` option on init. Attach `screen` to analytics + error payloads where `sessionId` is attached; per-call `screen:` override on `capture` / `captureException`.
- **Auto screen-view:** `setScreen` emits a `$screen` analytics event on change (same semantics as JS).
- **Opt-in auto-detection:** a `SauronNavigatorObserver extends NavigatorObserver` that calls `setScreen(route.settings.name ?? …)` on `didPush`/`didPop`; documented as opt-in (the app adds it to `MaterialApp.navigatorObservers`). No auto-install.

## Data model (mirror `session_id` / `device_key` exactly)

- **Migration `backend/migrations/2026-07-14-000007_events_screen/{up,down}.sql`:**
  ```sql
  -- up.sql
  ALTER TABLE analytics_events ADD COLUMN screen TEXT;
  ALTER TABLE error_events     ADD COLUMN screen TEXT;
  CREATE INDEX analytics_events_app_screen_idx ON analytics_events (app_id, screen);
  CREATE INDEX error_events_app_screen_idx     ON error_events (app_id, screen);
  -- down.sql
  DROP INDEX IF EXISTS analytics_events_app_screen_idx;
  DROP INDEX IF EXISTS error_events_app_screen_idx;
  ALTER TABLE analytics_events DROP COLUMN IF EXISTS screen;
  ALTER TABLE error_events     DROP COLUMN IF EXISTS screen;
  ```
- **Diesel schema (`schema.rs`):** add `screen -> Nullable<Text>` to both `analytics_events` and `error_events`.
- **Core envelope (`sauron-core/src/envelope.rs`):** add `pub screen: Option<String>` to `AnalyticsItem` and `ErrorItem` (defaulting to `None` via `#[serde(default)]`), beside their `session_id`.
- **DB models (`sauron-db/src/models.rs`):** add `screen: Option<String>` to `NewAnalyticsEvent` and `NewErrorEvent` (beside `session_id`/`device_key`).
- **Pipeline (`sauron-pipeline/src/process.rs`):** thread `ev.screen` into the `NewAnalyticsEvent`/`NewErrorEvent` builders (mirror `session_id`). No rollup change required for MVP.

## Metrics & queries (on-read — like funnel/journeys/percentiles)

All app-scoped, range-scoped by `since_days` (clamped 1..365), guarded by `authorize_app(..., EVENT_READ)`.

**Per-screen definitions:**
- **Views** = count of `analytics_events WHERE name = '$screen'` for the screen.
- **Events** = count of `analytics_events WHERE name <> '$screen'` for the screen.
- **Exceptions** = count of `error_events` for the screen.
- **Users** = `COUNT(DISTINCT distinct_id)` across analytics + errors for the screen (non-null/non-empty).
- **Dwell:** within each session, order `analytics_events` by `occurred_at`, take `gap = LEAD(occurred_at) OVER (PARTITION BY session_id ORDER BY occurred_at) - occurred_at`, and attribute each gap to the **current row's `screen`**. Sum per screen = `total_dwell_ms`; **`avg_dwell_ms = total_dwell_ms / NULLIF(views, 0)`**. Cap each gap at **30 min (1,800,000 ms)** so an idle/backgrounded tab can't inflate dwell; ignore rows with null `session_id` or null `screen`.

**Endpoints (new `routes/screens.rs`):**
- `GET /v1/apps/{app_id}/screens?since_days=N&q=&limit=&offset=` → `Vec<ScreenRow { screen, views, events, exceptions, users, avg_dwell_ms }>`, ordered by `views DESC`. Optional `q` does a case-insensitive `screen ILIKE` filter (escaped via the existing `like_contains` helper).
- `GET /v1/apps/{app_id}/screens/detail?name=X&since_days=N` → `ScreenDetail { stats: ScreenStats { screen, views, events, exceptions, users, avg_dwell_ms, total_dwell_ms }, recent_events: Vec<…>, recent_exceptions: Vec<…> }`. `recent_events`/`recent_exceptions` reuse the existing analytics-event / error-event list row shapes, filtered to `screen = X`, newest first, limited to ~20 each.

Repo functions in `sauron-db`: `screen_list`, `screen_stats`, plus reuse of existing event/error list queries with a `screen` filter (add an optional `screen: Option<&str>` parameter to those, or dedicated `recent_events_for_screen` / `recent_exceptions_for_screen`). Extract the dwell/gap SQL once and share between `screen_list` and `screen_stats`.

## Dashboard

- **Nav:** add a **Screens** item under the "Explore" group (alongside Sessions/Users/Devices) in the app-shell nav config.
- **Routes (`routes.ts`):** `'/screens'` → `ScreensList`, `'/screens/:name'` → `ScreenDetail` (both `guarded`). Screen names are arbitrary (route paths, labels) → encode with `encodeURIComponent`, exactly like `/persons/:distinctId`.
- **`ScreensList.svelte`:** a `DateRange` + `SearchInput` + `DataTable` of `ScreenRow`s (Screen, Views, Events, Exceptions, Users, Avg dwell via `formatDuration`), `Pagination`. Row click → `push('/screens/' + encodeURIComponent(screen))`. Mirrors `DevicesInventory.svelte`/`UsersExplorer.svelte`.
- **`ScreenDetail.svelte`:** `StatTiles` (Views, Users, Events, Exceptions, Avg dwell, Total dwell) + a "Recent events" list and a "Recent exceptions" list (each row links to the underlying event/issue). Mirrors `DeviceDetail.svelte`/`PersonProfile.svelte`.
- **Clickthrough (the "relate event/exception to a screen" ask):** wherever an event's or error's `screen` is shown — Event detail (Events explorer row/drawer), Issue detail, and the error-event view — render the screen as a link to `#/screens/{encodeURIComponent(screen)}`. Show nothing when `screen` is null.
- **Events explorer:** exclude `name = '$screen'` rows by default so synthetic screen-views don't clutter the event stream (filter in `list_analytics_events`, or add `AND name <> '$screen'` unless a future "show screen-views" toggle is set). Screen-views remain counted as **Views** on the Screens section.
- **Models/client:** `ScreenRow`, `ScreenStats`, `ScreenDetail` in `models/index.ts`; `api/screens.ts` with `listScreens(appId, {q, sinceDays, limit, offset})` + `getScreenDetail(appId, name, sinceDays)`.

## Testing

- **JS SDK (vitest):** `setScreen` stamps `screen` on subsequent events + exceptions; emits a `$screen` event only on change (not on same-name); per-call `{ screen }` overrides; `screenTracking:false` installs no history hook; a `screen` init option seeds the first screen. Follow the existing `session_id` SDK tests.
- **Flutter SDK:** analogous tests + a `SauronNavigatorObserver` test (didPush → setScreen → `$screen` emitted).
- **Backend:** extract dwell-gap capping / avg-dwell math into a pure helper and unit-test it (gap cap, `views=0` → 0 avg, attribution to the earlier screen). SQL/handlers verified **end-to-end via compose** (per the project convention — no DB test harness): seed events across two screens with a `$screen` entry each, assert views/events/exceptions/users and a sane avg dwell; assert the detail endpoint filters by screen.
- **Dashboard:** `svelte-check` + `vite build`; e2e — Screens list populates, row → detail, an event's screen link lands on the right detail, `$screen` rows absent from the Events explorer.

## Scope guardrails (YAGNI — out)

- No screen replay / screenshots.
- Dwell is event-gap based — **no** heartbeat/visibility pings.
- Auto-detection limited to web **History API** + Flutter **NavigatorObserver** — no React Router / Vue Router / Next / etc. adapters.
- No new envelope item type and **no** `screens` rollup table — `$screen` is a normal analytics event; all screen metrics are on-read (a rollup is the documented scale-up path if reads get slow).
- No change to sessions/devices rollups.

## Files touched (summary)

- **JS SDK:** `sdks/js/src/{types.ts, index.ts, client.ts (or scope.ts), identity.ts or a new screen.ts, integrations/performance.ts}` + tests; version bump.
- **Flutter SDK:** client + a `SauronNavigatorObserver` + tests; version bump.
- **Backend:** `migrations/2026-07-14-000007_events_screen/{up,down}.sql`; `sauron-db/src/schema.rs`, `models.rs`, `repo.rs`; `sauron-core/src/envelope.rs`; `sauron-pipeline/src/process.rs`; `sauron-api/src/routes/screens.rs` (new) + `main.rs` (routes) + `list_analytics_events` `$screen` exclusion.
- **Dashboard:** `models/index.ts`; `api/screens.ts` (new); `pages/ScreensList.svelte` + `ScreenDetail.svelte` (new); `routes.ts`; nav config; screen-link additions in Event detail / Issue detail / error-event view; Events list default filter.
