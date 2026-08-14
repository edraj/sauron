# Guest → identified person merge

**Date:** 2026-08-12
**Status:** Design approved, ready for implementation plan
**Scope:** backend (`sauron-pipeline`, `sauron-db`, `sauron-tier`, `sauron-ingest`) + `js`/`flutter` SDKs

## Problem

When an anonymous visitor signs up, `identify()` records the anon → person alias
and then nothing reads it. One human is counted as two people, forever.

Concretely, `process_identify` ([process.rs:586]) does exactly three things:

1. upsert `event_users(u-42)` with traits,
2. stamp `identified_at` / `identified_source = 'identify'`,
3. `INSERT INTO identities(app_id, alias_id = 'anon_x', distinct_id = 'u-42')`.

It never touches `sessions`, never rewrites `analytics_events.distinct_id`, and
never folds `event_users('anon_x')` into `('u-42')`. And `identities` is read by
**no product query at all** — the only consumers are migration 000038's one-shot
backfill and the PII inspector's scan list. So the link exists as a row and
changes no number anywhere.

Result: two `event_users` rows, two `event_user_environments` rows, and every
pre-login event attributed to `anon_x` permanently.

A second, smaller defect falls out of the same area. `sessions` is
`UNIQUE (app_id, session_id)` and `bump_session` does
`distinct_id = COALESCE(EXCLUDED.distinct_id, sessions.distinct_id)`
([repo.rs:5528]), so a session's owner is last-write-wins. Since `reset()` does
not rotate the session id, one session row can serially represent
`anon_x` → `u-42` → `anon_y` and record only the last.

## Decisions

Locked during design. Each changes the shape of the work, so record the *why*.

| # | Decision | Rationale |
|---|---|---|
| D1 | **No historical backfill.** Existing data is purged out-of-band. | An app+env purge is being built separately (`2026-08-12-admin-data-purge-design.md`). This feature ships forward-only against a clean slate. |
| D2 | **Merge is retroactive within a person's own timeline.** A guest's full pre-login history becomes theirs. | Anything less leaves the same human double-counted across the login boundary, which is the bug. |
| D3 | **An alias is burned on first claim.** `anon_x → u-42` can never be re-pointed to `u-99`. | Bounds cross-user leakage on shared devices; makes the map append-only, immutable and safely cacheable. Already enforced — see D3a. |
| D3a | Already implemented: `insert_identity` is `ON CONFLICT (app_id, alias_id) DO NOTHING` ([repo.rs:5225]). | No work required for the burn itself; only for observing conflicts (§5). |
| D4 | **The pre-login marker is derived, not computed at ingest.** | Zero write-path cost, no per-event flag to backfill. |
| D5 | **Approach B: rewrite hot rows in place, overlay the alias map on cold only.** | See "Approach" below. |

### Approach

Three options were compared. The deciding constraint is that migrations
[0028], [0031] and [0039] exist **specifically** to carry `distinct_id` in an
`INCLUDE` payload so that `count(DISTINCT distinct_id)` runs as an index-only
scan.

- **A — pure read-time overlay.** Never mutate stored data; resolve
  alias → person inside every aggregation. Simplest, uniform across tiers,
  reversible by construction. Rejected: it destroys all three index-only scans
  and puts a per-row lookup in front of ~20 aggregation sites — the same query
  shape that already took `/persons` from 152 ms to 28.96 s and tripped the 30 s
  `TimeoutLayer` into a 503. It also forces ~20 call sites to change in lockstep,
  where a missed one silently returns the old double-counted number.
- **B — hot rewrite + cold overlay. CHOSEN.** Zero read-path change on hot, all
  three index-only scans preserved, read cost constant forever.
- **C — surrogate `person_id` / person table.** The correct long-term identity
  model, but it does **not** replace this work: during the guest phase no person
  exists, so past rows still carry an anon-derived `person_id` after `identify()`
  and still need B's rewrite or A's overlay. Additive scope, not alternative
  scope. Deferred; B does not block it.

## Design

### 1. Data model

`identities` is promoted from dead storage to the source of truth for identity
resolution.

**New invariant — no chains.** An `alias_id` must never already be a merge
*target*, and a known target must never be accepted as an alias. This yields:

