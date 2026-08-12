# Persons list: 30s/503 under `environment_id` — design

Date: 2026-08-12
Status: **implemented and measured** (uncommitted). See "Measured" at the end.

Two things in this document were wrong when written and are corrected in place below, flagged
`CORRECTED:` — the rollup's foreign key target, and the `sessions` index column list.

## The bug

`GET /v1/apps/{app_id}/persons?sort=last_seen&limit=51&offset=0&environment_id={env}`
returns **503 after ~30.1s**.

The 503 is the 30s `TimeoutLayer` in `bins/sauron-api/src/main.rs:912` mapping a request
timeout onto `SERVICE_UNAVAILABLE`. It is **not** either of the two *named* 503 gates
(`schema_migration_required`, `busy`), which live only in `routes/active_users.rs` and touch
no route in this path. Same failure mode as the previously diagnosed
`overview/totals` / `users/summary` / `device-groups` 503s; `/persons` is the one query that
never received that fix.

`REQUEST_TIMEOUT_SECS` (`main.rs:38`) is a hardcoded `const`, not env-configurable. Raising it
is explicitly **not** part of this design: the layer exists to stop slow requests pinning
connections and pool slots, so a longer timeout trades one endpoint's 503 for pool exhaustion
across all of them.

## Root cause

`repo::list_persons` (`crates/sauron-db/src/repo.rs:7380`). Under `EnvFilter::One`:

1. **Membership.** `event_users` carries no `environment_id`, so membership in an environment
   is derived by three **correlated** `EXISTS` over `analytics_events` / `error_events` /
   `sessions` (`repo.rs:7423`). This is the exact shape that was rewritten to an uncorrelated
   `IN (… UNION …)` in `event_user_membership_exists` (`repo.rs:7661`) for a measured
   32.6s → 3.5s. `list_persons` open-codes its own copy and was left behind.

2. **Per-person LATERALs.** Three `LEFT JOIN LATERAL` aggregates run **once per admitted
   person**, not once per page row.

3. **Blocking sort.** Under a scoped read `last_seen` is
   `GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event)`, an aggregate over three
   other tables. A blocking `Sort` must consume every input row, so `LIMIT 51 OFFSET 0` caps
   nothing. The existing doc comment records 900 → 31463 planner cost (35.0x) under `One` on a
   2,000-row fixture, and states plainly that no index can buy the scoped case back.

4. **Missing indexes.** The LATERALs and the membership legs probe on
   `(app_id, distinct_id, …)` filtered by `environment_id`, but the only usable index is
   `analytics_distinct_idx (app_id, distinct_id, occurred_at DESC)` — **no `environment_id`**.
   Each probe therefore scans 29 partitions and filters environment afterward, ×3 tables,
   ×N persons. Migration `2026-08-11-000053_env_device_indexes` added exactly the right index
   for the **device** axis (`(app_id, device_key, environment_id, ts)`); the `distinct_id`
   twin does not exist.

There is also **no time window at all** on this query — no `since` parameter, and `ILIKE '%'`
on an unsearched page — so cost scales with total retained data, not with a window.

Note that (2) is paid under **every** scope, not just `One`: `event_users` was never
denormalized with lifetime counters the way `devices` was, so `events_count` / `errors_count`
read their LATERALs even under `EnvFilter::All`.

## Decisions taken

| Decision | Choice | Rejected alternative |
|---|---|---|
| Scope of fix | Slice A **and** Slice B, together | A alone (leaves the blocking sort) |
| Backfill cutover | One-shot backfill + per-app marker + fallback | In-migration backfill (boot outage); cut over immediately (silent wrong data) |
| Which scopes read the rollup | All four `EnvFilter` variants | `One`/`Unattributed` only (leaves `All` paying the LATERALs) |
| Counter accuracy | Delta-maintained, drift accepted — same trade as `devices` | Reconcile job; rollup-for-membership-only |
| Request timeout | Unchanged at 30s | Raising it / making it env-configurable |

Also rejected, carried over from the `overview_totals` fix: bounding the membership `EXISTS`
by `since` would prune hardest but changes metric semantics, and the existing doc comments
weigh and reject it deliberately.

---

