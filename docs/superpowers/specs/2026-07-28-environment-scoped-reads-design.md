# Environment-scoped reads — Slice 2

Date: 2026-07-28
Status: design approved, ready for planning
Predecessor: `2026-07-28-per-app-environments-design.md` (Slice 1, shipped)

## Problem

Slice 1 made environments real: they own the ingest credential, they are admin-managed,
and every signal is stamped with a provable `environment_id`. But nothing reads it. The
dashboard has no way to say "show me production", and the one place environment filtering
exists — a filter chip on the Events page — resolves an environment *name* against a
single endpoint.

So the product stores a dimension it cannot show. This slice makes environment a
first-class navigation context: a fourth level in the topbar alongside org, project and
app, threaded through every read that can be scoped by it.

## Goals and non-goals

**In scope:** the topbar environment switcher; environment threaded through every read
whose data can be attributed to one; correct per-environment aggregates for issues; the
Events environment chip retired.

**Not in scope:** environment as an RBAC boundary. That is Slice 3, and it is what turns
"which environment am I looking at" into "which environments may I look at".

## Decomposition and why it is not three separate ships

The original plan split this into S2a (reads), S2b (aggregates) and S2c (cold tier).
S2c is **deferred** — see "Deferred: the cold tier" below.

S2a and S2b are built as separate plans but **ship together**, and the topbar picker is
not exposed until both are done. The reason is that a picker which visibly ignores the
user on the Issues page — the most-used page in an error tracker — is worse than no
picker at all. It would raise an expectation the product does not yet meet, on the one
page where it matters most. Every intermediate state must be honest, so the user-visible
switch flips once.

## Locked decisions

1. **Correct aggregates, not just membership filtering.** An issue that appears under a
   given environment must show that environment's counts, not app-wide ones. Filtering
   which rows appear while leaving their numbers cross-environment produces a screen
   whose figures contradict its own filter.
2. **"All environments" is the default**, and is what the picker starts on.
3. **`ReadScope` replaces the bare `app_id`** on every telemetry read, so adding the
   dimension is a compile error at each call site rather than something to remember.
4. **A database integration harness is the real enforcement.** See "Testing".
5. **Omit the parameter for "all".** `?environment_id=<uuid>` scopes to one;
   `?environment_id=none` scopes to unattributed rows; absent means all. Back-compatible
   with every existing caller.
6. **`NULL` environment_id surfaces as "Unattributed"**, not backfilled and not hidden.
7. **The dashboard attaches environment via an axios interceptor**, not by threading a
   parameter through 22 API functions.
8. **The Events environment chip is retired**, accepting the loss of `neq` and
   multi-select.
9. **The write cost was measured before building** — and the measurement changed the
   design. Per-environment issue counts are computed on read, not maintained at ingest.
   See "No new table" below.

## Data model

### No new table: per-environment issue counts are computed on read

The original design added an `issue_environments` rollup maintained by an upsert inside
`process_error`. **Task 1's measurement killed it.** On this hardware, conflict-heavy
single-row upserts against the candidate table cost ~98µs each — roughly 15-25% added to
the per-error write path, against a 15% guardrail. The precedent for taking that seriously
is the search programme, which measured 9x write amplification on a set of GIN indexes and
dropped them on the evidence.

So the counts are computed at read time instead. When a specific environment is selected,
`list_issues` derives `times_seen`, `users_seen`, `first_seen` and `last_seen` from
`error_events` with a LATERAL subquery per returned issue, rather than reading the
denormalized columns on `issues`.

This inverts where the cost sits, and the inversion is favourable: an error tracker writes
far more often than it reads, and the read is bounded by the page size (~50 issues) while
the write happens on every single event. It also reuses the exact pattern `list_persons`
already uses and that this slice extends to `list_devices` — page first, then count per
returned row via LATERAL — so it is one idiom across the whole slice rather than two.

Three consequences worth stating plainly:

- **Under `EnvFilter::All`, nothing changes.** The default path still reads the
  denormalized columns on `issues` directly, with no join and no subquery. The extra cost
  is paid only by the user who asked for a specific environment.
- **`users_seen` becomes exact** under a specific environment. The app-wide figure comes
  from a Redis HyperLogLog and is approximate; a per-environment `count(DISTINCT
  distinct_id)` is not. The two will therefore disagree slightly, and the per-environment
  number is the more accurate one.
