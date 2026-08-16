# Time filtering for Events, Sessions, Users and Devices

Date: 2026-08-16
Status: design approved, not implemented

## Problem

The four signal-browsing lists — Events, Sessions, Users, Devices — can each express
exactly one kind of time question: "the last N days", where N comes from a four-button
picker (1 / 7 / 30 / 90) and the column the window applies to is hard-coded per route.

Three consequences:

1. **No absolute window.** "Between 1 August 14:20 and 1 August 14:35" and "before
   June" are both unaskable. Incident work needs the first; retention and churn work
   needs the second.
2. **The window's field is fixed.** Users can only be windowed by `last_seen`, so
   "users whose *first* seen falls in the last 7 days" — new users — cannot be asked,
   even though that page's own stat tiles compute exactly that distinction and display
   it directly above a table that cannot reproduce it.
3. **On Users the picker is decorative.** `UsersExplorer.load()` takes no `sinceDays`
   argument, so the range control on that page filters the stat tiles and nothing else.
   The persons table has never had a time window at all.

### Where each page stands today

| Page | Time control | Window column | Query language | Paging |
|---|---|---|---|---|
| Events | `DateRange` inside `FilterBar` | `occurred_at` | planner-wired | keyset cursor |
| Sessions | `DateRange` | `started_at` | planner-wired | offset |
| Users | `DateRange` (tiles only) | *none* | substring `search` | offset |
| Devices | `DateRange` | `last_seen` | substring `search` | offset |

`firstSeen` / `lastSeen` do exist as `sauron-query` catalog dimensions with `OPS_ORD`,
and `parse_time` already accepts both `-7d` and RFC3339 — but those dimensions are
scoped `R_ISSUES`, and neither `persons_list` nor `devices::list` is wired to the query
planner. The grammar could express these questions; the two routes that need them most
cannot receive one.

## Decisions

These were settled during design. They are not open.

1. **A dedicated time control, not the query language.** Extending the grammar to
   Persons and Devices would mean widening the catalog, writing leaf mappers, and
   landing S2c-style cursor/keyset work on two more routes. A first-class control
   reaches all four pages without any of it, and reads better for the specific
   question — a field/operator/value row is what people expect a time filter to look
   like. The grammar route stays open as later work; nothing here forecloses it.
2. **The control governs the table only.** Charts and stat tiles keep their own
   `DateRange`, visually separated. Otherwise `/sessions/summary`, `/users/summary`,
   `/events/top` and `/events/series` all need `from`/`to`/field parameters, and a
   card that cannot express the chosen field has to either lie or caption its way out.
3. **Date + time, entered in the browser's local zone.** Minute precision is what
   incident work needs, and a bare date means local midnight to the person typing it.
   The control labels the offset (e.g. `UTC+03`) so the value is never ambiguous.
4. **Devices gets the two missing indexes** rather than shipping an unindexed
   `first_seen`. Under the 30s `TimeoutLayer` a seq-scan does not read as "slow page",
   it reads as a broken endpoint.
5. **All four pages sync to the URL.** A precisely built window that cannot be linked
   or survive a refresh is most of the value thrown away.
6. **`received_at` and `identified_at` are out of scope.** Reasons in §7.

## 1. The control

`dashboard/src/lib/components/TimeFilter.svelte`, replacing `DateRange` on the four
pages. `DateRange` itself stays — the chart and tile cards still use it, as do other
pages.

```
[ Last seen ▾ ]  [ in the last ▾ ]  [ 30 days ▾ ]
[ Last seen ▾ ]  [ after       ▾ ]  [ 2026-08-01 14:20 ]   UTC+03
[ Last seen ▾ ]  [ before      ▾ ]  [ 2026-08-01 14:20 ]   UTC+03
[ Last seen ▾ ]  [ between     ▾ ]  [ from ] [ to ]        UTC+03
```

- The field dropdown is page-declared and renders as a single static label when the
  page offers only one field (Events), so the control does not grow a dropdown with
  nothing to choose.