## Slice A — make the live query cheap

No semantic change, no write-path change. A is not throwaway work: it remains the fallback
path for apps whose backfill has not completed.

### A1. Missing indexes

New migration `2026-08-12-000055_env_person_indexes`, mirroring `…-000053`:

- `analytics_events (app_id, distinct_id, environment_id, occurred_at)`
- `error_events (app_id, distinct_id, environment_id, occurred_at)`
- `sessions (app_id, distinct_id, environment_id, started_at, last_event_at)`

**CORRECTED:** the `sessions` entry originally read `(…, last_event_at DESC)`. The sessions
LATERAL aggregates **both** `min(started_at)` and `max(last_event_at)`, so both columns belong
in the index — as migration 53's device twin already does. Carrying only one puts the heap
fetch straight back.

`analytics_events` and `error_events` are partitioned parents: `CREATE INDEX` on the parent
recurses to all 29 partitions and builds them synchronously. Once a binary embedding this
migration ships, `require_current_schema` refuses to boot until it is applied, so this needs a
maintenance window — same operational note as migration 53.

### A2. Membership rewrite

Delete `list_persons`' open-coded three-`EXISTS` block (`repo.rs:7423`) and call the existing
`event_user_membership_exists(scope.env.clone(), 5)` instead. Bind index 5 is unchanged, so no
bind renumbering. This removes a duplicate rather than porting a second copy of it.

### A3. Expected outcome

Removes the per-probe partition scan and the correlated-EXISTS cost. Does **not** remove the
blocking sort, so residual cost still scales with the app's `event_users` count. A's measured
result is the input to deciding whether B's rollout can be staged or must be immediate.

---

## Slice B — `event_user_environments` rollup

### B1. Schema

Migration `2026-08-12-000056_event_user_environments`:

```sql
CREATE TABLE event_user_environments (
    app_id          uuid        NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    distinct_id     text        NOT NULL,
    environment_id  uuid        NULL REFERENCES app_environments(id) ON DELETE CASCADE,
    first_seen      timestamptz NOT NULL,
    last_seen       timestamptz NOT NULL,
    events_count    bigint      NOT NULL DEFAULT 0,
    errors_count    bigint      NOT NULL DEFAULT 0,
    sessions_count  bigint      NOT NULL DEFAULT 0,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);
```

`environment_id` is **nullable**, and `EnvFilter::Unattributed` is a real row with
`environment_id IS NULL` rather than an absence. This is required for `All` to equal the sum
of the individual environments — the same reason `Unattributed` is surfaced rather than hidden
(`scope.rs:36`).

**CORRECTED:** this originally read `REFERENCES environments(id)`, which is wrong and would
have rejected every real environment id. Migration 33 (`env_per_project`) **renamed** the old
`environments` table to `app_environments` and created a new catalogue table in its place; a
rename preserves the OID, so `analytics_events` / `error_events` / `workflows` kept their
pre-existing foreign keys and today point at `app_environments` — despite their original DDL
text still saying `environments(id)`. The value carried in every signal table's
`environment_id`, and handed to `EnvFilter::One` by the API, is an `app_environments.id`.
Verify with `pg_constraint`, never by reading the migration source. Caught by a foreign-key
violation in `person_env_upsert_widens_the_seen_window_in_both_directions`.

A nullable column cannot serve as a primary key, so uniqueness is a unique index over the
coalesced key:

```sql
CREATE UNIQUE INDEX event_user_env_key_idx
    ON event_user_environments (app_id, distinct_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));
```

The upsert's `ON CONFLICT` must name this index's expression list exactly, or it silently
becomes an unconstrained insert. Sort-supporting indexes, one per sortable column:

```sql
CREATE INDEX event_user_env_last_seen_idx  ON event_user_environments (app_id, environment_id, last_seen DESC);
CREATE INDEX event_user_env_first_seen_idx ON event_user_environments (app_id, environment_id, first_seen);
CREATE INDEX event_user_env_events_idx     ON event_user_environments (app_id, environment_id, events_count DESC);
CREATE INDEX event_user_env_errors_idx     ON event_user_environments (app_id, environment_id, errors_count DESC);
CREATE INDEX event_user_env_sessions_idx   ON event_user_environments (app_id, environment_id, sessions_count DESC);
```

