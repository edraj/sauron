# Active Users metric — design

**Date:** 2026-08-09
**Status:** approved and IMPLEMENTED 2026-08-09 (backend; dashboard chart still to wire)
**Supersedes:** the unruled placeholder in `2026-08-01-active-users-design.md`

## The ruling

> An active user is a **distinct `distinct_id` per UTC day**, falling back to the
> anonymous id when the person is unidentified.

Everything below follows from that sentence. It is written down because "active
users" is the single most reinterpretable number in an analytics product, and the
cost of changing the definition later is that every historical chart silently
changes shape.

## Why this definition and not the alternatives

**Session-based** (distinct sessions per day) was rejected. It is cheaper to
compute and immune to identity churn, but it overcounts one person across devices
and undercounts a person whose session spans the boundary. It measures traffic,
not people, and the metric is called Active *Users*.

**Identified-only** (count only people who have called `identify()`) was rejected
by the fallback clause. Most events from a logged-out or pre-signup user carry
only an anonymous id, and excluding those makes the number a measure of *signup
rate* plotted against time. Note this is NOT the same as excluding rows with an
EMPTY `distinct_id`, which is what the implementation does and which is discussed
under Identity: an anonymous id is an identity, an empty string is the absence of
one.

## Semantics, stated precisely

- **Bucket:** UTC calendar day. The existing per-day endpoints already bucket on
  `(occurred_at AT TIME ZONE 'UTC')::date` on the hot side and
  `CAST(occurred_at AS DATE)` with `SET TimeZone='UTC'` on the cold side, and
  those two must agree — see `sauron-tier`'s `DuckEngine::open`.
- **Identity:** `analytics_events.distinct_id`, and nothing else.

  **Correction to this spec, made during implementation.** It previously implied
  a *second* column to fall back to. There is none: `analytics_events` has no
  `anonymous_id` column. The anonymous id **is** the `distinct_id` an
  unidentified client sends — the browser and Flutter SDKs store `anon_<uuid>`
  and put it there until `identify()`. So "distinct id, falling back to the
  anonymous id" is one column, not two, and the ruling is satisfied as written.

  This is also why the Flutter anonymous-id parity work matters here: an id
  scheme that churns on upgrade shows up in this metric as a spike in new users
  that never happened.
- **Empty `distinct_id` is EXCLUDED, not counted.** The column is
  `NOT NULL DEFAULT ''`, so empty means "this client sent no identity at all" —
  server SDKs by design, and mobile clients predating the anonymous id. The only
  other candidate to fall back to is `device_key`, and counting devices inside a
  metric named Active *Users* answers a different question: one person on a phone
  and a tablet becomes two, and the number moves whenever someone reinstalls.
  Counting them together under the empty string would be worse still — every
  anonymous event in the deployment would collapse into one perpetual user.
  Measured on the largest app in the dev dataset, 0 of 212,415 rows have an empty
  `distinct_id`, so today this excludes nothing; it is a rule for traffic that
  arrives later.
- **Source table:** `analytics_events` only. Error events are not user activity —
  a crash loop from one device would otherwise read as engagement.
- **A user active on two days counts once per day**, never deduplicated across
  the range. "DAU averaged over 30 days" and "distinct users in 30 days" are
  different numbers and only the first is what this endpoint returns.

## The part that is genuinely hard: cross-tier COUNT(DISTINCT)

**`COUNT(DISTINCT)` is holistic, not additive.** Every existing cross-tier metric
in this codebase is a plain row count, which is why `tier_read.rs` can add the
hot and cold halves together and be exactly right. Distinct counts cannot be
added: a user active both before and after the watermark would be counted twice,
and there is no way to detect that from two independent totals.

This is the same reason `transaction_counts_by_day` is cross-tier while
transaction *percentiles* are documented as hot-only.

Three options, in the order they should be considered:

1. **Per-day distinct is safe across tiers when the watermark falls on a day
   boundary.** A given UTC day lives entirely in one tier unless the watermark
   cuts through it. Since partitions are day-granular by default
   (`TIER_GRANULARITY=day`) and the watermark only ever advances to a partition
   *end*, the watermark IS a day boundary in the default configuration. So
   per-day distinct counts can be computed independently per tier and
   concatenated — not summed — with at most one day needing care.
   **This is the recommended implementation**, and the boundary day must be
   handled explicitly rather than assumed away, because `TIER_GRANULARITY` is
   configurable to `week`/`month` and a week-granular watermark is not a day
   boundary.