- The default field is each page's current hard-coded column, and the default mode is
  `last` at each page's current default. **Nothing changes for a user who does not
  touch the control**, and no existing bookmark shifts its meaning.
- Presets for `last`: 1h, 24h, 7d, 30d, 90d, plus a free-entry N. The existing
  1/7/30/90 remain reachable.
- The `to` input on `between` is validated against `from` in the control, not only on
  the server: an inverted range should not cost a round trip to reject.

### Number-input hazard

The free-entry N must be a **text** input, not `<input type="number">`. Per
`svelte-number-input-binding`: `bind:value` on a number input writes back
`number | null`, which crashes a string validator; and because the submit button's
`disabled` is itself a `$derived`, the throw happens while *computing the guard*, so
the DOM freezes with the button still clickable. A number input also silently rounds
mistyped values.

## 2. Model

`dashboard/src/lib/models/time-filter.ts` — pure and unit-tested, no Svelte imports.

```ts
export type TimeMode = 'last' | 'after' | 'before' | 'between';

export interface TimeFilterState {
  readonly field: string;       // 'last_seen' | 'first_seen' | 'started_at' | ...
  readonly mode: TimeMode;
  readonly lastDays?: number;   // mode 'last'
  readonly from?: string;       // RFC3339 UTC — modes 'after' and 'between'
  readonly to?: string;         // RFC3339 UTC — modes 'before' and 'between'
}
```

Every field is `readonly`, not just the container. Svelte 5 `$state` deep-proxies the
object, so `tf.mode = 'after'` is a *reactive* mutation that would slip past a
`readonly TimeFilterState` annotation on the holder — the exact defect the table-sorting
slice-1 review caught on `SortState`.

Exported functions:

| Function | Purpose |
|---|---|
| `toParams(tf)` | → `URLSearchParams` for the wire |
| `fromParams(sp, fields, fallback)` | ← wire/URL, dropping anything the page does not offer |
| `validate(tf)` | `from <= to`; both present for `between`; `lastDays >= 1` |
| `describe(tf)` | human caption, e.g. "Last seen after 1 Aug 2026, 14:20" |
| `localToUtc(s)` / `utcToLocal(s)` | the conversion, with the defaulting rule below |

**Defaulting rule.** A value with no time component becomes local midnight *starting*
that day for `from`, and local midnight *starting the following day* for `to` — which,
against the half-open interval in §3, makes "between 1 Aug and 3 Aug" cover all of
3 August. Truncating `to` to the start of its own day instead would silently drop the
whole final day, which reads as a data bug rather than a boundary convention.

## 3. Wire format

Three parameters, identical across all four list routes:

- `time_field=<column>` — validated against a per-route whitelist. An unlisted value is
  a **400 that names the allowed set**, matching how `sort=` already behaves. It is
  never a silently ignored parameter.
- `from=<rfc3339>` — lower bound, **inclusive**.
- `to=<rfc3339>` — upper bound, **exclusive**.

The interval is half-open, `from <= col < to`. An inclusive `to` would have to be
expressed as the last representable instant of the period, and `23:59:59.999` silently
drops the final millisecond because `timestamptz` stores microseconds. Half-open has no
such gap. The UI still *presents* a whole-day `to` as that day being included, because
that is what "between 1 and 3 August" means to the person typing it — the conversion in
§2 is what reconciles the two.

`since_days` is kept, with its meaning unchanged, and is **ignored when `from` or `to`
is present**. Every existing bookmark, every other dashboard caller, and every
non-dashboard client keeps working untouched. Precedence is one-directional and stated
in the route docs so the two systems can never disagree about which won.

### Shared resolver

One function in `backend/bins/sauron-api/src/routes/search.rs`, beside the existing
`resolve_window` (which it generalises and eventually replaces):

