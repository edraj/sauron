# Retention & Cohorts — design

**Status:** approved design, not yet planned
**Date:** 2026-08-28
**Supersedes:** nothing. Fills the `retention, segments` gap listed unbuilt in
`plan.md` §5 (line 106) and §6 (line 121).

## Why

Sauron answers *what* users did (events, screens, transactions), *where they
went* (journeys, funnels) and *how many are active* (DAU/WAU/MAU, stickiness).
It cannot answer **whether they came back**. A flat DAU line hiding 100% weekly
turnover is indistinguishable, on every screen we ship today, from healthy
growth.

Grepping confirms the gap is real rather than merely unsurfaced: every
`retention`/`cohort` identifier in the backend refers to *data* retention
(purge horizons, TTLs), not user retention.

## Scope

Four surfaces, all reading one new table:

1. **Retention grid** — cohort × period-N triangle, daily or weekly.
2. **Lifecycle** — new / returning / resurrected / dormant per period.
3. **Error-impact split** — retention of users who hit an error in period 0
   versus those who did not. The unified error+analytics timeline is what makes
   this cheap for us and hard for anyone else.
4. **Churn list** — persons active before, silent since; click through to
   `PersonProfile`.

**Out of scope for this cut:** arbitrary start/return event selection,
property-filtered cohorts, saved segments. The API shape below does not
foreclose them.

## Decision: exact person-day rows

`user_activity_daily` stores HyperLogLog sketches. HLL unions (which is how
DAU/WAU/MAU work) but does not intersect, and retention is *precisely* an
intersection: who was in cohort C **and** active in period N. Retention
therefore cannot ride the existing rollups; it needs new storage.

Three shapes were considered, at the documented target of 10M records/day
(~900M rows per 90-day window):

| | Shape | Rows @1M users/90d | Notes |
|---|---|---|---|
| **A** *(chosen)* | one row per (app, env, distinct_id, day) | ~15M, ~2 GB | mirrors `device_sessions_daily` |
| B | one row per person-month, days as int32 bitmask | ~3M, ~300 MB | per-person period alignment across month edges is fiddly SQL |
| C | day-level roaring bitmaps over interned person ids | ~90 rows/env/quarter | exact intersection, but needs an id registry, a new dependency, merge remapping and reverse lookup |

**A** was chosen because every difficult part — the fold, the backfill, the
purge hook, the identity-merge hook — already has a working template in this
repo to copy, whereas B and C each invent new machinery in four places at once.
It is exact, which matters for numbers people argue about in meetings; `sketch.rs`
already draws this line, reaching for sketches only where exact aggregation is
impossible, and here it is possible. The API contract is identical under all
three, so B or C remain available later as a pure storage swap.

### The exception this makes, stated deliberately

Migration 71's header records the rollup principle: *"size is bounded by
(keys × environments × days), never by event volume"*. `person_days` is bounded
by **users × days**, and its fold emits one row per active person-day rather than
a fixed number of buckets. This is the first rollup whose write volume scales
with users. We accept it knowingly: ~2 GB per 90 days is ~0.1% of the ~1.7 TB
of firehose covering the same window, and it prunes on a horizon knob.

## Data model

Migration 74. One table, shaped after `device_sessions_daily`:

```sql
CREATE TABLE person_days (
    app_id         uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    distinct_id    text NOT NULL,
    day            date NOT NULL,
    events         bigint NOT NULL DEFAULT 0,
    errors         bigint NOT NULL DEFAULT 0,
    updated_at     timestamptz NOT NULL DEFAULT now()
);
```

Two indexes, because the two readers scan from opposite directions:

- `UNIQUE (app_id, env_key, distinct_id, day)` — the retention grid's cohort probe.
- `(app_id, env_key, day)` — lifecycle's day-range scan.

`env_key` is the existing `COALESCE(environment_id, '000…0'::uuid)` sentinel
from migration 56, for the reason recorded there: `NULL <> NULL`, so a plain
`UNIQUE` including a nullable column lets one person accumulate unlimited
unattributed rows whose counters silently stop accumulating.

`errors` exists solely to power the error-impact split; the fold already reads
the error firehose in the same pass, so it costs one column and no extra I/O.

**No cohort table.** `event_user_environments.first_seen` already records each
person's first signal per (app, environment), is maintained live by the write
path, and is already rebuilt by identity merge. Cohort assignment is
`date(first_seen)` — free, and consistent by construction.

**One row per environment, though**, which the queries must respect in two
places or they will quietly disagree with `/users/summary`:

- *Cohort assignment* on an unscoped (all-environments) request takes
  `MIN(first_seen)` across that person's environment rows. Taking any single row
  would place the same person in different cohorts depending on which
  environment the planner reached first.
- *Cell counts* are `count(DISTINCT distinct_id)`, never `count(*)`. A person
  active in two environments on one day has two `person_days` rows, and summing
  them would report retention above 100%.

Under an environment-scoped request both collapse to the single matching row and
the distinction costs nothing.

**Pruning.** `PERSON_DAYS_KEEP` (default 400 days) in the existing maintenance
pass, matching `MAX_TIMESERIES_DAYS`' ceiling on the longest answerable window.

