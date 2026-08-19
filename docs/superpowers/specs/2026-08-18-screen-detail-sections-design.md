# Screen detail: four collapsible fetch-on-demand sections

**Date:** 2026-08-18
**Status:** approved, ready to implement
**Page:** `#/screens/:name` — `dashboard/src/pages/ScreenDetail.svelte`

## Goal

Replace the two static preview cards on the screen detail page with four
collapsible sections — **Events**, **Exceptions**, **Devices**, **Users** —
that start empty and collapsed, load their first page only when the user
clicks *Fetch*, page forward and back, expand a row in place to show the full
record, and link out to that record's own detail page.

## Why this is not a frontend-only change

The four sections look symmetric in the UI, but only two of the four data sets
are reachable from an existing endpoint, and neither of the other two is a
filter away.

| Section | Reachable today? |
| --- | --- |
| Events | `repo::recent_events_for_screen` exists, but is hardcoded to 20 rows with no offset and is only returned inside the `screens/detail` payload. |
| Exceptions | `repo::recent_exceptions_for_screen` — same. |
| Devices | **Not reachable.** No `DevicesLower`; `routes::devices::list` takes a plain `search: Option<String>`. |
| Users | **Not reachable.** No `PersonsLower`; `routes::analytics::persons_list` likewise. |

Two dead ends were checked and rejected before settling on the design below:

1. **Reuse the query language.** The `screen` dimension in
   `crates/sauron-query/src/catalog.rs` is scoped to `R_ISSUE_OCC`
   (Issues + Occurrences). It could be widened to `Resource::Events`, since
   `analytics_events` has both the column and the partial index. But there is
   **no app-wide occurrences route** — occurrences are only reachable nested
   under an issue, `/v1/apps/{app_id}/issues/{issue_id}/events` — so the
   Exceptions card has nowhere to send `filter=screen:eq:X` anyway. And
   "devices on a screen" is not a column filter at all: it is a `DISTINCT` over
   a different table. Widening the catalog would buy one of four cards.

2. **Derive Devices/Users client-side from a page of events.** Wrong on its
   face: de-duplicating within a 25-row page produces a list whose length and
   page boundaries mean nothing. A page of events might contain three distinct
   users.

So: four sibling endpoints, one per card, uniform in shape.

## Backend

### Routes

Four new handlers in `bins/sauron-api/src/routes/screens.rs`, registered in
`bins/sauron-api/src/main.rs` beside the existing `screens/detail`:

```
GET /v1/apps/{app_id}/screens/events      ?name=&since_days=&limit=&offset=
GET /v1/apps/{app_id}/screens/exceptions  ?name=&since_days=&limit=&offset=
GET /v1/apps/{app_id}/screens/devices     ?name=&since_days=&limit=&offset=
GET /v1/apps/{app_id}/screens/users       ?name=&since_days=&limit=&offset=
```

All four share one query struct, `ScreenSectionQuery`, and these rules:

- **Authorization:** `super::scope::authorized_read_scope` with
  `perm::EVENT_READ` — the same permission `devices::list` and `persons_list`
  already require, so no card exposes data a role could not already reach from
  its own list page.
- **`environment_id` is read from `RawQuery`, never as a `Query<T>` field.**
  This is the trap `ScreenListQuery` documents in its own comment; a field here
  would bypass `authorized_read_scope`'s environment authorization.
- **`name` is required**; empty or whitespace is a 400, matching `detail`.
- **`since_days`** defaults to 30 and is clamped to `1..=365`, matching `detail`.
- **`limit`** clamped to `1..=100`, **`offset`** through `super::clamp_offset`.
- **Response is a bare JSON array** of at most `limit` rows. The client asks for
  `limit + 1` and treats the surplus row as the has-more probe — the house
  `overFetched` pattern already used by `listScreens`. No count endpoints:
  these are prev/next controls, not numbered pages.

The **exceptions** handler additionally uses
`authorized_read_scope_with_perms` and calls
`crate::symbolicate::gate_source_context` + `gate_event_body`, exactly as
`screens::detail` does today — `ErrorEvent` rows carry `perm::ISSUE_READ`
(body) and `perm::SOURCE_READ` (de-obfuscated frames) questions that
`EVENT_READ` does not answer.