```rust
pub struct TimeWindowSpec {
    pub column: &'static str,        // from the whitelist — never caller-supplied text
    pub from: DateTime<Utc>,
    pub to: Option<DateTime<Utc>>,
    pub clamped: Option<ClampInfo>,
}

pub fn resolve_time_filter(
    default_field: &'static str,
    allowed: &[&'static str],
    q: &TimeFilterQuery,
    now: DateTime<Utc>,
    max_days: i64,
    planner: Option<Clamp>,
) -> Result<TimeWindowSpec, ApiError>;
```

`column` is a `&'static str` selected *from the whitelist by equality*, never the
caller's string passed through. The repo layer then maps it to a diesel column via a
per-resource enum. No user-supplied text reaches SQL construction at any point.

### The span clamp is mandatory

`before X` has no lower bound. On `analytics_events` — partitioned by `occurred_at`,
29 partitions — an unbounded lower bound prunes nothing and scans all of them. That is
precisely the shape of the env-scoped analytics 503: the failure is the 30s
`TimeoutLayer`, and its cost scales with retained data rather than with anything the
caller asked for.

So the **total span** is clamped to the route's `max_days`: when `to` is present and
`from` is absent, `from` becomes `to - max_days`. All four routes cap at **365** today
(`sessions.rs` and `devices.rs` inline, `EVENTS_MAX_SINCE_DAYS` for events) — note this
is a tenth of the 3650 that Issues and Occurrences use, deliberately. `persons_list`
has no window at all today and adopts the same 365. Their *defaults* differ and stay
as they are: 30 days on Sessions and Devices, 365 on Events.

The clamp is *reported*, not silent — through the `clamped: ClampInfo` field the
`SearchEnvelope` already carries, with `field` set to the resolved column. The UI
renders it the same way it renders a planner clamp today. A narrowed window that does
not say it was narrowed is a wrong answer with a 200 on it.

For Users and Devices — single tables, bounded row counts, indexed on both columns —
the same clamp applies for uniformity. There is no benefit to two different rules.

### Repo signature change

Every affected repo function currently takes `since: DateTime<Utc>`, a lower bound
only. Each gains a window struct carrying the column choice and the optional upper
bound instead. Affected: `list_devices`, `list_device_groups`, `list_persons` (**both**
query shapes — the direct `event_users` path and the `event_user_environments` rollup
path), `search_sessions`, `count_sessions`, and the analytics events list.

`count_*` must take the identical window as its `search_*` counterpart. A total
computed over a different window than the rows is a caption that contradicts the table
under it.

## 4. Fields per page

| Page | Offered | Default | Index status |
|---|---|---|---|
| Events | `occurred_at` | `occurred_at` | covered several times over |
| Sessions | `started_at`, `last_event_at` | `started_at` | `sessions_app_env_time_idx`, `sessions_app_last_event_idx` |
| Users | `last_seen`, `first_seen` | `last_seen` | `event_users_app_{last,first}_seen_idx`; rollup has `event_user_env_{last,first}_seen_idx` |
| Devices | `last_seen`, `first_seen` | `last_seen` | `last_seen` only — see §6 |

`last_event_at` is labelled **"Last activity"** in the UI. There is no `ended_at` column
on `sessions`; duration is derived, and `last_event_at` is the honest name for what the
data holds.

### Note on Devices semantics

`list_devices`' window decides *which devices are listed*; a device's per-environment
`first_seen` can predate the page's window (repo.rs ~7159). Selecting `first_seen` as
the window field therefore means "devices whose app-level first sighting falls in the
window", which is the intended reading. The distinction is documented on the route so
it is not later mistaken for an off-by-one.

## 5. Users page: closing the decorative-picker gap

`persons_list` gains the three parameters and a `time_field` whitelist of
`last_seen` / `first_seen`. `UsersExplorer.load()` gains the window argument it has
never had, and the window enters the `viewKey` cache key.

**Cache-key hazard.** Per `cachedview-moving-key-trap`, a clock-derived value in a
`viewKey` mints a fresh entry on every load — the cache stays wired, typed and green
while hitting zero times, and only the network panel shows it. `mode: 'last'` resolves
to an absolute instant at request time, so the key must carry the **filter's
declaration** (`last:30d`) and never the resolved timestamp.

