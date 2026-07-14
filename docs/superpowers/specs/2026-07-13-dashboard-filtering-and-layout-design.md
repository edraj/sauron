# Dashboard: Grafana-style filtering + full-width layout — design

**Date:** 2026-07-13
**Status:** Approved (design), pending implementation plan
**Scope:** `dashboard/` (Svelte) + `backend/` (Rust) — the **Issues** and **Events** screens, plus an app-wide width change.

## Goal

Two connected asks:

1. **More space for details.** The content area is capped at `--content-max: 1240px` and centered, wasting space on wide monitors and making tables/detail views feel cramped. Go full-width and let detail pages use the reclaimed room.
2. **Filter Issues and Events "like Grafana."** Today Issues has only 4 status tabs + a date range, and Events has a date range + click-a-top-event + one search box. Replace both with a composable, ad-hoc filter bar.

## Locked decisions (from brainstorming)

- **Layout:** full-width (remove the 1240px cap); keep today's click-through to **dedicated detail pages** (no split-pane/drawer), made roomier.
- **Filter model:** **Grafana-style ad-hoc filter chips** — each filter is `field + operator + value`, AND-combined, shown as removable chips, alongside a free-text search and the time range.
- **Filter depth:** **curated fields only** — enum fields (level, status, environment) offer a value dropdown; string/number fields are free-type. Enum + operator per field type.

## Non-goals (explicit follow-ups, not this cut)

- Arbitrary `properties.*` (events) / `tags.*` (errors) filtering.
- A value-autocomplete / key-discovery endpoint over JSONB.
- Environment/release filtering **on Issues** (the `issues` table has no `environment`/`release` columns — those live on `error_events`; it would need an events join).
- Extending the filter bar to Sessions/Users/Devices (the component is built reusable so this is easy later, but it is out of scope here).
- OR-groups / nested boolean logic — filters are a flat AND list.

---

## Architecture overview

A reusable **`FilterBar`** component driven by a per-page **field registry**. Filter state lives in the **URL query** (shareable, survives reload) and is sent to the backend as repeated **`filter=field:op:value`** params, validated server-side against a **per-resource whitelist** and applied through diesel's boxed query builder (typed, parameterized — no raw SQL from user input).

```
FilterBar (chips + add-popover + search + DateRange)
   │  emits Filter[] + q + since_days
   ▼
page (Issues.svelte / Events.svelte)
   │  syncs state ⇄ URL querystring
   ▼
api module  ── filter=level:eq:error & filter=status:neq:resolved & q=… & since_days=30 ──▶
backend route  → parse+whitelist → repo list_* (boxed builder .filter() per field/op)
```

---

## Section A — Full-width layout

**File:** `dashboard/src/app.css`

- Raise `--content-max` from `1240px` to `~2200px` — a sane ceiling that protects ultra-wide monitors from absurd line lengths while letting typical 1440–1920px screens run effectively full-width. The `.content-inner` wrapper keeps `margin-inline: auto` + existing side padding (~24px).
- This is app-wide; every screen gains width. Verify no page assumed the narrow column (spot-check Overview, Settings, Onboarding, which use their own inner `max-width` and are unaffected).

## Section B — FilterBar (frontend)

**New files:** `dashboard/src/lib/components/filters/FilterBar.svelte`, `dashboard/src/lib/components/filters/filters.ts`.

### `filters.ts` — model + codec (pure, testable)

```ts
export type Op = 'eq' | 'neq' | 'contains' | 'gt' | 'lt';
export type FieldType = 'enum' | 'string' | 'number';

export interface FieldDef {
  key: string;              // wire + column key, e.g. 'level'
  label: string;            // UI label, e.g. 'Level'
  type: FieldType;
  ops: Op[];                // allowed operators for this field
  options?: string[];       // enum values (for type 'enum')
}

export interface Filter { field: string; op: Op; value: string; }

// Codec: URL/query <-> Filter[]. One repeated `filter` param per filter,
// value URL-encoded; parse splits on the first two ':' only.
export function encodeFilters(filters: Filter[]): string[];   // -> ["level:eq:error", ...]
export function parseFilters(raw: string[], fields: FieldDef[]): Filter[]; // drops unknown field/op
export const OP_LABEL: Record<Op, string>; // '=', '≠', 'contains', '>', '<'
```

### Field registries

`ISSUE_FIELDS`:
| key | label | type | ops | options |
|-----|-------|------|-----|---------|
| level | Level | enum | eq, neq | debug, info, warning, error, fatal |
| status | Status | enum | eq, neq | unresolved, resolved, ignored |
| type | Type | string | eq, neq, contains | — |
| culprit | Culprit | string | eq, neq, contains | — |
| times_seen | Events | number | eq, gt, lt | — |
| users_seen | Users | number | eq, gt, lt | — |

`EVENT_FIELDS`:
| key | label | type | ops | options |
|-----|-------|------|-----|---------|
| name | Event | string | eq, neq, contains | — |
| distinct_id | User | string | eq, neq, contains | — |
| session_id | Session | string | eq, neq, contains | — |
| environment | Environment | enum | eq, neq | (loaded from `GET .../environments`) |
| release | Release | string | eq, neq, contains | — |

### `FilterBar.svelte`

- **Props:** `fields: FieldDef[]`, `filters: Filter[]` (bindable), `search: string` (bindable), `sinceDays: number` (bindable). Emits changes via bindings; the page owns persistence.
- **Renders:** active-filter **chips** (`Field OP value` + remove `×` using the Lucide `x` icon) · **"+ Add filter"** button opening a small popover: **field ▾ → op ▾ → value**, where value is a `<select>` for `enum`, a numeric `<input>` for `number`, and a text `<input>` for `string` · the existing `SearchInput` (free-text) · the existing `DateRange`.
- Adding a filter appends a chip and closes the popover; removing a chip updates state. All changes flow to the page, which re-queries and rewrites the URL.
- Uses existing UI atoms (`Icon`, `Button`, `SearchInput`, `DateRange`) and CSS variables; theme-aware.

