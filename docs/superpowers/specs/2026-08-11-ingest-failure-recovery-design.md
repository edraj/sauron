# Ingest Failure Recovery — Design

**Date:** 2026-08-11
**Status:** BUILT, tested and **browser-verified** (uncommitted). 1,601 backend +
720 dashboard tests green; `npm run build` exits 0. Includes 10 real-Postgres
tests, 9 real-Redis tests, and one HTTP test through the compiled binary.

Browser run against an isolated stack confirmed: the page renders, the
capped-group row shows `242,700 / 6 / −242,694 lost`, the drill-down opens
payloads, Retry re-queued 4 events (verified as 4 real entries on the Redis
stream, group → `requeued`, all payloads stamped, audit row written), Drop's
Cancel deleted nothing, Drop's confirm hard-deleted and left an audit row
carrying no payload, the status filter works, and a user without `org:manage`
gets the nav hidden plus a denial naming the permission.
**Supersedes nothing.** Narrows the role of `sauron:ingest:dlq`.

---

## 1. The problem

A job that fails in the worker is dead-lettered on its **first** attempt and lands
in `sauron:ingest:dlq`, a Redis stream that **nothing reads**. There is no replay
path, no admin UI, no CLI. Its only consumer is the Prometheus gauge
`sauron_ingest_dlq_length`.

So today a failure is *countable* but not *recoverable*. Three consequences:

1. **No retry exists.** `RECLAIM_IDLE_MS = 60_000` (`worker.rs:65`) looks like a
   retry but is not: `claim_stale` reclaims entries that were never acked — crash
   recovery for a worker that died mid-batch. A job that *fails* is dead-lettered
   immediately, so a two-second Postgres hiccup permanently loses every event in
   flight.
2. **Failures accumulate in RAM.** Dead-lettered payloads live in Redis for
   `INGEST_DLQ_RETENTION_HOURS` (default 168h / 7 days). Measured on the dev
   instance 2026-08-11: `sauron:ingest:dlq` held 242,700 entries — 434 MB, i.e.
   >99% of all Redis memory in use — from a 35-second benchmark run 15 days
   earlier.
3. **The bounds only hold while code runs.** Both guards on the DLQ (`MAXLEN` at
   write time, `XTRIM MINID` on the worker-0 tick) require a running worker.
   Orphaned data is bounded by nothing. This design does **not** close that hole;
   see §10.

---

## 2. What we are building

A three-tier recovery path:

| Tier | Where | Holds | For how long |
|---|---|---|---|
| Retry | Redis ZSET `sauron:ingest:retry` | jobs mid-backoff | ≤ ~3 min |
| Terminal | Postgres `ingest_failures` + `ingest_failure_payloads` | grouped failures + capped payloads | `INGEST_FAILURE_RETENTION_DAYS` (30) |
| Backstop | Redis `sauron:ingest:dlq` (existing) | only failures we could not even record | 7 days |

Plus an admin page at `/admin/ingest-failures` where a deployment admin inspects
grouped failures and either **retries** or **drops** them.

---

## 3. Locked decisions

Recorded so implementation does not relitigate them.