2. **HyperLogLog sketches** stored per day at ingest, merged across tiers. Correct
   for any granularity and cheap to merge, but it is approximate, and an
   analytics number that is quietly ±2% invites a support ticket the first time
   someone reconciles it against a manual query.
3. **Hot-only**, documented as such, like the percentiles. Honest and trivial, but
   it makes the chart silently truncate at the rotation age — which is exactly
   the failure the cold-restore work exists to avoid.

**Decision: option 1**, with the straddling-day case detected and either (a)
served from whichever tier holds the majority of that day with a documented
caveat, or (b) excluded from the response with an explicit `partial_days` field.
(b) is preferred — a missing point an operator can see beats a wrong point they
cannot.

## Endpoint

```
GET /v1/apps/{app_id}/analytics/active-users?days=30
→ { series: [{ day: "2026-08-01", count: 1234 }], partial_days: ["2026-07-11"] }
```

- Gated on `event:read` at the app scope, consistent with the other analytics
  aggregates. Note this is an **aggregate**, so under the D4 ruling it does *not*
  additionally require `issue:read` — it exposes no event body and no issue
  metadata.
- Environment-scoped through the existing interceptor/`ReadScope` path, like every
  other app-scoped analytics read.

## Dashboard

**Correction, made when wiring it up (2026-08-09).** This section named the wrong
page. It said the series is "the per-app per-day series that page's chart needs" —
but `ActiveUsers.svelte` already renders a per-day chart, and that page is
**project**-scoped with an identified-vs-guest split. This endpoint is
**app**-scoped. Putting it there would have placed an app-scoped number on a
project-scoped page, next to a differently-scoped chart of the same name.

It is wired into **`Overview.svelte`** instead, where the other app-scoped
analytics series live and where the app selector already establishes the scope.
The two genuinely coexist, but along a different axis than this section claimed:
the project report answers "which apps/environments, identified or guest" and is
hot-only (it reports `truncated` past the rotation age); this one answers "how many
distinct people per day for this app" and reads across tiers, so it keeps
answering.

Cache key must carry `sessionStore.scopeKey` as everywhere else.

## Testing

The tests that actually matter, in priority order:

1. **A user active on two days counts on both; a user active twice in one day
   counts once.** This is the definition; if only one test survives, this is it.
2. **Empty `distinct_id` is excluded**: a day containing only identity-less
   events has NO active users, not one. (This item previously described a
   fallback that the schema does not support — see the Identity correction above.)
3. **Cross-tier**: a range straddling the watermark returns each day exactly once
   and never double-counts a user active on both sides.
4. **The straddling day** is reported in `partial_days` rather than silently
   halved.
5. **Environment scoping**: a user active only in `prod` does not appear in the
   `staging` series.

Mutation-test at least (1) and (3) — a `COUNT(DISTINCT)` accidentally written as
`COUNT(*)` passes every smoke test on single-event-per-user fixtures.

## What this design does NOT cover

- WAU/MAU rollups. They are not simply sums of DAU and need their own ruling.
- Retention/cohort curves.
- Backfill of the metric over already-cold data beyond what the cross-tier read
  gives for free.

## Implementation notes (added 2026-08-09)

- `repo::active_users_by_day_hot` (hot) and `DuckEngine::distinct_users_by_day`
  (cold), assembled by `tier_read::active_users_by_day`, which CONCATENATES rather
  than merges and returns `partial_days` for any day present in both tiers.
- Route: `GET /v1/apps/{app_id}/analytics/active-users?since_days=N`.
- 8 DB tests, **5 mutations all caught**. Two of those tests had to be
  strengthened before they discriminated: the half-open test placed its boundary
  event at 12:00 rather than at the exclusive instant itself, and the UTC-bucket
  test could not distinguish `occurred_at::date` from
  `(occurred_at AT TIME ZONE 'UTC')::date` until it forced a non-UTC session
  timezone. Both were passing for the wrong reason.
- **Done 2026-08-09:** wired into `Overview.svelte` (not `ActiveUsers.svelte` — see
  the Dashboard correction above), with `partial_days` surfaced as named text
  rather than an unexplained gap. HTTP-level coverage added in
  `bins/sauron-api/tests/http_active_users.rs` (route, `event:read` gate, env
  narrowing), mutation-checked.