## Fold

`fold.rs`'s existing pull already yields `(app_id, environment_id, occurred_at,
distinct_id)` from both the analytics and error firehoses. Add one delta map
keyed `(app, env, day, distinct_id)`, folded in the same pass and the same
transaction.

Unlike `add_user_activity`, `add_person_days` needs no read-modify-write round
trip: there is no sketch to merge in Rust, so it is a chunked
`INSERT … ON CONFLICT DO UPDATE SET events = person_days.events + EXCLUDED.events`
(and likewise `errors`). Chunking uses the existing `CHUNK` constant.

Selection stays by `received_at` behind the watermark and bucketing stays by
`occurred_at`, exactly as the other folds do, so a late event lands in its
correct historical bucket.

## The four hazards

Each is answered by copying a template that already exists in this repository.

### 1. The readiness gate would lie by default

`rollup_backfill` markers already exist for apps backfilled under migration 71.
Reusing that marker would make `rollups::is_ready()` return true for an app
whose `person_days` is empty — producing a **confident 0% retention grid**,
which is worse than an error because it looks like an answer.

This codebase has learned this twice already: `event_user_env_rollup_epoch` and
`device_env_rollup_epoch` exist as separate tables for exactly this reason.
Retention gets its own `person_days_epoch` and `person_days_backfill` marker,
and the API gates on *those*.

### 2. Identity merge must union days, not sum them

`identity_merge.rs` rewrites `distinct_id` across six tables and rebuilds
`event_user_environments`. Alias `person_days` rows are upserted into the
canonical id with `+` on the counters; two rows for the same day collapse to
one via the unique index, so the day *set* unions while the counters sum.
Without this hook, every `identify()` call silently inflates retention — and
guest-then-identify is the common path, not an edge case.

### 3. GDPR erasure must reach the new table

`purge.rs` deletes `event_user_environments` during erasure. `person_days` is
added beside it. Missing this leaves per-person daily activity behind after an
erasure request.

### 4. Backfill must be additive against a cutoff

`sauron-migrate backfill-person-days`, following `person_env_backfill.rs`
exactly: aggregate rows strictly before the cutoff and **add**, rather than
`ON CONFLICT DO NOTHING`. The write path bumps `person_days` from the moment
migration 74 lands, so a live bump can create a row before the backfill reaches
that person; `DO NOTHING` would then skip it and leave that person short by
their entire history — silently and permanently. Disjointness is a property of
the cutoff being the instant the live path started counting, which is why the
epoch is stamped by the migration itself and is not `Utc::now()`.

The known residual documented there applies here unchanged: a backdated event
arriving between the cutoff and the backfill finishing is counted twice. It is
bounded by the backfill's duration and is disclosed, not fixed.

## Semantics

Pinned here rather than left to emerge from whatever the SQL happens to do:

- **Cohort** — persons bucketed by `date(first_seen)` in UTC; daily, or weekly
  by ISO week (Monday start).
- **Period N** — offset from *that person's own* cohort start, not from a
  calendar anchor (aligned retention).
- **Retained in period N** — at least one `person_days` row within that period.
- **Period 0** is 100% by construction, so it renders as the cohort **size**,
  not as a percentage.
- **Incomplete cohorts render empty, never 0%.** A cohort whose period-N window
  extends past `as_of` has no answer yet. Showing 0% there is the most common
  retention-chart bug in the category. `as_of` comes from `rollup_watermarks`,
  as with every other rollup.
- **Lifecycle**, per period P: `new` (first_seen in P) · `returning` (active in
  P and P−1) · `resurrected` (active in P, not P−1, active sometime before) ·
  `dormant` (active in P−1, silent in P; drawn as a negative bar, the
  conventional rendering).
- **Error-impact** — within a cohort, split on `errors > 0` **in period 0**
  (that person's first day, or first ISO week, at the chosen granularity) and
  draw two curves. Measuring exposure only in period 0 is what keeps the
  comparison from being circular: users who churn early cannot accumulate later
  error exposure, so a whole-window split would manufacture the correlation it
  claims to find. The UI states this is an association, not causation.
- **Identity caveat** — when a guest identifies, the merged `first_seen` moves
  earlier, retroactively moving that person between historical cohorts. Correct,
  but surprising enough to footnote in the UI.

## API

New `routes/retention.rs` rather than extending `analytics.rs` (1,768 lines).
Authorization matches its neighbours: `scope::authorized_read_scope` with
`perm::EVENT_READ`. `environment_id` is read via `RawQuery`, **not** as a
`Query<T>` field — see `routes/scope.rs`'s module docs for the extractor trap
that avoids.

| Endpoint | Returns |
|---|---|
| `GET /v1/apps/{app_id}/retention` | `{ granularity, as_of, ready, cohorts: [{ start, size, periods: [n\|null] }] }` |
| `GET /v1/apps/{app_id}/retention/lifecycle` | `{ as_of, ready, points: [{ start, new, returning, resurrected, dormant }] }` |
| `GET /v1/apps/{app_id}/retention/churn` | keyset-paginated persons, silent for at least `silent_periods` |

Query parameters on the grid: `granularity=day|week`, `cohorts`, `periods`,
`since_days` or `from`/`to`, `environment_id`, `split=none|errors`. The churn
list takes `silent_periods` (default 4) at the same `granularity`, so "churned"
means the same span of time the grid is drawn in.

Three load-bearing decisions:

- **`null` is not `0`** in the `periods` array. `null` means *not knowable yet*.
  The wire type carries the distinction so no client can invent a zero.
- **`ready` and `as_of` ride in every response**, mirroring `rollups_status`.
  This is what turns "the operator never ran the backfill" into an explicit UI
  state instead of a plausible-looking empty grid.
- **Cap the product, not the dimensions**: `cohorts × periods ≤ 400`.
  `active_users.rs` records this lesson already — bounding 20 apps and 92 days
  independently does not bound their 1,840-scan product.

`/retention/churn` reads `event_user_environments.last_seen` rather than
`person_days`, and uses keyset pagination with a hard LIMIT like every other
list endpoint.

Environment scoping **narrows** these queries rather than widening them, because
`env_key` sits in the leading index position — the opposite of the
`/users/summary` shape behind the known 30s timeouts. No SWR cache in v1: the
grid is a bounded index join under a 400-cell ceiling. The active-users SWR
pattern is the escape hatch if measurement disagrees.

## Dashboard

One page at `#/retention`, in the existing **Analyze** nav group beside Funnels
and Journeys.

