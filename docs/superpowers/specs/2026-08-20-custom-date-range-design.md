# Custom date ranges — design

**Date:** 2026-08-20
**Status:** implemented — see "What changed during implementation" at the end

## The problem

Every analytics page in the dashboard windows its data with the same four-button
strip: `24h / 7d / 30d / 90d`. All four are *relative to now*. There is no way to
ask "what happened on 12 August", "how did last week compare", or "show me
July". The control also holds its value in page-local `$state`, so the window
resets to that page's default on every navigation — picking `90d` on Overview
and clicking through to Performance silently lands you back on `30d`.

Three things are missing, and they compound:

1. **No upper bound.** `RangeQuery` carries only `since_days`, and 25 repo
   functions take a bare `since: DateTime<Utc>`. A closed interval cannot be
   expressed at all.
2. **No shared selection.** Each page has its own `let sinceDays = $state(…)`.
3. **No persistence.** Nothing about the window survives a reload.

## What we're building

A **Custom** button on the range strip that opens a calendar popover offering a
specific **day**, **week**, **month**, or an arbitrary **range** — and a shared,
persisted selection so the window follows you across pages and reloads, with a
list of saved named ranges for the ones you keep coming back to.

## Decisions already taken

| Question | Answer |
| --- | --- |
| Which pages | **Everywhere**, Overview included — so the backend needs a real upper bound |
| What persists | **Both** the current selection *and* a list of saved named ranges |
| Picker UI | A hand-built **calendar grid**, not `<input type="date">` pairs |
| Timezone | The **browser's local zone**, converted to UTC instants on the wire — consistent with `time-filter.ts`, which already does this for the list pages |

---

## Architecture

Three layers, built bottom-up. Each is correct on its own and ships without
the layer above it.

```
  DateRangePicker.svelte      ← calendar grid, saved-range list
          │  DateRangeValue
  range.svelte.ts             ← shared selection + saved ranges, localStorage
          │  since_days= | from=&to=
  RangeQuery → resolve_range  ← precedence, floor, clamp disclosure
          │  repo::Range
  25 repo functions           ← SQL gains an optional upper bound
```

### Why bottom-up

The upper bound is what makes a custom range *correct*. Shipping the picker
first would mean "12 August" quietly returned everything from 12 August
**onwards** — a wrong answer carrying a 200, which is the exact failure mode
this codebase's existing comments (`resolve_time_filter`, `resolve_window`)
were written to prevent.

---

## Layer 1 — `repo::Range`

### The type

```rust
/// A half-open window: `from <= col < to`.
///
/// `from` is never optional and `to` is, and the asymmetry is load bearing —
/// the same asymmetry `TimeWindowSpec` documents. `analytics_events` is
/// `PARTITION BY RANGE (occurred_at)`, so an unbounded LOWER bound is a
/// MergeAppend across every partition. An unbounded UPPER bound costs nothing.
#[derive(Debug, Clone, Copy)]
pub struct Range {
    pub from: DateTime<Utc>,
    /// EXCLUSIVE.
    pub to: Option<DateTime<Utc>>,
}
```

with constructors `Range::since(from)` (open above) and `Range::new(from, to)`.

### Why a new type rather than an extra parameter

Two candidate shapes were considered:

- **Add `until: Option<DateTime<Utc>>` beside `since`.** Rejected: 25 signatures
  gain a parameter, and every *untouched* function keeps compiling while
  silently ignoring an upper bound the caller believes it applied.
- **Change `since: DateTime<Utc>` → `range: Range` only on the functions that
  actually honour it.** Chosen. The type is the documentation: a function taking
  `Range` honours both bounds, a function still taking `since` honours only the
  lower one, and the compiler forces every call site to be revisited. Nothing
  silently ignores a bound it was handed.

### The SQL, and the bind-index trap

These are hand-built `sql_query` strings with positional binds, and the index
arithmetic is delicate — `top_events` computes `let limit_idx = if
scope.env.consumes_bind() { 4 } else { 3 }`. Inserting a bind in the middle
would shift every index after it in 25 functions, which is precisely the class
of change that compiles, passes clippy, and returns wrong rows.

**The upper bound therefore always binds LAST**, mirroring `EnvFilter`:

```rust
impl Range {
    /// ` AND col < $idx`, or empty when the window is open above.
    pub fn upper_sql(&self, col: &str, bind_index: usize) -> String;
    pub fn upper_sql_for(&self, alias: &str, col: &str, bind_index: usize) -> String;
    pub fn consumes_bind(&self) -> bool;
}
```

plus a `bind_range!` macro shaped exactly like `bind_env!`. Existing indices are
untouched; the only new number per function is "the next free index", which each
function already computes for its own env fragment.

For diesel query-builder functions (`top_issues`' `All` arm, `count_issues`) the
bound is a conditional `.filter(col.lt(t))` on a boxed query — no arithmetic at
all.

### Scope: the 25 functions

Everything reachable from a page that renders `<DateRange>`:

| Page | Functions |
| --- | --- |
| Overview | `overview_totals`, `event_series`, `error_series`, `top_issues`, `top_events`, `user_stats`, `active_user_series`, `session_stats`, `session_duration_series`, `session_duration_histogram` |
| Workflows | `workflow_list`, `count_workflows`, `workflow_detail`, `workflow_runs` |
| Screens | `screen_list`, `count_screens`, `screen_stats`, `devices_for_screen`, `recent_events_for_screen`, `recent_exceptions_for_screen`, `users_for_screen` |
| Performance | `performance_summary`, `performance_series` |
| Issues | `count_issues`, `occurrence_stats`, `issue_occurrence_series` |
| Funnels | `funnel` |
| Journeys | `journey_graph` |

`tier_read::active_users_by_day` already takes `(since, to)` and needs no
change. `mask_tail_sweep_batch` and `sample_tag_keys` take a `since` that has
nothing to do with the picker and keep their current signature.

---

## Layer 2 — the wire

### `RangeQuery`

```rust
pub struct RangeQuery {
    #[serde(default = "default_days")] pub since_days: i64,
    pub from: Option<DateTime<Utc>>,
    pub to:   Option<DateTime<Utc>>,
    #[serde(default = "default_top")] pub limit: i64,
    pub name: Option<String>,
}
```

`RangeQuery` is a whole query struct, never `#[serde(flatten)]`ed, so the
`deserialize_any` trap that forced `opt_i64_from_str_or_int` onto
`TimeFilterQuery` does not apply here — `since_days: i64` deserializes today and
keeps deserializing. `from`/`to` are `Option<DateTime<Utc>>`, and chrono's
`Deserialize` accepts the `&str` visitor `serde_html_form` hands it. A test
drives the real extractor anyway rather than the resolver, because that is the
only place the trap is visible.

### `resolve_range`

A new function in `routes::search`, sharing its core with `resolve_time_filter`
so the precedence rule has exactly one definition:

- **Explicit bounds win outright.** `since_days` is not consulted when either
  `from` or `to` is present.
- **`to` alone gets a floor** of `to - max_days`, and the narrowing is
  **disclosed** — the substituted value *is* the floor, so a `from < floor`
  comparison finds them equal and would report nothing. This is the bug class
  the codebase has already hit twice (`resolve_window`, `resolve_time_filter`);
  the fix is a separate `floored_open_lower_bound` flag, and it is copied here
  rather than re-derived.
- **`from >= to` is a 400**, not a confidently empty result. Half-open intervals
  make equal bounds select nothing.
- **Span ceiling stays 365 days**, matching `MAX_WINDOW_DAYS` and every existing
  route ceiling — but see below for how it is enforced.

### Which endpoints reject what

Every handler in the table above accepts `from`/`to`. Handlers whose repo
function was *not* taught the bound do not gain the parameters at all, so a
stray `?from=` there is ignored by `serde` exactly as any other unknown
parameter is today — but no dashboard code sends one, and a test pins the set.

---

## Layer 3 — the overview cache

`cache_key` currently ends in `since_days` and carries a long comment about why
it must key on the *discrete selector value* and never on the derived `since`
timestamp: `Utc::now() - days` differs on every request, so a key built from it
mints a fresh entry per request and hits 0% while looking perfectly healthy.

An **absolute** range does not have this problem — `from`/`to` are fixed
instants chosen by the user, stable across requests and across users. So the key
gains a window token:

```
overview:v3:{section}:{app}:{env}:{window}
   window = "30d"                      relative
          | "2026-08-12T00:00Z..2026-08-13T00:00Z"   absolute
```

Two rules the tests pin:

1. A **relative** window still tokenizes to `{days}d` — never to a derived
   instant. The existing 0%-hit-rate guard is extended, not replaced.
2. `v2` → **`v3`**: `{days}` and `{days}d` would collide across the format
   change, and a stale `v2` entry would be served for up to 24 h under the new
   code. Bumping is cheap; the doc block on `cache_key` already says to bump
   whenever meaning changes.

`scope_token` (the SSE filter) takes the same token, so a browser watching an
absolute window is not pushed a section computed for a relative one.

---

## Layer 4 — the shared range store

### The value

```ts
export type DateRangeValue =
  | { kind: 'last'; days: number }
  | { kind: 'absolute'; from: string; to: string; preset: 'day' | 'week' | 'month' | 'custom' };
```

`from`/`to` are RFC3339 UTC, `to` **exclusive** — the same convention
`TimeFilterState` documents, and for the same reason: an inclusive bound has to
be spelled `23:59:59.999`, and `timestamptz` stores microseconds, so it silently
drops the last millisecond of every window.

**`preset` is stored, the label is not.** A label baked into localStorage in
English would still read English after the user switches the dashboard to
Arabic. The label is derived at render time from `preset` + the bounds through
`Intl`, in the active locale.

### Persistence

Two keys, both hardened the way `time-format.svelte.ts` and
`nav-collapse.svelte.ts` already are (private-mode Safari throws on both read
and write):

- `sauron.dateRange` — the current shared selection.
- `sauron.dateRange.saved` — an array of `{ id, name, from, to, preset }`,
  capped at 20, newest first.

Anything unparseable, out of range, or inverted is **dropped on read** rather
than surfaced. These values outlive the code that wrote them; a stale entry must
degrade to the page default, not to an error page.

### Interaction with per-page defaults

Pages do not share a default — Overview starts at 30 days, Issues at
`WIDEST_RANGE` (3650). The rule:

> The store holds the user's **explicit choice**. Until they make one, every
> page uses its own fallback.

So first load is byte-identical to today. Once the user picks anything, that
choice applies everywhere.

**Issues is the case worth naming.** Its list deliberately ignores the date
predicate at `WIDEST_RANGE` and discloses that through `ignoresDateRange`.
A global selection narrower than the widest setting will start filtering that
list. That is the intended behaviour of a global range control — and the
existing disclosure already tells the user which cards honour the window and
which do not, so nothing becomes silent.

---

## Layer 5 — the picker

`DateRange.svelte` keeps its preset chips and gains a trailing **Custom** chip.
The chip renders the active absolute range's derived label when one is selected,
so the control always states the window it is applying.

The popover is a hand-built month grid with four modes:

| Mode | Interaction | Resulting window |
| --- | --- | --- |
| Day | click a day cell | that local day, `[00:00, next 00:00)` |
| Week | click the week's leading row gutter | that local week |
| Month | click the month header | that local month |
| Range | click a start day, then an end day | `[start 00:00, end+1 00:00)` |

Local-day → UTC-instant conversion reuses `localInputToUtc` from
`time-filter.ts` rather than a second implementation. The `to` bound takes the
**start of the following day**, so "12 Aug to 14 Aug" covers all of 14 August —
truncating to the start of its own day would drop the final day, which reads as
a data bug.

Calendar arithmetic goes through the `Date` constructor's own overflow handling,
never `+ 86_400_000`: a day is 23 hours across a spring-forward transition, and
the millisecond form lands an hour into the next day and quietly widens the
window. `time-filter.ts` already documents this; the picker follows it.

### Accessibility and RTL

- The grid is a `role="grid"` with roving `tabindex`; arrows move by day,
  `PageUp`/`PageDown` by month, `Home`/`End` to the ends of a week.
- Arrow keys are **logical**, not physical: under `dir="rtl"` `ArrowLeft`
  advances. The dashboard ships Arabic, so this is not hypothetical.
- Weekday headers and month names come from `Intl` in the active locale, with
  the numbering system pinned as `dashboard-arabic-i18n` records.
- Escape closes and returns focus to the trigger; outside `pointerdown` closes.
  `SwitcherMenu.svelte` already implements this pattern and is the model.

### Saving a range

The popover footer offers **Save this range**, which prompts for a name and
appends to `sauron.dateRange.saved`. Saved ranges list above the grid, each with
a remove control.

---

## Testing

| Layer | What is tested, and how it could fail |
| --- | --- |
| `Range` | `upper_sql` emits nothing when open and `AND col < $n` when closed; `consumes_bind` agrees with it. Asserted on the **emitted SQL** via `debug_query`/string equality — the `scope.rs` tests already establish that only this catches a swapped arm. |
| repo fns | Behavioural DB tests over a real PG: a row inside the window is returned, a row *after* `to` is not. The second half is the one that fails today. |
| `resolve_range` | Precedence, the floor-disclosure case (`to` alone must report `clamped`), inverted bounds → 400, over-wide span narrowed and disclosed. |
| extractor | One test drives `Query<RangeQuery>` through the real axum extractor with `?since_days=7&from=…`. A resolver-level test cannot see the deserializer trap. |
| cache key | Relative windows key on `{days}d` and are **stable across two calls with a clock tick between them**; absolute windows key on the bounds; `v2` and `v3` keys differ. |
| `date-range.ts` | Day/week/month builders land on local midnight and the exclusive next-midnight; DST days stay one calendar day; wire encoding sends `since_days` XOR `from`/`to`. |
| store | Round-trips through a fake storage; survives `getItem` throwing; drops inverted and over-wide stored ranges; caps the saved list. |
| picker | Vitest cannot compile Svelte here, so the grid gets a browser-harness pass: keyboard nav, RTL arrow direction, Escape focus return. |
| i18n | The existing untranslated-string test covers the new strings; every key added to `en` gets an `ar` sibling. |

## Out of scope

- A timezone **setting**. Everything renders in the browser's zone today; adding
  a picker is its own feature and would change every existing page.
- Comparison ranges ("vs previous period").
- Sharing a range by URL beyond what pages already do with `since_days`.


---

## What changed during implementation

Six things moved between the design above and what shipped. Each is recorded
here rather than edited into the body, so the reasoning stays visible.

### 1. An over-wide explicit window is REFUSED, not narrowed

The design said a custom range wider than 365 days would be narrowed from the
`from` end and disclosed. There is nowhere to disclose it: these routes return
bare arrays (`Json<Vec<EventCount>>`), not the `SearchEnvelope` the list routes
answer with, so `clamped` has no field to travel in. A silent narrowing is a
wrong answer carrying a 200 — the exact shape this layer exists to prevent.

So the rule splits by which parameter asked:

- `since_days` above the ceiling keeps its long-standing **silent clamp**.
  Issues ships 3650 as its widest setting and has always been served 365;
  turning that into a 400 would break a shipped control to close a gap nobody
  hit.
- `from`/`to` above the ceiling are a **400** naming the ceiling. They are new,
  so there is no compatibility to keep, and a refusal is the only honest answer
  available without an envelope. The same rule covers `to` alone, which asks
  for an unbounded lower bound.

The picker enforces the same bound client-side and says so, so the 400 is
unreachable from the UI.

### 2. Two ceilings on the client, not one

`MAX_RANGE_DAYS = 365` bounds an **absolute** window; `MAX_RELATIVE_DAYS = 3650`
bounds a relative one. A single constant would either have rejected Issues'
3650 (breaking it) or accepted a 3650-day absolute window (400ing every request
on the page). The asymmetry mirrors the server exactly.

### 3. Workflows joined the taught set

Four more functions — `workflow_list`, `count_workflows`, `workflow_detail`,
`workflow_runs` — were missed in the original enumeration and do render a
`<DateRange>`. Their SQL bounded time with `started_at >= now() -
make_interval(days => $2)`, a clock expression rather than an instant, so they
also moved to an explicit `Timestamptz` lower bound on the way past.

### 4. The cache window has three variants, not two

`from` alone (a lower bound with no upper) is a fourth shape the design did not
name. It gets `Window::Since`, whose token is `{iso}..` — stable, because the
instant is the user's own rather than derived from the clock.

### 5. The shared selection starts EMPTY

The design said pages keep their own fallback "until the user makes a choice"
but did not say how. `RangeStore.value` is `null` until then and
`effective(fallbackDays)` resolves it, so first load is byte-identical to
before this feature.

### 6. The issue-detail occurrence window is deliberately NOT shared

`IssueDetail`'s occurrence list defaults to everything (3650 days). Adopting a
7-day global selection there would open an issue page showing none of its own
occurrences, which reads as missing data rather than as a filter. It keeps its
own default and its own control.