- **It cannot see tiered data.** Once a partition is exported to Parquet and dropped,
  those occurrences leave `error_events`, so a per-environment count over a window older
  than `TIER_HOT_DAYS` under-reports. `issues.times_seen` does not, because it was
  incremented at ingest. This is a real discrepancy at the tier boundary and is documented
  rather than solved — solving it means the cold tier work that is already deferred.

An index on `error_events (issue_id, environment_id)` supports the LATERAL; the existing
`error_events_issue_time_id_idx` leads with `issue_id` but not `environment_id`.

### Everything else needs no new table

- **`event_users` (persons)** — `list_persons` (`repo.rs:2065`) already computes its
  counts via LATERAL subqueries over `analytics_events`, `error_events` and `sessions`.
  All three carry `environment_id`, so the counts become environment-correct by adding a
  predicate inside each LATERAL. Only the outer `event_users` page needs an `EXISTS`.
- **`devices`** — `list_devices` LATERAL-joins `sessions` for its session count, which is
  directly filterable. Its `events_count` / `errors_count` columns are denormalized
  (maintained by `repo::bump_device`), so those two move to the same LATERAL treatment
  `list_persons` already uses rather than gaining a rollup. This is a small,
  self-contained improvement to code we are already touching.
- **screens** — there is no `screens` table; `screen_ctes` (`repo.rs:3063`) derives
  everything from `analytics_events.screen` and `error_events.screen`. The predicate
  drops into all four CTEs.

### Missing indexes

`sessions` and `transactions` carry `environment_id` but have no index on it. Both gain
`(app_id, environment_id, occurred_at DESC)`, mirroring the ones
`2026-07-27-000025_search_indexes` added for `error_events` and `analytics_events`.

## The scope type

```rust
/// Which environments a read covers.
pub enum EnvFilter {
    /// Every environment, plus rows with no environment. The picker's default.
    All,
    One(Uuid),
    /// Rows whose `environment_id IS NULL` — signals ingested before Slice 1, or
    /// under the old per-app environment cap. Surfaced rather than hidden so that
    /// "All" genuinely equals the sum of the individual environments.
    Unattributed,
}

/// Tenant + environment scope for a telemetry read. Replaces the bare `app_id: Uuid`
/// that ~36 read functions took, so the dimension cannot be added to some and missed
/// on others: every call site is a compile error until it supplies one.
pub struct ReadScope {
    pub app_id: Uuid,
    pub env: EnvFilter,
}
```

Applying it takes **two different mechanisms**, because the read layer is not written in
one style. Of roughly 36 read functions, only three — `list_analytics_events`,
`list_sessions`, `list_issues` — use diesel's boxed-query form. The rest are
`diesel::sql_query` with hand-written SQL strings and positional binds.

**For the three boxed queries**, a `macro_rules!` per concrete column, following the
precedent the search programme set in `crates/sauron-db/src/query_plan/issues.rs`. A
generic function bounded only by `Column<Table = …>` cannot prove diesel's downstream
`ValidGrouping`/`QueryFragment` obligations, which is why that file expands a macro once
per concrete column rather than writing one helper.

**For the ~25 raw-SQL functions**, a predicate fragment plus a bind. This repo already
does exactly this: `screen_ctes(pred: &str)` (`repo.rs:3063`) interpolates a predicate
string into four CTEs, and `performance_summary` (`repo.rs:2534`) carries the optional
-filter idiom `($3::text IS NULL OR op=$3)` baked into its SQL. The helper is:

```rust
impl EnvFilter {
    /// SQL fragment to AND into a raw query, or "" for `All`. `n` is the next free
    /// positional bind index; `One` consumes it, `All` and `Unattributed` do not.
    pub fn sql_fragment(&self, n: usize) -> String {
        match self {
            EnvFilter::All => String::new(),
            EnvFilter::One(_) => format!(" AND environment_id = ${n}"),
            EnvFilter::Unattributed => " AND environment_id IS NULL".to_string(),
        }
    }
}
```