```
resolve(app, id) = COALESCE(map[app][id], id)   -- single level
resolve(resolve(x)) == resolve(x)               -- idempotent
```

Idempotence is load-bearing. It is what makes the cold overlay safe regardless of
whether the tiering worker snapshotted a partition *before* or *after* a merge
landed: a pre-merge Parquet file holds `anon_x` and resolves to `u-42`, a
post-merge one holds `u-42` and resolves to itself. Same answer either way. This
demotes the tier race from a correctness bug to "the hot rewrite occasionally
does redundant work".

**Schema additions:**

```sql
-- metadata-only ADD COLUMN (nullable, no default); NULL on virtually every row
ALTER TABLE analytics_events ADD COLUMN guest_alias TEXT;
ALTER TABLE error_events     ADD COLUMN guest_alias TEXT;
```

The derived pre-login marker (D4) is `guest_alias IS NOT NULL`.

Deliberately **not** added to `sessions` / `transactions` / `workflows`: they get
their `distinct_id` rewritten like everything else, but "was this pre-login" is a
question about events. Easy to add later, hard to remove.

```sql
CREATE TABLE identity_merges (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id            UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    alias_id          TEXT NOT NULL,
    distinct_id       TEXT NOT NULL,          -- the surviving person
    state             TEXT NOT NULL DEFAULT 'pending'
                      CHECK (state IN ('pending','running','done','failed','dead')),
    attempts          INT  NOT NULL DEFAULT 0,
    last_error        TEXT,
    claimed_at        TIMESTAMPTZ,            -- lease stamp; also the fencing token
    next_attempt_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    alias_first_seen  TIMESTAMPTZ,            -- captured during the event_user_environments fold
    alias_last_seen   TIMESTAMPTZ,
    cold_stale        BOOLEAN NOT NULL DEFAULT TRUE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at      TIMESTAMPTZ,
    UNIQUE (app_id, alias_id)
);
CREATE INDEX identity_merges_runnable_idx
    ON identity_merges (created_at)
    WHERE state IN ('pending','failed','running');
```

**`'dead'` is a distinct terminal state, not `'failed'` with a full attempt count.**
Without it, exhausted rows stay in the runnable index forever and — being the
oldest — sit at the head of every `ORDER BY created_at` claim scan, discarded by
a heap-side filter that never shrinks.

Enum-like column as `TEXT` + `CHECK`, never a custom SQL type — house rule.

The queue exists so a merge is resumable and observable rather than fire-and-forget
inside the ingest path.

### 2. The merge job

**Enqueue.** `insert_identity` already returns `usize` — `1` on a fresh claim,
`0` when burned. Enqueue **only on `1`**, so the burn check and the merge trigger
share one atomic statement with no extra query.

> **Both identify paths must enqueue.** `process::process_identify`
> ([process.rs:586]) and the batched loop in [batch.rs:693] each call
> `insert_identity` independently. Wiring only one leaves merges silently
> not-happening on whichever path the deployment actually uses. This is the
> single most likely way to ship this feature broken.

**Where it runs.** A background task inside `sauron-ingest` — off the per-item
path, but not a new binary (a new bin means `packaging/rpm/binaries.txt` plus
systemd units for no benefit). Drains with

```sql
WHERE ( (state IN ('pending','failed') AND attempts < MAX_ATTEMPTS
         AND next_attempt_at <= now())
     OR (state = 'running' AND claimed_at < now() - <lease>) )
ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1
```

Three things that are not optional, each closing a hole that is silent:

- **The lease arm.** Without it a merge claimed as `'running'` by a worker that
  then dies — an OOM, or any deploy landing while the queue is non-empty — is
  orphaned permanently: nothing resets `'running'`, and because the alias is
  already burned, `identify()` never produces another `Fresh` claim and
  `enqueue_merge` never re-queues it. That guest never merges, and nothing logs
  it, because the process that would have logged is the one that died.
- **`next_attempt_at` backoff.** Without it a failing row is re-claimed by the
  very next iteration of the drain loop, so all `MAX_ATTEMPTS` are spent in
  milliseconds. That makes the cap effectively *one* attempt against any fault
  lasting longer than microseconds — a deadlock, a lock timeout, a connection
  blip — and turns a transient fault into permanent silent loss.
