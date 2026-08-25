# Populate the dev instance with 10M events — design

**Date:** 2026-08-24
**Status:** approved, not yet implemented

## Goal

Load the running compose instance (`sauron_pgdata`) with 10,000,000 events —
8,000,000 analytics events and 2,000,000 error events — spread across 90 days,
attributed to 50,000 distinct users on devices drawn from a pool of 100 models,
with sessions and transactions to match.

The dataset exists so query plans, index behaviour and dashboard latency can be
observed at a realistic scale. That purpose sets every fidelity decision below:
a dataset that is fast to build but shaped unlike production would answer the
wrong question.

## Locked decisions

| Decision | Value |
|---|---|
| Target database | compose stack, `sauron_pgdata` (API `:10000`, dashboard `:10002`) |
| Target app | `Weby`, `app_id = ee1fb653-cadd-4f27-9321-ff10f382a18c` |
| Load path | direct bulk SQL, server-side |
| Time window | `2026-05-27 → 2026-08-24` (90 days) |
| Users | 50,000 `event_users` |
| Devices | ~50,000 `devices` rows drawing from a pool of **100 models** |
| Sessions | yes |
| Transactions | yes, 1 per analytics event (8,000,000) |
| Migrations | run `sauron-migrate` first (63 → 70) |
| Payload | realistic — cloned from real worker output |

## Approach: clone-and-fan-out

`analytics_events_2026_07_27` holds 210,839 rows that are genuine ingest-worker
output. The loader uses them as a template pool and fans them out with fresh
ids, timestamps and identity keys.

This matters more than it might look. The alternative — hand-writing what a row
should contain — reproduces the loader author's *belief* about the schema, and
that belief drifts from what `sauron_db::batch::write_rows` actually writes.
Cloning makes shape fidelity structural rather than asserted: the jsonb columns,
their widths, their TOAST behaviour and their key distributions are whatever the
worker really produced.

Everything runs server-side. No client round-trips, no Rust build (`backend/target`
is already 405 GB and builds are slow).

## Prerequisite: migrations 64 → 70

The instance is at migration `20260816000063`; the repo has
`2026-08-24-000070_event_user_env_rollup_epoch`. Three of the seven pending
migrations change what a loaded row costs on disk:

- **65** `error_events_lz4_and_index_audit` — switches `error_events` to lz4
  TOAST compression. `SET COMPRESSION` only affects *new* writes, so 2M rows
  loaded beforehand would keep pglz permanently.
- **66** `analytics_events_index_audit` — reworks the analytics indexes. Loading
  first means building indexes that then change.
- **68** `error_stack_pool` — pools stacktraces into `error_stack_blobs`.

Run `sauron-migrate` to completion and confirm `__diesel_schema_migrations`
reaches `20260824000070` before any load step.

## Components

All under `scripts/seed/`, run in order.

### `00-partitions.sql`

`analytics_events`, `error_events` and `transactions` are **daily RANGE
partitions on `occurred_at`**. Each has exactly **32** real partitions today,
covering `2026-07-15 → 2026-08-17` with `2026-08-05` and `2026-08-06` missing.
The 90-day window therefore needs **58 new partitions per table, 174 in total** —
including every day from `2026-08-18` forward, which `sauron-tier` never created
because the stack was down.

Each new partition carries the same storage settings
`repo::create_range_partition` applies:

```sql
WITH (autovacuum_vacuum_scale_factor = 0.0, autovacuum_vacuum_threshold = 20)
```

These are not cosmetic — migration 60 exists to put them on every leaf of all
three tables, and a partition created without them is a silent divergence from
every worker-created one.

### `10-dimensions.sql`

- **50,000 `event_users`** — `distinct_id` of the form `seed_u_000001`, with
  `first_seen` / `last_seen` spread across the window rather than clustered.
- **~50,000 `devices`** — `device_key` `seed_d_000001`, with `model` / `os`
  drawn from a fixed pool of **exactly 100 models**.
- **~500,000 `sessions`** — derived at roughly 20 events per session, so session
  length is a consequence of the event stream rather than an independent knob.

### `20-events.sql`

- **8,000,000 `analytics_events`**
- **2,000,000 `error_events`** over a pool of **300 distinct issues**, with ~25%
  `handled = false` so crash-free rate is a meaningful number rather than 0 or 100.