Registration touches four parity-tested tables, so an omission fails CI rather
than production:

- `routes.ts` — lazy route, inline `import()` literal at the call site.
- `PAGE_ACCESS['/retention'] = { perm: 'event:read', level: 'app', title: 'Retention', envAware: true }`
- `SHELL_FLAGS['/retention'] = APP`
- `Sidebar` entry in the Analyze group.

i18n strings go in `catalog/analyze.ts` **with Arabic written at authoring
time**. The untranslated-string test has returned a false green twice; it is not
a safety net.

Components:

- **`RetentionGrid.svelte`** — new. Nothing in the library (BarList, FunnelChart,
  SankeyChart, Sparkline, TimeSeriesChart, UserActivityChart, DurationHistogram)
  renders a matrix. Incomplete cells get a visually distinct *empty* state, not a
  pale 0% colour, plus a legend that says so.
- **Lifecycle** needs a stacked bar with a negative `dormant` series. If
  `TimeSeriesChart` does not stack cleanly, add a small `LifecycleChart.svelte`
  rather than bending it.
- **Error split** is a toggle rendering two curves beneath the grid, not a
  doubled grid.
- **Churn list** is `DataTable` + `CursorPagination` + `SortableTh`, rows
  clicking through to `PersonProfile`, with `auxclick` handled —
  `stopPropagation` on `click` alone opens two tabs on middle-click.

Page structure is four independently fetched cards (the `ScreenDetail` pattern),
so a slow churn query cannot block the grid.

## Testing

**The trap that would otherwise sink this suite.** Test databases pin the rollup
epoch ten years forward so suites exercise legacy paths. Retention *has* no
legacy path: pinned the same way, `ready` is false everywhere, every test
asserts an empty grid, and the suite passes having verified nothing. Two
mitigations, both required:

1. Fixtures seed `person_days_epoch` to a past instant explicitly.
2. A test asserts `ready == true` for a seeded app — so a closed gate fails red
   instead of passing empty.

Related: a backend run without a reachable database prints `ok` in 0.00s. This
suite is verified by row assertions and elapsed time, never by a green line.

Layers:

- Pure `fold_person_day_rows` unit tests over plain slices, no Postgres,
  mirroring the existing `fold_*_rows` functions.
- `rollup_equivalence.rs`: folded `person_days` equals the same window computed
  directly from raw events. This is the case that catches double-counting.
- Identity merge: guest active on days {1,2}, canonical on {2}; after merge the
  canonical person has exactly {1,2}, counters summed, no duplicate day row.
- Purge: erasure removes `person_days` rows for the erased `distinct_id`.
- Backfill disjointness across the cutoff, with the documented residual asserted
  as known rather than absent.
- API: cell-cap rejection, `null`-versus-`0` in incomplete periods, environment
  scoping narrowing the plan, and the `ready == false` response shape.
- Dashboard: the four parity tests fire automatically; plus a component test
  that an incomplete cell renders empty rather than 0%.

## Rollout

- Migration 74 creates the table **and stamps its epoch in the same migration**.
  A stamp taken later lies about every row that arrived in between, and that
  instant is not recoverable after the fact (the migration-70 lesson).
- The live fold begins immediately. History requires the operator-run
  `sauron-migrate backfill-person-days`; until its marker exists the API reports
  `ready: false` and the UI names that exact command rather than rendering an
  empty grid.
- Version lockstep: `Cargo.toml`, `sauron.spec` and `dashboard/package.json`
  move together.
- Completion is claimed only after a runtime drive against a seeded database,
  not from unit tests alone.