**This is the riskiest part of the slice**, and it should be understood as such rather
than as a mechanical edit. Positional bind renumbering in hand-written SQL is exactly the
kind of change that compiles, passes a smoke test, and is silently wrong — a fragment
appended without its bind, or a bind index off by one, produces either a runtime error or
a filter on the wrong value. It is also the part `diesel::debug_query` cannot check at
all, since these queries are strings rather than diesel expression trees.

That is the argument for the integration harness being non-optional rather than a nicety:
for the majority of this slice's surface, it is the *only* mechanism that can prove the
predicate is both present and correct.

**What `ReadScope` does and does not buy.** It makes *passing* the scope compulsory — the
signature change is a compile error at every call site. It does not make *using* it
compulsory: a function body can destructure `app_id`, ignore `env`, and compile. The two
mechanisms are a pair, not belt-and-braces.

## The query planner is not involved

`sauron-query` and `sauron-db::query_plan` are fully built and tested but **wired to no
route** — `prepare()` has no caller outside tests, and `prepare.rs:38` says so
explicitly. Every live filter still flows through `filter.rs`'s allowlists and
hand-written diesel in each repo function.

This slice therefore threads environment through the **live** path only. It does not wire
the planner, which belongs to the search programme. Where the planner already models
environment — `Store::Column("environment_id")` on Occurrences and Events
(`catalog.rs:293`) — no change is needed. Where it models it as `Store::Rollup` on Issues
and hard-errors `NotYetSupported` (`catalog.rs:256`, `prepare.rs:116`), that entry
anticipated a rollup table this slice ultimately did **not** build — the measurement in
Task 1 sent us to compute-on-read instead. So `Store::Rollup` on Issues stays unimplemented
and keeps erroring, and whoever wires the planner will need to either point that dimension
at the LATERAL form this slice uses or keep rejecting it. Flagged here because the catalog
comment currently promises a table that will not exist.

## Wire contract

| Value | Meaning |
|---|---|
| parameter absent | all environments, including unattributed |
| `?environment_id=<uuid>` | that environment only |
| `?environment_id=none` | rows with `environment_id IS NULL` |

An unknown or malformed UUID is a `400`, not a silent fallback to "all" — falling back
would show a user more data than they asked for, which is the wrong direction to fail.

Of the 38 read handlers in the API, **26 touch a signal table** and are therefore
environment-relevant. One (`events/list`) already filters by environment name and is
converted to the new contract. Three (the timeseries endpoints) are deferred and reject
the parameter — see "Deferred: the cold tier". That leaves **22 handlers to scope**,
backed by roughly 36 repo functions.

The remaining 12 cannot be attributed to an environment at all — monitors
(project-scoped, with no app link), alert and notification config, saved funnels, symbol
artifacts and `/v1/admin/storage`. They are unchanged, and reject the parameter as
unknown rather than accepting and ignoring it.

## Frontend

`sessionStore` gains a fourth level following the existing chain exactly: a
`sauron.environment_id` key, a `loadAppEnvironments` loader, and a
`resolveCurrentEnvironment` resolver. The resolver picks the environment flagged
`is_default` rather than `[0]`, since `Environment` carries that flag and Slice 1
guarantees exactly one live default per app. `setApp` becomes async to load
environments, changing its two call sites (`Topbar.svelte:65` and `selectApp`).

`can()` and `CanScope` are **untouched**. Environment is a selection level, not an RBAC
scope type, until Slice 3.

**Request attachment** is a single interceptor in `lib/api/client.ts`, which today has no
notion of scope. It appends `environment_id` from the store, with an explicit opt-out
list for reads that must not be scoped: `listEnvironments` (it is the source of the
list), `getApp`, `listSavedFunnels`, `listArtifacts`. This removes what would otherwise
be a 22-function, 23-call-site, 22-wrapper diff.

**Re-fetching** is where the interceptor does not help: a page whose effect does not
observe the environment will keep showing the previous one's data. Slice 1's review
caught exactly this bug in `Docs.svelte`, and here there are 24 sites. So the store
exposes `scopeKey`, a derived `${appId}:${envId}`, and every telemetry effect keys on it
instead of `currentAppId`. It is greppable, and a test asserts every telemetry page
references it.