### Repo

In `crates/sauron-db/src/repo.rs`:

- `recent_events_for_screen` — add an `offset` parameter. Keeps the
  `name <> '$screen'` exclusion so the list is real events, not the synthetic
  screen-view rows. Reaches `analytics_events_app_screen_time_idx`
  `(app_id, screen, occurred_at DESC) WHERE screen IS NOT NULL`.
- `recent_exceptions_for_screen` — add an `offset` parameter. Reaches
  `error_events_app_screen_time_idx`.
- `devices_for_screen` / `users_for_screen` — **new**, sharing one
  `screen_signal_union` helper and one `SCREEN_ACTOR_AGG` select. Both union
  `analytics_events` and `error_events` on `app_id=$1 AND occurred_at>=$2 AND
  screen=$3`, group by `device_key` / `distinct_id`, and order
  `last_seen DESC, k ASC` — the tiebreak is what makes `OFFSET` paging total.

**Changed during implementation — these return SCREEN-SCOPED rows, not
`DeviceRow`/`PersonRow`.** The original plan was to reuse those two row types.
That was wrong: neither `persons` nor `devices` is a plain table on the read
path. `PersonRow` is assembled from `event_users` joined to an
`event_user_environments` rollup, `DeviceRow` likewise carries derived
`last_distinct_id`/`sessions_count`, and both list queries are documented as
delicate (the persons rollup and the device-groups fan-out each cost an
incident). Joining them here would have meant either reopening those queries or
quietly reproducing half of one.

So the two new endpoints answer `ScreenUserRow`/`ScreenDeviceRow`, whose
counters (`views_on_screen`, `events_on_screen`, `exceptions_on_screen`,
`first_seen_on_screen`, `last_seen_on_screen`) are computed here from the
screen's own signal. `event_users.properties` and the `devices` descriptive
columns are `LEFT JOIN`ed for display only — per-entity facts with no rollup to
get wrong. The join is LEFT, not INNER, because an identity can appear in the
event stream before its `event_users`/`devices` row is upserted, and an INNER
join would drop exactly the newest ones.

This is also the better answer to the card's question: "this user fired 209
exceptions on this screen" beats their lifetime total, which is one click away
on `/persons/:distinct_id` anyway. The `_on_screen` suffix exists so the two
can never be misread for each other.

Both new queries reuse the union shape `screen_ctes`' `us` CTE already uses to
compute the per-screen user count, so the row sets agree with the stat tile
above them by construction rather than by coincidence.

Every one of the four threads `scope.env` through `crate::scope_env!` /
`crate::bind_env!`, and the two new raw-SQL ones must place the env bind after
`$3` and shift nothing, as `screen_stats` documents.

## Frontend

### New files

- `dashboard/src/lib/components/CollapsibleFetchCard.svelte` — the shared
  shell. Props: `title`, `icon`, `count?`, and a `rows` snippet. Owns collapsed
  state, the *Fetch* button shown while nothing has loaded, the spinner, the
  error line, and the prev/next footer. Knows nothing about what it lists.
- `dashboard/src/lib/api/screen-sections.ts` — four typed functions
  (`listScreenEvents`, `listScreenExceptions`, `listScreenDevices`,
  `listScreenUsers`) each returning `ListPage<T>` via `overFetched`, mirroring
  `listScreens`.

### `ScreenDetail.svelte`

The two static `Recent events` / `Recent exceptions` cards are **removed**. The
stat tiles stay. Below them, the four `CollapsibleFetchCard`s render in a
two-column grid that collapses to one below 900px, matching the existing
`.lists` breakpoint.

Row interaction, per section:

| Section | Expanded row shows | Links to |
| --- | --- | --- |
| Events | name, timestamp, `properties`, `tags`, session, release | *(no detail page — expand only)* |
| Exceptions | type, message, culprit, stack | `/issues/:issue_id` |
| Devices | family, model, OS, arch, browser, counts | `/devices/:device_key` |
| Users | distinct id, traits, first/last seen, counts | `/persons/:distinct_id` |