- **`attempts < MAX_ATTEMPTS` binds the pending/failed arm only.** Gating the
  lease arm with it too means a worker that dies on its last attempt strands the
  row in `'running'` forever, never reaching `'dead'`.

**`claimed_at` is also the fencing token.** A lease can reclaim a job whose
original worker is slow rather than dead, so both terminal writes carry
`AND state = 'running' AND claimed_at = <the value this claim returned>`.
`AND state = 'running'` alone is insufficient — under the second worker the row
genuinely is `'running'`; the timestamp is what tells the two claims apart.

The rotation age is resolved **once per drain pass**, not once per process:
`tier.hot_days` is operator-tunable at runtime, and a boot-time read drifts in
the *under-marking* direction for `cold_stale` — the silently-wrong one.

**Order — idempotent steps first, consuming steps last.**

| # | Table | Statement |
|---|---|---|
| 1 | `analytics_events` | `UPDATE SET distinct_id = person, guest_alias = alias WHERE app_id = $1 AND distinct_id = alias` |
| 2 | `error_events` | same |
| 3 | `sessions` | `UPDATE SET distinct_id = person` |
| 4 | `transactions` | `UPDATE SET distinct_id = person` |
| 5 | `workflows` | `UPDATE SET distinct_id = person` |
| 6 | `devices` | `UPDATE SET last_distinct_id = person WHERE last_distinct_id = alias` |
| 7 | `event_user_environments` | **fold** (consuming) |
| 8 | `event_users` | **fold** (consuming) |
| 9 | `identity_merges` | `state = 'done', completed_at = now()` |

Steps 1–6 are idempotent by construction — a re-run matches zero rows.

**Three of the six had no index at all.** An earlier version of this line
claimed all six "ride the existing `(app_id, distinct_id, occurred_at)`
indexes". That index exists for `analytics_events`, `error_events` and
`sessions` only. Measured against the real migration set:

| statement | plan | buffers | time |
|---|---|---|---|
| `analytics_events` | Index Scan | 2 | 0.03 ms |
| `transactions` | **Seq Scan, all partitions**, 300k rows filtered | 4,286 | 19.2 ms |
| `devices` | **Seq Scan**, 100k rows filtered | 1,682 | 5.1 ms |
| `workflows` | Index Scan on the app prefix + `Filter: distinct_id` | — | O(app's workflows) |

Once per signup, and scaling with total retained volume rather than with the
guest's own row count — at 10M transactions, ~1.1 GB of buffer churn per merge,
evicting exactly the cache the read path depends on. Migration 0058 adds the
three missing indexes (`transactions`/`workflows` on `(app_id, distinct_id)`,
`devices` on `(app_id, last_distinct_id)`), each `WHERE … IS NOT NULL` since
all three columns are nullable and a large share of rows never carry a person. Recovery is "run the
whole job again"; there is no per-table progress to get wrong. Each step is its
own transaction so a heavy guest does not hold one long-lived lock across all
partitions. None of them touch `occurred_at`, so **no row moves between
partitions**.

**The folds must consume their source.** A counter fold is *not* idempotent —
re-running double-counts. Written as a move:

```sql
WITH moved AS (
  DELETE FROM event_user_environments
   WHERE app_id = $1 AND distinct_id = $alias
  RETURNING environment_id, first_seen, last_seen,
            events_count, errors_count, sessions_count
)
INSERT INTO event_user_environments
       (app_id, distinct_id, environment_id, first_seen, last_seen,
        events_count, errors_count, sessions_count)
SELECT $1, $person, environment_id, first_seen, last_seen,
       events_count, errors_count, sessions_count
  FROM moved
ON CONFLICT (app_id, distinct_id,
             COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid))
DO UPDATE SET
  first_seen     = LEAST   (event_user_environments.first_seen, EXCLUDED.first_seen),
  last_seen      = GREATEST(event_user_environments.last_seen,  EXCLUDED.last_seen),
  events_count   = event_user_environments.events_count   + EXCLUDED.events_count,
  errors_count   = event_user_environments.errors_count   + EXCLUDED.errors_count,
  sessions_count = event_user_environments.sessions_count + EXCLUDED.sessions_count,
  updated_at     = now();
```

The `DELETE` is what makes it idempotent: a second run moves nothing and adds
nothing.

Two traps, both already documented in-tree:

- **The `ON CONFLICT` target must name that exact `COALESCE` expression.**
  Migration 0056 states this explicitly — the unique key is an expression index
  and the nil-uuid sentinel exists because `NULL <> NULL` would otherwise let one
  person accumulate unlimited unattributed rows.
- **`ON CONFLICT DO UPDATE` cannot affect the same row twice in one statement.**
  Safe here (the alias's own rows are unique per environment key, so no two
  `moved` rows collide on one target) but it gets an explicit test rather than an
  assumption — it is the same trap that forced derived counters in the ingest
  failure-recovery work.

`event_users` folds identically, with `properties` concatenated **anon-first**
(`EXCLUDED.properties || event_users.properties`) so the person's `identify()`
traits win, and `identified_at` / `identified_source` left untouched on the
surviving row.

`alias_first_seen` / `alias_last_seen` are captured in the **`event_user_environments`**
fold's statement, aggregated `min`/`max` across the alias's `moved` rows.

**Not from `event_users` — that is the wrong clock.** `event_users.first_seen`/
`last_seen` are written as `now()` at ingest ([`repo::upsert_event_user`]), whereas
`event_user_environments.first_seen`/`last_seen` are bound to the event's own
timestamp ([`repo::bump_person_env`]) — the same value that lands in
`analytics_events.occurred_at`. Both consumers of this span compare it against
event time: the overlay window is an `occurred_at` range over Parquet, and
`cold_stale` proxies "was this already exported", which the tier watermark decides
by `occurred_at`. Sourcing it from ingest time shifts the span later and silently
prunes an offline-flushed guest out of the overlay forever — the `hot_days - 1`
margin cannot absorb a difference of clocks.

The aggregate needs an `IS NOT NULL` guard: `min()` over an empty `moved` returns
one all-NULL row, not zero rows, so without it a re-run would blank an
already-captured span and violate `cold_stale NOT NULL`.

**Redis breadcrumbs** are keyed by `(app, distinct_id)` with a 1800 s TTL. Not
rewritten — they expire within 30 minutes and rewriting buys nothing. Documented,
not fixed.

### 3. The cold overlay

Cold Parquet is immutable, so the hot rewrite cannot reach it. One cold query
aggregates identity today — `distinct_users_by_day` ([duck.rs:293]):

```sql
SELECT CAST(occurred_at AS DATE) AS day,
       count(DISTINCT COALESCE(m.person, e.distinct_id)) AS cnt
FROM read_parquet(?, hive_partitioning=true, union_by_name=true) e
LEFT JOIN alias_map m ON m.alias = e.distinct_id
WHERE ...
GROUP BY 1 ORDER BY 1
```

The map is read from Postgres and registered as a DuckDB temp table per query.
**Not** via `postgres_scanner`: DuckDB is unbundled and vendored here, and making
a correctness-critical path depend on an extension load is a bad trade.

**The map must be bounded.** One alias per converted device per app is millions
of rows at scale. Two prunes, both written in step 7's statement (the
`event_user_environments` fold already reads those rows in order to delete them —
and, unlike `event_users`, carries event time rather than ingest time):

- **`alias_first_seen` / `alias_last_seen`** — a cold query for `[from, to)`
  loads only aliases whose span overlaps the window.
- **`cold_stale`** — if every one of the alias's rows was still hot when the
  merge ran, the rewrite fixed them *before* export, so that alias can never be
  stale in Parquet and is excluded from the overlay permanently. Most guests
  convert inside `tier.hot_days`, so this prunes the large majority.

`cold_stale` is computed conservatively as
`alias_first_seen < watermark + 1 day`, where `watermark = now() - effective_tier_hot_days`.
Over-marking costs a few extra overlay rows and a slower cold query;
under-marking is a silently wrong number. The margin covers the watermark
advancing between enqueue and execution.

**Both prunes apply only to completed merges.** Until step 8 runs, the span is
NULL and `cold_stale` is its conservative `TRUE` default, so neither prune is
safe. The overlay's selection is therefore:

```sql
SELECT alias_id, distinct_id FROM identity_merges
 WHERE app_id = $1
   AND ( state <> 'done'                              -- span unknown: never prune
         OR (cold_stale
             AND alias_first_seen < $window_end
             AND alias_last_seen  >= $window_start) )
```

Pruning an in-flight merge by a NULL span would drop it from the overlay while
its hot rewrite has not yet landed either — the one window in which the row is
stale in *both* tiers at once.

**Structural guard.** A second cold aggregation added later that forgets the
overlay would silently keep double-counting — no error, no failing test. The
resolution therefore lives in **one helper that builds the scan**
(`resolved_cold_events(glob, app, window)`), so new cold queries inherit it by
default instead of by remembering.

Already handled for free: `union_by_name = true` is in the existing
`read_parquet` call, so Parquet files written before the `guest_alias` migration
read that column back as NULL rather than failing.

**Stated limit.** The overlay makes cold *counts* correct. Cold *rows* still
physically carry the anon id, so anything reading raw cold rows and displaying a
`distinct_id` (a drill-down, an export) shows the pre-merge value unless it goes
through the same helper.

### 4. SDK changes

| SDK | mints `anonymous_id` | has `reset()` | changes |
|---|---|---|---|
| **js** | yes | yes | 2 |
| **flutter** | yes | yes | 2 |
| node / python / csharp | hardcode `null` | no | **none** |

The three server SDKs never create an alias, so there is no long-lived-process
hazard (one process aliasing all its traffic to whoever logged in first) and no
`reset()` gap. Both js and flutter already carry the `anonUsed` /
`_anonymousIdUsed` guard, so a first-ever-launch `identify()` does not mint a
speculative alias. `anonymous_id` is `Option<String>` on the wire
([envelope.rs:301]), so `null` is accepted — no wire change, no version-compat
problem.

**Change 1 — auto-reset on identity switch.** Persist the last identified id. On
`identify(id)` where a *different* id was previously identified, mint a fresh
anon id **before** building the item and send `anonymous_id: null`.

This cannot repair events already sent under a burned alias. It bounds the
damage of a forgotten `reset()` to one guest window instead of every future one.

**Change 2 — `reset()` rotates the session id.** Today it clears the scope user
and mints a fresh anon id but leaves `session_id` alone, so one `sessions` row
can serially hold two people and record only the last (see "Problem"). Rotating
on `reset()` means a session never spans two people.

### 5. Making the silent failure observable

The burn rule (D3) drops a conflicting `identify()` with no signal, so nobody
would ever learn their app forgot `reset()`.

`insert_identity` switches from `ON CONFLICT DO NOTHING` to the house pattern
already used by `bump_session`:

```sql
ON CONFLICT (app_id, alias_id) DO UPDATE SET distinct_id = identities.distinct_id
RETURNING distinct_id, (xmax = 0) AS inserted
```

This returns both whether it inserted *and* the existing target, distinguishing
two cases that are indistinguishable today (both return zero rows):

- **same target** → benign repeat `identify()` by the same user. Ignore.
- **different target** → a real conflict. Count per app, log at `warn`, expose.

Without this, the design's chosen safety behaviour is indistinguishable from the
feature not working.

## Failure modes

| Mode | Behaviour | Mitigation |
|---|---|---|
| Crash mid-merge | Person half-merged: some tables rewritten, some not | Steps 1–6 idempotent, 7–8 consuming; re-run the whole job. The row is left `'running'`, and **the lease arm is what re-claims it** — without that arm this row is orphaned forever, since a burned alias is never re-enqueued. |
| Merge job lag | Reports briefly show the person twice | Bounded by queue depth; surfaced as a pending-merge gauge |
| Repeated failure | Row hits `attempts = MAX_ATTEMPTS` | `state = 'dead'` (a distinct terminal state, so it leaves the runnable index), retained for inspection, logged. No infinite retry. |
| Job stolen by the lease from a slow-but-live worker | Both workers run the same merge | Data is safe — the second `fold_rollups`' `DELETE … RETURNING` finds the rows already gone, so `moved` is empty and nothing double-counts. The `claimed_at` fence stops the loser overwriting the winner's terminal state. |
| Tier race (export before rewrite) | Parquet keeps the anon id | Benign: the cold overlay resolves it, and `resolve()` is idempotent so post-merge files are unaffected |
| Heavy guest (50k+ rows) | One large `UPDATE` burst; new row versions across partitions | Per-table transactions; log affected row counts. Chunking deferred until a real case appears. |
| Shared device, `reset()` not called | The next guest's pre-login events resolve to the previous user | Burn rule caps it; SDK change 1 bounds it; §5 makes it visible |
| Cold drill-down | Raw cold rows show the pre-merge `distinct_id` | Stated limit; route reads through `resolved_cold_events` |