The picker is a fourth `SwitcherMenu`, which needs no component changes — it is already
generic. Its items are `All environments`, each live environment, and `Unattributed`.
Note `.left` in the topbar is `overflow: hidden` with a 180px name cap
(`SwitcherMenu.svelte:240`, tightening to 110px below 640px), so four triggers plus three
separators will truncate on narrow viewports. The environment switcher is therefore the
first to collapse: below 860px it drops its `Env` label chip and shows only the
environment name, and below 640px the project switcher's name is hidden in favour of
keeping app and environment legible — those two are what change the meaning of the data
on screen.

**The Events chip is retired**: the field definition in `filters.ts`, the runtime option
injection in `Events.svelte`, and the docs row describing it. `parseFilters` already drops
unknown fields silently, so an existing shared link carrying
`filter=environment:eq:prod` still loads — it simply no longer constrains environment.
The backend's `EVENT_FILTERS` entry is retained for API back-compatibility.

## Error handling

| Condition | Response |
|---|---|
| Malformed `environment_id` | `400` |
| `environment_id` naming an environment in another app | `404` |
| `environment_id` naming a retired environment | accepted — retired environments keep their history, and Slice 1 made that an explicit property |
| Parameter sent to an endpoint that cannot scope by environment | `400` unknown parameter |

## Testing

**A database integration harness — new for this repository.** Slice 1's final review named
its absence as the weakest point in that slice, and this slice cannot honestly claim
enforcement without it. It seeds one app with two environments plus rows carrying a NULL
environment, then drives **every** read function with `EnvFilter::One` and asserts nothing
belonging to the other environment comes back, and with `EnvFilter::Unattributed` that
only NULL-environment rows come back.

This is what covers the reads `diesel::debug_query` cannot see — `screen_ctes`'s CTEs,
`list_persons`'s LATERAL joins, and `default_partition_counts_by_day`'s raw SQL built
from a table name. It tests behaviour rather than SQL text, which is the difference
between proving the predicate is applied and proving it was mentioned.

**Aggregate correctness** gets its own cases: an issue with occurrences in two
environments must report each environment's counts under that environment's filter, and
their sum under "All".

**A frontend test** asserts every telemetry page keys its effect on `scopeKey`.

**The measurement that changed the design.** Task 1 benchmarked the proposed per-event
rollup upsert before it was built, on the reasoning that the search programme had measured
9x write amplification for a set of GIN indexes and dropped them on that evidence. The
result — ~98µs per conflict-heavy upsert, roughly 15-25% added to the per-error write path
— exceeded the 15% guardrail, and the design moved to compute-on-read. That task is
retained in the plan as executed history, not as pending work: it is the reason the shape
of this slice is what it is.

**A read-cost check** replaces it. The LATERAL form is bounded by the page size, but that
should be verified rather than assumed: `EXPLAIN (ANALYZE, BUFFERS)` on the Issues list
under a specific environment, with the page limit applied before the subquery, on a
realistically-sized `error_events`. If the plan aggregates the whole history instead of the
returned page — the exact regression `list_persons`' own comment warns about — stop and
report rather than shipping it.

## Deferred: the cold tier

Three timeseries endpoints (`/errors/timeseries`, `/events/timeseries`,
`/transactions/timeseries`) read across hot Postgres and cold Parquet via
`tier_read.rs`. `environment_id` **is** present in the Parquet files — the tier export is
`SELECT *` — but it is not a hive partition key
(`crates/sauron-tier/src/layout.rs:85` pins the key at `app_id`/`year`/`month`), so an
environment-filtered cold read would open every month-file for the app and filter
row-wise with no pruning.

**The dashboard calls none of these three endpoints.** So rather than build cold support
speculatively, they are left unchanged and **reject an `environment_id` parameter with a
`400`** naming the limitation. This is chosen over silently returning hot-only numbers,
which would be wrong in a way nobody could see. When something needs them, cold support
becomes its own slice, and the three-way hot/`_default`/Parquet merge invariant
(`tier_read.rs:96`) has to hold under the new predicate.

## Known gaps carried forward

- Environment is not in any URL, so a shared link resolves against the recipient's
  selection. This is already true of org, project and app; environment makes it more
  visible because it changes what the numbers mean. Making the selection shareable is a
  larger change than this slice — it would need a carrier on the 20-odd pages that sync
  nothing to the URL today.
- Retiring the chip loses `environment != x` and multi-environment selection. Accepted.
- Monitors remain project-scoped with no app or environment link, so they sit outside the
  environment model entirely.