Backfill marker, in the same migration:

```sql
CREATE TABLE event_user_env_backfill (
    app_id       uuid        PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    completed_at timestamptz NOT NULL
);
```

A dedicated table rather than `runtime_settings`, because the marker is per-app and needs a
foreign key.

### B2. Write path

`PersonEnvBump` in `crates/sauron-db/src/batch.rs`, beside `DeviceBump` (`batch.rs:280`),
which is the template:

```rust
pub struct PersonEnvBump {
    pub app_id: Uuid,
    pub distinct_id: String,
    pub environment_id: Option<Uuid>,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub events_delta: i64,
    pub errors_delta: i64,
    pub sessions_delta: i64,
}
```

Two timestamp fields, not one: the conflict arm drives `first_seen` through `LEAST` and
`last_seen` through `GREATEST`, so collapsing a batch to a single timestamp would move
`first_seen` forward to the newest signal in the group — the same reasoning already recorded
on `SessionBump::first_at` (`batch.rs:205`).

- Folded in `Acc` (`crates/sauron-pipeline/src/batch.rs:115`) keyed on
  `(app_id, distinct_id, environment_id)`, alongside `devices` / `device_at`.
- Written by `bump_person_envs`, copied from `bump_devices` (`batch.rs:300`).
- **Rows sorted by the conflict key before the upsert**, so concurrent batches take row locks
  in the same order. `bump_devices` already does this, and the ingest path has already
  produced one deadlock (`users_seen` vs. the issue upsert) that was invisible because the
  worker's stdout was being discarded. This adds a third row-lock participant.
- `sessions_delta` is folded from the same signal that produces `SessionBump`, which already
  carries `distinct_id`, `environment_id`, `first_at`/`last_at` and both other deltas
  (`batch.rs:200`) — every input already exists at that point in the pipeline.
- **`sessions_delta` is +1 only when the session row is newly inserted, not on every bump.**
  `SessionBump` folds repeated signals for one `session_id` within a batch, but the same
  session is bumped again by every subsequent batch, so a naive +1 per fold entry counts one
  session many times. `bump_sessions` must report which keys inserted (`xmax = 0` in a
  `RETURNING` clause, or `RETURNING (created_at = updated_at)`) and only those contribute.
  `events_delta` / `errors_delta` have no such problem — they are genuinely additive per
  signal.
- Rows with an empty `distinct_id` are not folded. `event_users` has no such row, so emitting
  one would create rollup entries with no `event_users` counterpart.

`crates/sauron-pipeline/src/process.rs` (the unbatched path, `process.rs:563`) gets the
equivalent single-row upsert so the two paths stay equivalent.

### B3. Backfill and marker

A one-shot subcommand (not a migration step): for each app, aggregate the three source tables
into `event_user_environments`, then insert the `event_user_env_backfill` row **in the same
transaction as that app's final batch**. The marker must never be visible before the data it
claims — that ordering is the only thing standing between this design and a silently empty
persons page.

Not run inside the migration because `require_current_schema` fail-closes the API on a stale
schema: an in-migration aggregate over 29 partitions is a boot outage proportional to
retained data.

**Concurrent ingest.** The write path (B2) is unconditional — it bumps the rollup from the
moment the migration lands, including for apps that are not yet backfilled. So the backfill
cannot use `ON CONFLICT DO NOTHING`: a live bump that creates the row first would make the
backfill skip it, leaving that person permanently short by their entire history.

Instead the backfill is **additive against a cutoff**. It records `T0` at start, aggregates
only source rows with `occurred_at < T0` (`started_at < T0` for sessions), and upserts with
`ON CONFLICT DO UPDATE SET events_count = event_user_environments.events_count +
EXCLUDED.events_count` (and likewise for the other two counters, with `LEAST`/`GREATEST` for
the timestamps). Live bumps arriving during the backfill carry events at or after `T0`, so the
two sets are disjoint and the addition is correct.

Known residual: a **backdated** event — an SDK offline queue replaying with an old
`occurred_at` — that arrives between `T0` and the backfill finishing is counted twice, once by
the live bump and once by the cutoff scan. Bounded by the backfill's duration, and drift is
already accepted (decision table above), so this is recorded rather than engineered away.