The row is a button that toggles inline expansion; the link out is a separate
control on the same row, so the two affordances never collide.

`getScreenDetail` keeps returning `recent_events`/`recent_exceptions` — the
page simply stops rendering them. Trimming the endpoint's payload is out of
scope here and would touch other callers.

### Card state must reset when the screen changes

`svelte-spa-router` **reuses the component instance** when navigating
`#/screens/A` → `#/screens/B`. Card state (`rows`, `offset`, `expandedId`,
`hasFetched`) lives in the page, so without an explicit reset keyed on
`screenName`, screen B renders screen A's rows under screen B's title, with the
stat tiles above them already showing B's numbers.

This is the failure mode a remounting test harness structurally cannot see, so
the reset is asserted by driving a real in-place navigation, not by mounting
the page twice.

## Risks

1. **The aggregate on the two new queries is O(events on screen), and
   MEASURED it does not use the index.** Verified with `EXPLAIN (ANALYZE,
   BUFFERS)` against a 212,501-event dev database, on a screen holding 42,175
   of those events in the window:

   | Query | Plan | Time | Buffers |
   | --- | --- | --- | --- |
   | Events / Exceptions page | Index Scan per partition | **1.7 ms** | 68 |
   | Users / Devices page | Seq Scan, all partitions | **~245 ms** | 86,133 |

   The index (`analytics_events_app_screen_time_idx`) exists and the predicate
   matches it; the planner declines it because the query needs 20% of the table
   (42,175 / 212,501), where a sequential scan is genuinely cheaper. This is not
   a fixable predicate bug — it is inherent to ranking users by last-seen and
   counting their per-screen activity, both of which must read every matching
   row. Dropping the per-user counters measures at 88 ms, so the counters cost
   ~157 ms of the 245 ms.

   **Accepted, with the limit stated:** cost scales linearly with a screen's
   event volume in the window, so ~10x traffic or retention puts this at ~2.5 s
   and ~100x reaches the 30 s `TimeoutLayer`. Two things bound the damage: the
   fetch-on-demand design means this runs only when a user explicitly clicks,
   never on page load (unlike the `/overview` aggregate that caused the 503s),
   and the page's own `screen_stats` tiles already pay the same order of cost.

   If it does need to get faster, the path is the loose-index-scan technique
   already used for the screens count: walk `(app_id, screen, occurred_at DESC)`
   backwards to collect one page of distinct keys and stop early, then compute
   counters for just those rows — which needs a new
   `(app_id, screen, distinct_id)` index, i.e. a migration.

2. **Permission-shaped emptiness.** A role with `EVENT_READ` but not
   `ISSUE_READ` must see the Exceptions card *absent*, not present-and-erroring.
   Gating follows `PAGE_ACCESS`, the existing single source of truth.

## Testing

**Backend**
- Unit: `ScreenSectionQuery` clamping — `since_days` bounds, `limit` bounds,
  empty `name` rejected.
- DB: one test per new repo fn asserting rows, the `$screen` exclusion on
  events, the `distinct_id <> ''` filter on users, and that a second page at
  `offset = limit` returns disjoint rows (the untiebroken-OFFSET check).
- These must run with `dangerouslyDisableSandbox` and host-network containers.
  A DB suite that prints `ok` in 0.00s has skipped, not passed — assert on the
  test count, not the exit code.

**Frontend**
- `svelte-check` and `vitest`.
- Harness drive of the page: cards start collapsed and empty; *Fetch* issues
  exactly one request per card with the expected URL (`name`, `since_days`,
  `limit = pageSize + 1`, `offset`); next/prev move `offset`; and an in-place
  `#/screens/A` → `#/screens/B` navigation clears every card.

## Out of scope

- Trimming `recent_events`/`recent_exceptions` from the `screens/detail` payload.
- Widening the `screen` catalog dimension to `Resource::Events`.
- Count endpoints / numbered pagination for the four sections.
- Sorting controls within the cards.


## Verification results (2026-08-19)