| # | Decision | Rationale |
|---|---|---|
| D1 | Retry only **transient** failures; permanent ones go straight to Postgres with `attempts = 0` | Malformed JSON is the dominant failure and is deterministic — retrying it 3× costs 3 minutes to reach a guaranteed-identical result |
| D2 | Unknown error kind classifies as **Permanent** | Permanent ≠ discarded. It means "surface to a human now" rather than hide in a retry loop. Fail-visible, not fail-silent |
| D3 | Backoff parks in a **Redis ZSET** scored by due-time | Survives restarts, occupies no worker, and reuses the worker-0 tick. In-process sleep loses in-flight backoffs on deploy and races `RECLAIM_IDLE_MS` |
| D4 | Visibility: **deployment admin only** (`require_deployment_admin`) | The dominant failure never decoded, so it has no `org_id` to scope by. `org_id` is stored on the row regardless, so an org-scoped view is a later slice with no migration |
| D5 | Manual retry **re-injects onto the main ingest stream** with a correlation id | Retry exercises the real pipeline, so it tests the same path production runs. The correlation id is what closes the loop — without it the admin never learns whether the retry worked |
| D6 | Drop is a **hard DELETE**; non-dropped rows age out on a retention reaper | Strongest privacy position for masked copies of real user events. The audit entry is the only survivor, so it must carry enough to reconstruct the decision |
| D7 | **Group by fingerprint**, one row per failure kind | 242,700 identical failures become ~3 rows. Follows the existing Issues pattern |
| D8 | Keep individual payloads in a **child table, capped per group** | Grouping alone would make retry a one-event sample replay, not recovery. The cap keeps one runaway failure from eating the disk |
| D9 | Scheduler and classifier live **in `sauron-pipeline`** on the worker-0 tick | Adds zero deploy surface. The tick that must run for this to work is the same one that must already run for ingest to work |
| D10 | The Redis DLQ **survives**, narrowed to a backstop | When Postgres is down we cannot write a failure row — which is exactly when transient failures spike. Without the backstop, a Postgres outage becomes silent event loss |

---

## 4. Failure classification

`sauron-pipeline/src/classify.rs`

```rust
pub enum FailureKind { Transient, Permanent }
```

**Transient:** pool timeout / `Error::BrokenTransactionManager`, connection reset,
SQLSTATE `40001` (serialization failure) and `40P01` (deadlock detected),
`53300` (too many connections), Redis connection errors, symbolication network
timeouts.

**Permanent:** serde decode failure, `23503` (FK violation — unknown `app_id`),
`23514` (check constraint), `22001` (value too long), oversized payload.

**Default:** `Permanent` (D2).

`error_kind` is a short stable slug (`decode`, `db_fk_violation`, `db_deadlock`,
`symbolication`, `unknown`) used for grouping and for the metrics label. It is
NOT the raw error message.

---

## 5. Retry scheduling

`sauron-pipeline/src/retry.rs`

```
process_job fails
  ├─ Permanent ─────────────────────► record_failure() → ACK
  ├─ Transient, attempts < 3 ───────► ZADD retry (due = now + 60s) → ACK
  └─ Transient, attempts = 3 ───────► record_failure() → ACK

worker-0 tick (30s, existing loop):
  drain_due_retries()      ZRANGEBYSCORE 0..now → XADD main → ZREM
  reap_dlq_once()          (existing)
  reap_ingest_failures()   (new)
```

ZSET member is a JSON envelope `{payload, attempt, first_failed_at, error_kind}`.
Score is the due timestamp in unix millis, so `ZRANGEBYSCORE key 0 <now>` is the
whole due-check.

Three correctness rules, each of which is a silent-loss bug if broken:

1. **Parking a job MUST ack the Redis stream entry.** Otherwise `RECLAIM_IDLE_MS
   = 60_000` reclaims it at almost exactly the moment the ZSET re-injects it, and
   every transient failure double-writes.
2. **`ZREM` strictly after a successful `XADD`.** A crash between them yields a
   duplicate, not a loss. Duplicates are the correct side to fail toward.
3. **Draining is capped per tick** (`RETRY_DRAIN_LIMIT`, 500) so a mass failure
   cannot convert one tick into an unbounded re-injection storm.

**Backoff granularity is honest:** the tick is 30s, so a 60s backoff is really
60–90s. The constant is named `RETRY_BACKOFF_SECS = 60` and the tick granularity
is documented rather than papered over.

**The attempt counter is a separate Redis key** (`sauron:ingest:att:<hash>`,
`INCR` + `EXPIRE`, TTL 900s). Discovered during implementation: a drained retry
is re-injected as an ordinary stream payload, so the attempt count cannot ride
along with it. Without a counter beside the job, every re-injected retry would
fail at "attempt 1" forever and **the retry loop would never terminate** — a
permanently-broken payload cycling through the backoff set for the life of the
deployment. Teaching the stream format to carry a counter was rejected: it would
change the decode path every healthy event takes for the benefit of the rare
failing one. The TTL must exceed `MAX_ATTEMPTS × RETRY_BACKOFF_SECS`, or an
expiry mid-sequence silently grants a fresh set of attempts; a unit test pins
that relationship.