### Page wiring (`Issues.svelte`, `Events.svelte`)

- Replace the Issues **status tabs** with the FilterBar; `status` becomes a normal filter field (default seeded to `status:eq:unresolved` on first load to preserve today's default view). Keep the StatTiles + occurrences chart above the bar.
- Replace the Events ad-hoc **search-only** header with the FilterBar; keep the volume chart + top-events (clicking a top event adds/replaces a `name:eq:<event>` chip instead of a separate `selectedEvent` variable).
- **URL sync:** read `querystring` (svelte-spa-router) on mount to hydrate `filters/search/sinceDays`; on any change call `replace('/issues?' + build(qs))` so state is shareable and back/forward works. Pagination `offset` resets to 0 on filter change.

## Section C — Backend filter contract

**Wire format (query params):** repeated `filter=<field>:<op>:<value>` + `q=<free text>` + existing `since_days`, `limit`, `offset`.
Operators: `eq → =`, `neq → <>`, `contains → ILIKE %v%`, `gt → >`, `lt → <`.

**Parsing + whitelist (new, shared):** `backend/bins/sauron-api/src/routes/filter.rs`
- Parse `Vec<String>` of `field:op:value` into `Vec<ParsedFilter{ field, op, value }>` (split on first two `:`).
- A per-resource **allow map**: `field -> (allowed ops, value type)`. Unknown field, disallowed op for that field, or a value that fails type coercion (e.g. non-integer for `times_seen`) → `ApiError` 400 with a clear message. This keeps the SQL surface fixed and safe.
- Route deserializes filters via `serde` `Vec<String>` query (`#[serde(default)] filter: Vec<String>`), passes parsed+validated filters into the repo.

**Repo — extend the existing boxed builders** (`backend/crates/sauron-db/src/repo.rs`):
- Add a typed `IssueFilter` / `EventFilter` enum (or a small `(Field, Op, Value)` list) that `list_issues` / `list_analytics_events` fold into the boxed query with strongly-typed diesel column expressions:
  - e.g. `level eq v` → `q = q.filter(issues::level.eq(v))`; `culprit contains v` → `q.filter(issues::culprit.ilike(format!("%{v}%")))`; `times_seen gt n` → `q.filter(issues::times_seen.gt(n))`.
  - `q=` free-text: Issues → ILIKE over `title/type/culprit`; Events → keep existing `name/distinct_id` ILIKE.
- **Environment (events):** the value is an environment *name*; resolve to `environment_id` via the existing `environments` lookup (by `app_id` + name) and filter `environment_id.eq(id)` (`neq` → `<>`/`IS DISTINCT FROM`). Unknown env name → empty result set.
- Keep default ordering (`last_seen desc` / `occurred_at desc`), limit/offset.

**Routes:** `routes/issues.rs` (`list_issues`) and `routes/analytics.rs` (`events_list`) gain the `filter: Vec<String>` + `q` params, call the parser, and pass validated filters down. Preserve existing `authorize_app` + permission checks.

## Section D — Roomier detail pages

Reflow the detail pages to use the reclaimed width (they currently sit in narrow centered columns):
- **`IssueDetail.svelte`:** two-column at wide widths — main column (title, stacktrace, occurrences chart) + a right rail (level, status, first/last seen, times/users seen, tags, assignee). Collapses to one column under ~900px.
- **`SessionDetail.svelte` / `DeviceDetail.svelte` / `PersonProfile.svelte`:** let their content/timeline use the width instead of a fixed narrow column; keep readable max-widths on prose blocks only.
- Purely presentational — no data/API changes.

## Testing / verification

- **Backend (`cargo test`):** unit tests for `filter.rs` (valid `field:op:value` → parsed; unknown field, bad op, non-numeric value → 400). Repo tests: seed issues/events and assert `list_issues` / `list_analytics_events` honor each op (`eq/neq/contains/gt/lt`) and combined AND filters. Must keep the existing 38 tests green.
- **Frontend:** `filters.ts` codec is pure — round-trip `parseFilters(encodeFilters(x)) == x` and unknown-field dropping (add a runner if none, else assert via a small script). `svelte-check` 0/0 + `vite build`. Preview: verify chips add/remove, enum dropdowns, URL updates + reload restores state, and full-width layout.

## Build sequence

1. **Backend filter core:** `routes/filter.rs` (parser + whitelist types) + tests.
2. **Backend repo:** extend `list_issues` and `list_analytics_events` with typed filters + `q`; env-name resolution; repo tests.
3. **Backend routes:** wire `filter`/`q` params into `issues.rs` + `analytics.rs`.
4. **Frontend model:** `filters.ts` (model + codec + `ISSUE_FIELDS`/`EVENT_FIELDS`).
5. **Frontend component:** `FilterBar.svelte`.
6. **Frontend pages:** wire FilterBar + URL sync into `Issues.svelte` and `Events.svelte` (retire status tabs / `selectedEvent`); extend the api modules (`api/issues.ts`, `api/events.ts`) to pass `filter[]`/`q`.
7. **Layout:** `app.css` width change; detail-page reflow (`IssueDetail`, then Session/Device/Person).
8. **Verify:** cargo test · svelte-check · vite build · preview.

## Open follow-ups (tracked, not now)

- Arbitrary property/tag filtering + value autocomplete (the full-Grafana depth).
- Env/release filtering on Issues (events join).
- Roll FilterBar out to Sessions/Users/Devices.