**Backend** — `cargo test --workspace`: **1943 passed, 0 failed, 4 ignored**
across 78 suites. `cargo clippy --workspace --all-targets` clean. Formatting
clean for every file touched here.

> **That number was reported once before it was true.** The first run set only
> `TEST_DATABASE_URL`. `http_env_scoping.rs` needs `TEST_REDIS_URL` *as well*
> and returns early printing `ok` when either is missing — so it contributed a
> full green suite line while executing nothing, and the total was identical to
> a real green run. Counting tests is not enough; the tell is **duration**
> (`0.00s` = skipped, `0.85s` = ran). With Redis actually supplied the suite was
> `1942 passed / 1 failed` and the failure was this work's — see D1 below.
> Correct invocation:
>
> ```
> TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5555/postgres \
> TEST_REDIS_URL=redis://127.0.0.1:6390 cargo test --workspace --no-fail-fast
> ```

New tests: 4 unit tests on `section_bounds` (blank name refused, limit clamped
at both ends, offset never negative, window matches the page) and one DB test,
`users_and_devices_for_screen_are_scoped_and_agree_with_screen_stats`, which
pins the row count to `screen_stats.users` across env_a / env_b / all, checks
paging is disjoint, and carries an explicit non-vacuity guard so it cannot pass
on an empty fixture.

**Frontend** — `svelte-check`: 0 errors (6 warnings, all pre-existing in
`TimeFilter`/`Timeline`). `vitest`: 1008 passed.

**Runtime** — driven against a live API on the 212k-event dev database:

- All four cards render collapsed and empty; expanding shows the Fetch prompt
  and issues **no** request until clicked.
- Wire format confirmed: `?name=%2Fsettings&limit=26&offset=0&environment_id=…`
  — 25 + the over-fetch probe, with the interceptor's `environment_id` attached.
- Paging: page 2 returned rows 26–50 with **zero overlap** against page 1.
- Row expand shows per-screen counters and traits; the separate arrow navigates
  to `#/persons/:distinct_id` and `#/issues/:issue_id`.
- **The reset works**: an in-place `#/screens/%2Fsettings` →
  `#/screens/%2Fpricing` navigation left all four cards collapsed with zero
  rows, under the new screen's own title and tiles.
- Zero console errors in a clean tab across a full four-card exercise.


## Defects found by post-implementation review, and fixed (2026-08-19)

An adversarial multi-agent pass over the finished work found five real defects.
All are fixed, and each fix was proven by **mutation**: break it deliberately,
confirm the guard now fails, restore.

**D1/D2 (critical) — the four new routes broke a source-walking contract test,
and made its sibling pass vacuously.** `http_env_scoping.rs` enumerates
app-scoped GET routes straight out of `main.rs`, then probes each with an
`environment_id`. Its probe sent no `name`, so `Query<ScreenSectionQuery>`
failed extraction *before* the handler ran and the resulting 400 was read as
"this route rejects environment_id" — putting all four in the rejecting set and
turning `the_backend_rejection_set_matches_the_dashboard_exclusion_list` red.
Worse, `every_app_scoped_get_either_narrows_or_rejects_environment_id` accepts
any 400, so it passed on a 400 that had nothing to do with environment scoping:
the guard written so "a route added tomorrow is covered" was covering nothing.