Redis-side failures fail **toward terminal**, not toward retry: if the counter
is unreadable the job is treated as having exhausted its attempts, so an unwell
Redis cannot re-park the same job indefinitely.

---

## 6. Schema

Migration `2026-08-11-000051_ingest_failures`.

### `ingest_failures` (parent — one row per failure kind)

| Column | Type | Notes |
|---|---|---|
| `id` | UUID PK | |
| `fingerprint` | TEXT NOT NULL UNIQUE | see §7 |
| `error_kind` | TEXT NOT NULL | stable slug from §4 |
| `error_message` | TEXT NOT NULL | most recent raw message, for the human |
| `org_id` / `project_id` / `app_id` | UUID NULL | **no FK** — same reasoning as `audit_log`: an inert snapshot, and the row must survive its app being deleted. NULL when the payload never decoded |
| `occurrences` | BIGINT NOT NULL DEFAULT 1 | total seen, including payloads past the cap — **the only counter** |
| `status` | TEXT NOT NULL DEFAULT 'failed' | `failed` \| `requeued` \| `resolved` |
| `first_seen_at` / `last_seen_at` | TIMESTAMPTZ NOT NULL | |

**Changed during implementation.** The design originally carried denormalized
`retained_payloads` / `dropped_payloads` columns. It cannot: bumping them
requires updating this row a *second* time in the same statement as the
fingerprint upsert, and Postgres will not apply a second update to a row already
modified by another sub-statement of the same statement. The counters would have
silently drifted from reality while every test passed. They are now **derived on
read** — `retained` is `COUNT(children)`, `dropped` is `occurrences - retained` —
which cannot drift at all. This is why `conn.transaction` being unavailable
(diesel-async 0.9 / MSRV 1.82) changed the schema rather than merely the code.

Indexes: `UNIQUE (fingerprint)`; `(status, last_seen_at DESC, id DESC)` for the
keyset-paged default view — `id` is the tiebreaker, not decoration, because rows
written in one transaction share a `last_seen_at` to microsecond precision and an
untiebroken cursor skips or repeats at the page boundary.

### `ingest_failure_payloads` (child — the recoverable events)

| Column | Type | Notes |
|---|---|---|
| `id` | UUID PK | |
| `failure_id` | UUID NOT NULL REFERENCES `ingest_failures(id)` ON DELETE CASCADE | |
| `payload` | JSONB NOT NULL | **already PII-masked** — `mask::apply_wire` runs before anything is persisted or re-queued |
| `attempts` | INTEGER NOT NULL DEFAULT 0 | retries burned before it landed here |
| `created_at` | TIMESTAMPTZ NOT NULL | |
| `requeued_at` | TIMESTAMPTZ NULL | set when re-injected, cleared on failure |

Index: `(failure_id, created_at)`.

**Cap:** `INGEST_FAILURE_PAYLOAD_CAP`, default 1000 per fingerprint. Past the cap
the child insert is skipped while `occurrences` still increments, so the derived
`dropped` count accounts for every refused occurrence.

Two workers recording the same fingerprint concurrently can each see
`count < cap` and both insert, overshooting the cap by the number of racing
writers. Deliberate: the cap guards disk, it is not an invariant anything reads.

---

## 7. Fingerprinting

```
sha256(error_kind ‖ normalize(error_message) ‖ app_id.unwrap_or(NIL))
```

`normalize` strips volatile substrings before hashing — UUIDs, standalone
integers, quoted literals, and byte offsets — so `row 4821` and `row 9` collapse
into one group instead of producing 242,700 of them. Without normalization the
grouping decision (D7) buys nothing.

---

## 8. API

