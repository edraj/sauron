# Seed: 10M events on the dev instance

Populates the `Weby` app (`ee1fb653-…`) with **8M analytics + 2M error events**
across **90 days** (`2026-05-27 → 2026-08-24`), plus 8M transactions, ~500k
sessions, 50,000 users and 50,000 devices drawn from a pool of 100 models.

Design and rationale: [`docs/superpowers/specs/2026-08-24-populate-10m-events-design.md`](../../docs/superpowers/specs/2026-08-24-populate-10m-events-design.md).

## Run

Scripts are numbered and run in order against the compose Postgres:

```bash
for f in 00-partitions 10-dimensions 20-events 30-rollups 40-analyze 50-verify; do
  docker cp "scripts/seed/$f.sql" sauron-postgres-1:/tmp/ && \
  docker exec sauron-postgres-1 psql -U sauron -d sauron -q -f "/tmp/$f.sql"
done
```

`20-events.sql` is the long one (~15 min) and commits per day, so an interruption
costs one day rather than the run. `90-cleanup.sql` is **not** part of the
sequence — run it only to remove the seed.

## Preconditions

- **Schema at migration 70 or later.** The prebuilt Docker images embed
  migrations only to 53 and the compose `migrate` service reports "migrations up
  to date" while applying nothing — check `max(version)` in
  `__diesel_schema_migrations` rather than trusting that message.
- **`sauron-tier` must be DOWN.** At `TIER_HOT_DAYS=30` it exports-then-drops any
  partition ending before the cutoff, which is most of a 90-day window. Bringing
  it up after seeding will rotate ~60 days of this data to Parquet.
- **~40 GB free** wherever `Docker Root Dir` lives (`docker info`). Measured:
  ~38 GB for the full set. Note `/` and `/home` are separate filesystems here.

## Two hazards this hit, recorded so the next run does not

**The DEFAULT partition can block partition creation.** `error_events_default`
held 30 rows of real data spanning dates the window needed partitions for, and
`CREATE TABLE … PARTITION OF` refuses while a default partition holds rows that
would belong to the new range. Those rows were moved to `seed_default_rescue`,
the partitions created, then re-inserted so they routed correctly. That table is
deliberately left behind as the record of the move — it is **not** seed data.

**`LIKE 'seed_%'` is not safe on this instance.** It already contained 81
`event_users` rows from an earlier session's seeding (`seed_mrsuix2u_191`, …) on
the same app. Every pattern in `90-cleanup.sql` is anchored to the exact id shape
this seed emits, so a cleanup cannot take those with it.

## Light-payload mode (Phase B benching)

`10-dimensions.sql` accepts `-v light=1`, which strips the sampled payload
pools down to ~0.2 KB/row. Use it to seed at production DENSITY rather than
production payload weight — the rollup/architecture bench cares about row
counts, not JSON bulk:

```bash
docker exec sauron-postgres-1 psql -U sauron -d sauron -q -v light=1 -f /tmp/10-dimensions.sql
```

For the 10M/day Phase B recipe, also raise the per-day totals in
`20-events.sql`'s `seed_plan` to 10M/day over 14 days (~140M rows, ~40-60 GB)
before running. Rollups must be backfilled afterwards
(`sauron-migrate backfill-rollups`).

## Known simplification

`error_events.stacktrace_sha256` is left NULL, so seeded rows keep their
stacktrace inline rather than pooled into `error_stack_blobs` (migration 68).
That is a valid state — the column is nullable and the worker produces un-pooled
rows too — but it means seeded error events sit at the larger end of the size
range.