## Testing

> **Backend DB tests silently skip under the sandbox** — the Bash sandbox has its
> own netns, so DB-backed tests return early while printing `ok`. Every test
> below must be run with `dangerouslyDisableSandbox` against host-network
> containers, and the pass count compared against the known baseline. A green run
> is not evidence on its own.

**Correctness**

1. Merge rewrites `analytics_events` / `error_events` and sets `guest_alias`.
2. **The headline assertion:** `count(DISTINCT distinct_id)` over a guest-then-identified
   timeline returns **1**, not 2 — pre- and post-merge.
3. Re-running a completed merge changes no counter (idempotence).
4. Fold into an existing target row in the same environment sums each counter
   exactly once.
5. Fold of a row with `environment_id IS NULL` hits the nil-uuid sentinel and
   does not create a second unattributed row.
6. Fold does not trip "cannot affect row a second time" when the alias has rows
   in several environments.
7. Burn: a second `identify()` with a different user does not re-point the alias,
   and is counted as a conflict.
8. Chain rejection: `u-42 → u-99` is refused when `u-42` is already a target.
9. `guest_alias IS NOT NULL` selects exactly the pre-login events.
10. **Both identify paths enqueue** — one test per path (`process_identify` and
    the batched loop). Neither may be asserted by proxy.

**Cold tier**

11. Parquet written before a merge resolves to the person via the overlay.
12. An alias with `cold_stale = false` is excluded from the overlay.
13. Window pruning: an alias whose span does not overlap the query window is not
    loaded.
14. An alias whose merge is still `pending` **is** loaded regardless of span —
    the in-flight window where the row is stale in both tiers at once.
15. Tier race: export the partition, *then* merge, then query — the cold count is
    still correct.

**Performance (regression guards)**

16. `EXPLAIN` confirms the index-only scans from migrations 0028 / 0031 / 0039
    are still chosen after the `guest_alias` column is added.
17. The persons list and active-users endpoints stay well inside the 30 s
    `TimeoutLayer` on a seeded dataset.

**SDK**

18. js + flutter: `identify()` with a different user mints a fresh anon id and
    sends `anonymous_id: null`.
19. js + flutter: `reset()` rotates the session id.
20. node / python / csharp: unchanged — `anonymous_id` still `null`.

## Out of scope

- App+env data purge — separate spec (`2026-08-12-admin-data-purge-design.md`).
- Surrogate `person_id` / full identity graph (approach C).
- Unmerge / manual re-point in the dashboard. `guest_alias` retains the original
  value so it stays possible later.
- Historical backfill of pre-existing data (D1).
- Cross-app identity resolution — the alias map stays per-app, matching
  `identities`.

[process.rs:586]: ../../../backend/crates/sauron-pipeline/src/process.rs
[batch.rs:693]: ../../../backend/crates/sauron-pipeline/src/batch.rs
[repo.rs:5225]: ../../../backend/crates/sauron-db/src/repo.rs
[repo.rs:5528]: ../../../backend/crates/sauron-db/src/repo.rs
[duck.rs:293]: ../../../backend/crates/sauron-tier/src/duck.rs
[envelope.rs:301]: ../../../backend/crates/sauron-core/src/envelope.rs
[0028]: ../../../backend/migrations/2026-07-28-000028_issue_env_covering_index/up.sql
[0031]: ../../../backend/migrations/2026-07-29-000031_issue_env_latest_index/up.sql
[0039]: ../../../backend/migrations/2026-08-01-000039_analytics_active_user_index/up.sql