- **8,000,000 `transactions`**

Shaping rules:

- `occurred_at` uniform across the 90 days, modulated by a diurnal curve and a
  weekday/weekend factor. Every timestamp must land inside a real partition.
- `distinct_id` drawn **zipfian** over the 50,000-user pool, so a minority of
  users carry a majority of events. A uniform draw would make every
  per-user query equally cheap and hide exactly the tail latency this dataset
  exists to expose.
- `environment_id` drawn from Weby's three enrollments — `production`, `benchmark`,
  `demo` — production-dominant.

`environment_id` stores the **`app_environments.id` (enrollment id)**, not the
`environments.id` catalogue id. Confirmed against the rows already present:
`de670846-…` and `f5ca97e8-…` are both enrollment ids. Using the catalogue id
would produce rows the API filters to nothing.

Determinism via `setseed()` so a rerun reproduces the same dataset.

### `30-rollups.sql`

The derived state the worker would have written:

- **`issues`** — `times_seen`, `users_seen`, `first_seen`, `last_seen`
  aggregated from the loaded error events. `error_events` has no dedup: one full
  row per occurrence, and `issues.times_seen` is the only counter.
- **`event_user_environments`** and **`device_environments`** rollups, with
  session counts credited in.
- **The epoch markers** — `device_env_rollup_epoch` and
  `event_user_env_rollup_epoch`. Without them `/persons` and `/device-groups`
  fall off their fast path and time out at this scale. Both markers are
  operator-written; no deployment path writes them.

### `40-analyze.sql`

`VACUUM (ANALYZE, PARALLEL 0)` per partition. The `PARALLEL 0` is required, not
defensive: parallel vacuum workers exhaust the container's `/dev/shm` and the
statement fails with *could not resize shared memory segment*.

### `90-cleanup.sql`

Every seeded identity key is prefixed `seed_`, so cleanup is exact:
`DELETE … WHERE distinct_id LIKE 'seed\_%'` for the eight days that overlap the
existing Weby data (`2026-07-20 → 2026-07-27`), and `DROP` for partitions that
are wholly seeded. The prefix is what makes removal surgical instead of a
pattern-matched guess at which rows were ours.

## Load strategy

For each day: create a standalone table with `LIKE … INCLUDING ALL`, fill it,
build its indexes, then `ATTACH PARTITION` with a matching CHECK constraint so
the attach skips validation. Attaching a pre-indexed table avoids rebuilding the
parent's 14 partitioned indexes per insert.

Estimated 40–60 minutes.

WAL churn will be roughly the size of the data. `max_wal_size` can be raised
temporarily on the postgres service if checkpoint frequency becomes the
bottleneck.

## Cost

Derived from measured per-row sizes on this instance
(`analytics_events_2026_07_27`: 412 MB heap + 245 MB indexes over 210,839 rows).

| Table | Rows | Bytes/row | Total |
|---|---|---|---|
| `analytics_events` | 8,000,000 | 3,116 | ~25 GB |
| `error_events` | 2,000,000 | 3,576 | ~7 GB |
| `transactions` | 8,000,000 | 743 | ~6 GB |
| `sessions` | ~500,000 | ~500 | ~0.3 GB |
| **Total** | | | **~38 GB** |

Docker's root is `/home/splimter/docker-data` on `/dev/nvme0n1p3`, which has
451 GB free. Migrations 65 and 68 should reduce the `error_events` figure.

## Verification

Evidence, not assertions:

1. Exact row counts per table, and the 80/20 analytics-to-error split.
2. `count(distinct distinct_id) = 50000`; `count(distinct model) = 100`.
3. **`analytics_events_default` still at 0 rows** — the single check that proves
   every generated timestamp landed in a real partition rather than silently
   falling through to the default.
4. `EXPLAIN` on `/overview/totals` and `/persons` showing the rollup fast path
   is taken, not a per-row sweep across 90 partitions.
5. The dashboard at `:10002` renders the app's pages against the loaded data.

## Out of scope

- Changes to `crebain` or the ingest path. This is a data-loading exercise.
- Any app other than `Weby`.
- Cold-tier / Parquet rotation of the loaded partitions.