`sauron-api/src/routes/failures.rs`, all four gated on `require_deployment_admin`
(the `org:manage`-in-every-org pattern already used by Storage and Tier Policy).

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/admin/ingest-failures` | keyset-paged; `?status=&error_kind=&limit=&cursor=` |
| GET | `/v1/admin/ingest-failures/:id/payloads` | paged sample payloads |
| POST | `/v1/admin/ingest-failures/:id/retry` | re-injects **every retained child**; sets group `status=requeued` |
| DELETE | `/v1/admin/ingest-failures/:id` | hard DELETE, cascades children; audited first |

**Retry loop closure (D5):** the re-injected envelope carries
`failure_payload_id`. On success the worker deletes the child row and decrements
`retained_payloads`; when a `requeued` group reaches zero children it becomes
`resolved`. On failure the new error is written back and the group returns to
`failed`. Without this the Retry button is fire-and-forget and the admin never
learns the outcome.

**Audit (D6):** drop is recorded *before* the delete — `ingest_failure.drop`,
carrying `fingerprint`, `error_kind`, `app_id`, `occurrences`, and
`retained_payloads`, and **never the payload**, which would defeat the masking
allowlist rule. Where `org_id` is known the entry files under that org; where it
is NULL the action is deployment-wide and uses `record_all_orgs`, matching how
`tier_policy.update` is handled. Retry is audited as `ingest_failure.retry`.

---

## 9. Dashboard

New page `dashboard/src/pages/IngestFailures.svelte` at `/admin/ingest-failures`,
registered in `routes.ts`, `admin-nav.ts` (icon `refresh-cw`), and `page-access.ts`.
Typed client `lib/api/ingest-failures.ts`, model + tests in
`lib/models/ingest-failures.ts`.

Grouped table: error kind, message, app, occurrences, retained/dropped, first and
last seen, status. Row expands to sample payloads. Per-group **Retry** and
**Drop** actions, both permission-gated and both confirmed — drop says plainly
that it is permanent.

The dropped-payload count is shown on every group where it is non-zero
("1,000 of 242,700 retained — 241,700 are not recoverable"). Silent truncation
that reads as full coverage is the specific bug class this page exists to expose.

Uses house UI components (`DataTable`, `Button`, `Icon` registry) — no raw
`<button>`/`<table>`.

---

## 10. What this does NOT fix

- **Bounds still require running code.** Every guard here — retry drain, retention
  reaper, payload cap — runs on the worker tick. With all workers stopped, the
  data is bounded by nothing, exactly as the current DLQ is. The only guard that
  holds with everything stopped is a Redis-side `maxmemory`, which is a separate
  operational change and is recommended alongside this.
- **The existing 434 MB.** Orphaned pre-fix data needs a manual
  `XTRIM sauron:ingest:dlq MAXLEN 0`. This feature does not clean it.
- **Payloads past the cap** are unrecoverable by construction. Surfaced, not hidden.

## 11. Incidental correction

`dashboard/src/lib/models/inspector.ts:55` still tells users the DLQ "is XADD with
no MAXLEN and no TTL, and no reaper exists". All three clauses became false when
the bounded DLQ and reaper landed. The PII Inspector is showing a closed hazard as
open; corrected as part of this work.

---

## 12. Testing

| Area | Test |
|---|---|
| Classification | each error variant maps to the right kind; unknown → Permanent |
| Retry scheduler | due vs not-due; a crash between `XADD` and `ZREM` yields a duplicate, never a loss; drain respects `RETRY_DRAIN_LIMIT` |
| Ack discipline | a parked job is acked, so `claim_stale` cannot also redeliver it |
| Fingerprint | two messages differing only by UUID/row number collapse to one group |
| Cap | the 1001st occurrence increments `dropped_payloads` and inserts no child; `occurrences` and `dropped_payloads` never disagree |
| Retry loop closure | success deletes the child and resolves the group; failure returns it to `failed` with the new error |
| RBAC | a non-deployment-admin gets 403 from all four endpoints |
| Audit | drop writes an entry carrying fingerprint/kind/counts and **no payload**; NULL `org_id` routes to `record_all_orgs` |
| Retention | rows past the cutoff are deleted; children cascade |
| Backstop | a `record_failure` failure falls through to the Redis DLQ rather than dropping the event |

Backend tests must run with `dangerouslyDisableSandbox` against host-network
containers — the sandbox has its own netns, so DB-backed tests otherwise return
early while printing `ok`.