The obvious fix — adding the four to `scope.ts`'s
`BACKEND_REJECTS_ENVIRONMENT_ID` — is a **data leak**: that array is what
`shouldScopeUrl` reads, so the dashboard would stop attaching `environment_id`
and every card would render all environments' rows under environment-scoped
tiles. The correct fix is a `name=` arm in the test's `extra_query`
(`http_env_scoping.rs`), leaving `scope.ts` untouched.
*Mutation proof:* replacing `raw_query.as_deref()` with `None` in
`section_events` now fails the guard ("accepted a malformed environment_id — it
is neither narrowing nor rejecting"); before the fix it passed.

**D3 (medium) — nested `<button>`.** `TimeValue` renders its own button (it
toggles relative/absolute time globally and calls `stopPropagation`). Placing it
inside `SectionRow`'s summary button was invalid HTML and stole the row click:
clicking a row's timestamp flipped every timestamp in the dashboard instead of
expanding the row. Fixed with `asText` on the four summary-line instances only —
the two inside `detail` snippets sit outside the button and stay interactive.
*Verified in the browser:* `.summary button` count 25 → 0, and a timestamp click
now expands the row with the time format unchanged.

**D4 (medium) — index-keyed rows leaked expanded state across page turns.**
`{#each rows as item, i (i)}` keyed by array index while `rows` is replaced
wholesale, so Svelte reused each `SectionRow` instance — and its private
`expanded` flag — across a page change. Fixed with a required `rowKey` prop
(`e.id` / `x.id` / `d.device_key` / `u.distinct_id`).
*Mutation proof, in the live UI:* under index keying, opening
`crebain-user-303` on page 1 and pressing Next left page 2's third row
(`crebain-user-246`) rendered `aria-expanded="true"` with an open panel —
one person's traits shown under another's identity. Under identity keying the
same sequence yields zero expanded rows.

**D6 (coverage) — a devices-only environment leak was invisible.** The DB test
asserted `users.len() == stats.users` but never asserted anything about
`devices.len()`; the only devices check was a per-row `signal > 0` loop, which a
leak cannot fail (leaked rows carry counters too). Fixed with a
`distinct_device_count_for_screen` oracle written as independent hand-rolled SQL
— deliberately not reusing `screen_signal_union`, since an oracle built from the
implementation reproduces its bugs.
*Mutation proof:* dropping `env_sql` from the device union now fails with
`left: 5, right: 2`; before, it printed `ok`.

**D5 (coverage) — nothing asserted these URLs are environment-scoped.** Added a
`scope.test.ts` case pinning all four to `shouldScopeUrl === true` and
`computeScopeParams` returning the id. It fails in the opposite direction from
the backend contract test, which is the point: a future exclusion regex broad
enough to swallow `screens/…` would otherwise silently drop `environment_id` and
every card would serve cross-environment rows at HTTP 200.

Reviewed and deliberately not changed: bind-index arithmetic (hand-verified for
all four `EnvFilter` variants), both `LEFT JOIN`s (1:1 via existing unique
constraints), ORDER BY totality, `sectionKey` stability, the generation guard,
and TS↔Rust field parity — all confirmed correct.


## The 245ms: measured, and deliberately NOT fixed here (2026-08-19)

The Devices/Users aggregate was re-examined to see whether an index could close
it. It can — and the fix was still rejected. Both halves were measured on the
dev database (212k events, 42,175 on the target screen).

**A covering index makes the read 3.9x faster.**

```sql
CREATE INDEX ... ON analytics_events (app_id, screen, occurred_at DESC)
  INCLUDE (distinct_id, device_key, name) WHERE screen IS NOT NULL;
```

| | plan | time | buffers |
| --- | --- | --- | --- |
| existing index | Seq Scan, all partitions | 245 ms | 86,079 |
| covering index | **Index Only Scan, `Heap Fetches: 0`** | **62 ms** | 34,015 |

It is a strict superset of `analytics_events_app_screen_time_idx` — same key
columns, same partial predicate — so it would REPLACE that index rather than sit
beside it. Net size +73 MB (236.7 vs 163.7 across partitions, against 1,502 MB
of existing indexes on the table).

**And it costs 14% on the ingest write path.** 20,000-row insert, same rows,
same table: **537 ms with the index, 471 ms without.**

That is the number that settles it. `analytics_events` is the highest-write
table in the system and the one this project has spent the most effort making
fast; a 14% write tax is not something a screen-detail feature gets to levy on
ingest to make a click-to-load card faster than "already sub-second". The read
side is user-initiated and bounded — nothing runs it on page load.

Two further reasons to leave it: a teammate is mid-audit of this exact table's
indexes (`2026-08-18-000066_analytics_events_index_audit`, which deliberately
*kept* the screen index), and the true write cost belongs in a crebain
throughput run, not a single-statement timing.

**So: no migration here.** The finding is recorded so the decision can be made
by whoever owns ingest performance, with the numbers already in hand. If it is
taken, it should replace the existing index rather than add to it.