While unmarked, reads use the live path, so a partially-populated rollup is never read.

### B4. Read path

`list_persons` branches on the marker for `scope.app_id`:

- **Unmarked** → today's query, now A-optimised. Unchanged behaviour.
- **Marked** → `event_user_environments` is the sole source for membership, `first_seen`,
  `last_seen` and all three counts:
  - `One` / `Unattributed` — read the matching row directly.
  - `All` / `Subset` — `GROUP BY distinct_id` with `min(first_seen)`, `max(last_seen)`,
    `sum()` on the three counts, filtered by the `EnvFilter` fragment.

`Subset` is a distinct variant, not a flavour of `One` (`scope.rs:35`), and is the easiest of
the four to omit by accident.

`event_users` is still joined for `properties` (which stays app-wide and is deliberately not
per-environment — see `PersonRow`'s doc comment) and for the `ILIKE` search over
`distinct_id` / `properties::text`.

The person subquery keeps the alias `eu`, because `person_sort_spec`
(`routes/analytics.rs:192`) emits the qualified column `eu.distinct_id`. Every other sort
column is an unqualified output alias resolved against the select list, so `SortSpec` needs no
change provided the rollup branch reuses the same aliases: `first_seen`, `last_seen`,
`events_count`, `errors_count`, `sessions_count`.

With this branch the `ORDER BY … LIMIT` applies to a single indexed table, so paging is
bounded by page size again. That is the actual fix for the 30s.

`nulls_last` stays uniformly `false`: rollup rows are `NOT NULL` on all five sort columns, so
the existing measured claim in `person_sort_spec`'s comment still holds — but it now holds for
a second reason and the comment should say so.

---

## Hazards

1. **Silent-empty.** A marker written before its data turns the persons page quiet-wrong
   instead of erroring. Mitigated by the same-transaction ordering in B3; a test asserts a
   partially-backfilled app still reads the live path.
2. **Counter drift, accepted.** After cold rotation to Parquet, rollup counts will *exceed* a
   live `COUNT(*)` over hot Postgres. That is the more correct number, but it will read as a
   bug to whoever compares them next, so it goes in the doc comment rather than only here.
3. **`All` regression risk.** Today `All` reads `eu.first_seen` / `eu.last_seen` from durable
   `event_users` columns that cannot be wrong. Moving `All` onto the rollup makes it depend on
   backfill correctness. The marker fallback bounds this, and the equivalence tests below
   cover it directly.
4. **Two query shapes.** `list_persons` will carry both branches until every deployment is
   backfilled. This is a real maintenance cost and was accepted explicitly; the alternative
   (in-migration backfill) trades it for a boot outage.
5. **Deadlock surface.** See B2 — lock ordering is not optional here.

## Verification

Per the known harness defect, backend suites must run with `dangerouslyDisableSandbox` plus
host-network containers and `max_connections=800`; the Bash sandbox has its own netns, so
DB-backed tests otherwise return early **while printing `ok`**. A green run that did not
actually connect proves nothing. Baseline to compare against: 1391 tests.

1. **Equivalence.** Old vs. new `list_persons` output compared across
   `All` / `One` / `Subset` / `Unattributed` × all six sort columns × both marker states.
   `crates/sauron-db/tests/env_scoping.rs` already seeds the awkward identities —
   session-only, cross-env, unattributed — and `tests/offset_sort.rs` already covers tie
   stability and computed-column ordering.
2. **Write-path fold.** `PersonEnvBump` folding matches N sequential single-row upserts,
   including the `LEAST`/`GREATEST` timestamp behaviour that a collapsed fold would get wrong.
   Separately: a session bumped across several batches increments `sessions_count` exactly
   once — the failure this catches is silent over-counting that grows with session length, and
   a single-batch test cannot see it.
3. **Backfill additivity.** An app receiving live ingest *while* its backfill runs ends with
   counts equal to a from-scratch aggregate. This is the check that would catch the
   `DO NOTHING` mistake the design originally made.
4. **Backfill marker.** A partially-backfilled app reads the live path; a fully-backfilled app
   matches the live path's output exactly.
5. **Migration.** Applies cleanly to a fresh DB; all indexes present; `COALESCE` unique index
   actually enforces the intended uniqueness (assert a duplicate insert is rejected for both a
   real `environment_id` and `NULL`).
6. **Measurement.** Before/after timings on the real reported query, reported as numbers.
   Measure three points — today, after A, after B — so A's contribution is separable rather
   than inferred.

Measurement traps carried over from the earlier env-scoping work, both of which have already
produced misleading results once:

- A fixture whose events all fall **inside** the query window makes a time-bound fix look
  useless. Production's cost is the data *outside* the window that still gets scanned.
- A **single-app / single-environment** fixture has every row matching, so the planner
  correctly prefers a seq scan and any new index appears to change nothing. Index wins only
  appear with many apps and environments.

## Out of scope

- Raising or externalising `REQUEST_TIMEOUT_SECS`.
- Adding a `since` window to the persons list (changes semantics).
- A drift-reconciliation job for the rollup counters.
- The `devices` / `get_device` paths, already fixed by migration 53.

---

## Measured (2026-08-12)

Three points on one fixture, `EXPLAIN (ANALYZE, BUFFERS)` over the **exact** strings
`list_persons` emits — captured from the running build via
`repo::list_persons_sql_for_test` / `list_persons_rollup_sql_for_test`, not transcribed by
hand.

**Fixture.** 5 apps × 3 environments; target app has 20,000 `event_users`, 400,000
`analytics_events`, 60,000 `error_events`, 60,000 `sessions`, spread over ~350 days across 13
monthly partitions. Query: `sort=last_seen&limit=51&offset=0&environment_id=<env0>`.

| point | change | execution | planner cost | seq scans | sort key |
|---|---|---|---|---|---|
| 1 | baseline (correlated `EXISTS`, no env-person indexes) | **2,752 ms** | 4,586,999 | 5 | `GREATEST(max, max, max)` over 3 tables |
| 2 | after Slice A (uncorrelated `IN`, + 3 indexes) | **2,040 ms** | 2,492,384 | 5 | same |
| 3 | after Slice B (rollup, backfilled) | **37 ms** | 4,282 | 1 | `r.last_seen` — a plain column |

**Slice A alone is 1.35×. Slice B is 74×.** That is the headline, and it is not what Slice A's
prior results predicted: the same uncorrelated rewrite measured 9.3× on `overview_totals`.

The reason is visible in the plans. A removes membership-probe cost — planner cost drops 46% —
but the plan *shape* does not change: 5 sequential scans remain, and the sort key is still an
aggregate over three other tables, so the blocking `Sort` still consumes every admitted person
before `LIMIT 51` applies. A makes the slow plan cheaper; it does not make it a different plan.
Only the rollup changes the shape, and the 1,071× drop in planner cost is that change.

**So the decision to do A+B together was the right one, and A alone would not have fixed the
reported 503.** If A had shipped alone, this endpoint would have gone from ~30 s to ~22 s —
still over the 30 s `TimeoutLayer` under any additional load, and still degrading with
retained data.

**Backfill cost, measured on the same fixture:** 520,000 source rows → 108,000 rollup rows in
**5.0 s** for all 5 apps (3.5 s for the 20k-person app). The rollup's summed counters equal the
source row counts exactly (400,000 / 60,000 / 60,000), which is an independent check that the
three `UNION ALL` legs neither drop nor double-count.

### What this fixture does NOT establish

Stated so the next reader does not over-claim these numbers:

- **13 monthly partitions, not production's 29 daily ones.** Per-partition overhead is
  understated, so the index win (point 1 → 2) is a lower bound.
- **The baseline here is 2.7 s, the reported production failure is 30.1 s** — roughly 11× more
  work than this fixture carries. The *ratios* should hold; the absolute numbers should not be
  quoted as production figures.
- **No cold-tier data.** Every row lives in Postgres, so the divergence between rollup counters
  and a live `COUNT(*)` after Parquet rotation (hazard 2 above) does not appear here and
  remains unmeasured.
- **A 60 s `statement_timeout` cap** was set for the run; no point reached it.