## 6. Migration

`backend/migrations/2026-08-16-000062_devices_first_seen_index/`:

```sql
CREATE INDEX devices_app_first_seen_idx
  ON devices (app_id, first_seen);
CREATE INDEX device_env_app_env_first_seen_idx
  ON device_environments (app_id, environment_id, first_seen);
```

This mirrors what `event_users` and `event_user_environments` already have for persons,
which is why Users needs no migration and Devices does. Both query shapes are covered,
matching how the persons pair is indexed.

`down.sql` drops both.

## 7. Deliberately excluded

**`received_at` on Events.** No index, and `analytics_events` is partitioned by
`occurred_at` — a `received_at`-only window prunes no partitions and scans all 29. It
is genuinely useful for diagnosing device-clock skew, but it can only be safe as a
*secondary* predicate alongside an `occurred_at` window, which is a different feature
from the one being built. Revisit with the ingest-clamp work.

**`identified_at` on Users.** Indexed on `event_users`, but the column does not exist on
`event_user_environments` — so it would fail exactly when an environment is selected,
which is the dashboard's default state. Adding it needs a rollup column plus a backfill.

## 8. URL state

`time_field`, `from`, `to` and `since_days` round-trip through the query string on all
four pages. Events already reads `since_days` from `location.search`; Sessions, Users
and Devices gain a read-on-mount and a `replaceState` on change.

`fromParams` drops any `time_field` the page does not offer and falls back to the
page default, so a stale or hand-edited link degrades to a valid view rather than
producing a 400 on first paint.

The URL write must not itself retrigger the load effect — the same shape as the
`untrack` guard in Events' predicate effect, which exists so a page move or sort change
is not immediately reset by the effect that watches the predicate.

## 9. Testing

**Frontend** — `time-filter.test.ts`:
- local→UTC conversion, including a DST-transition date, and the `00:00:00.000` /
  `23:59:59.999` defaulting rule.
- `toParams`/`fromParams` round-trip across all four modes.
- `validate` rejects `from > to`, a `between` missing either bound, and `lastDays < 1`.
- `fromParams` drops a `time_field` the page does not offer.
- A rendering test that the field control collapses to a label at one offered field.

**Backend** — `resolve_time_filter` unit tests:
- `since_days` honoured when `from`/`to` absent; ignored when either is present.
- unlisted `time_field` → 400 naming the allowed set.
- `to` with no `from` produces `from = to - max_days` **and** a populated `clamped`.
- a planner clamp strictly tighter than the request wins; one that merely matches does
  not (the existing `resolve_window` rule, preserved).

**Route-level http tests**, one per route, on a **two-app fixture**. Single-app fixtures
return identical rows whether or not a predicate is correctly scoped, which is how the
slice-2 cross-tenant leak reached a passing test suite. Each asserts the row set for
each of the four modes and the 400 on an unlisted field.

**Note on running them:** per `backend-tests-silently-skip`, the Bash sandbox has its
own network namespace, so DB-backed tests return early while printing `ok`. These must
run with `dangerouslyDisableSandbox`, host-network containers and `max_connections=800`,
against the real baseline count — a green run at the wrong count is not a green run.

## 10. Documentation

Ships with the slice, not batched after it: `wiki/Dashboard.md` gains the time-filter
section, and the in-app `Docs.svelte` cheatsheet gains the field/mode table. Any wiki
sentence stating that these lists offer only a fixed range picker becomes false the
moment this ships and is corrected in the same change.

## Out of scope

- Wiring Persons and Devices to the query planner, and widening `firstSeen`/`lastSeen`
  beyond `R_ISSUES` in the catalog. Decision 1 keeps this open as later work.
- Saved views over a time filter — belongs with the saved-views slice.
- Any change to chart or stat-tile windows (decision 2).
- Sorting: all four pages already sort on these columns via `SortableTh`. Only Devices'
  `first_seen` sort benefits here, as a side effect of the new index.
