# Admin data purge

Date: 2026-08-12
Status: approved

## Problem

Bad ingest produces garbage that pollutes every metric derived from it — a
misconfigured SDK flooding an environment, a bad release emitting a junk event
name, a load test whose traffic was never meant to be real. There is today no
way to remove it. The only deletion paths that exist are `repo::delete_app` and
`repo::delete_project`, which destroy the whole container and everything under
it, and the `sauron-tier` rotation, which moves data to cold Parquet rather than
removing it. An operator who wants their charts to be right again has no
recourse short of dropping the app.

Deleting the raw rows is not by itself sufficient, and this is the crux of the
design. `sessions`, `devices`, `event_users`, `event_user_environments` and
`issues` all carry **monotonic counters** — `events_count`, `errors_count`,
`times_seen`, `users_seen`, `sessions_count` — that are only ever incremented,
by `repo::bump_session` / `bump_device` / `upsert_issue` in the pipeline. No
recompute path exists anywhere in the codebase. The Sessions, Devices and Users
screens read those counters directly. So a purge that deletes only raw events
leaves every one of those screens reporting the garbage it just removed, which
is precisely the state the operator ran the purge to escape.

## Scope

A new deployment-admin page and API for purging product signal data within one
app, bounded by environment and time range, followed by a recompute of every
rollup the deletion touched.

**In scope:** `error_events`, `analytics_events`, `transactions`, `workflows`,
the inspector scan artefacts, and the `issues` / `sessions` / `devices` /
persons rollups.

**Out of scope:** `store_daily_metrics` (a separate CSV import path, unrelated
to the event pipeline and its rollups); `auth_sessions` and `users` (dashboard
logins and members, not product signals — named here only because "sessions"
and "users" are ambiguous in this schema and mean the product-signal tables
throughout this document); cold Parquet rewriting (see *Cold tier* below);
recurring or policy-driven retention.

## Authorization

The existing `require_deployment_admin` in
`bins/sauron-api/src/routes/admin.rs` — org-scoped `org:manage` in **every** org
that exists. No new permission and no migration for one.

This is deliberately stricter than the app-scoped alternative. In the common
single-tenant self-hosted deployment it means "the admin". In a multi-tenant
deployment it means a tenant's own admin **cannot** purge their own bad data;
only a global operator can. That is a known and accepted consequence, chosen
because the operation is irreversible.

Note the guard's own semantics: a deployment with zero orgs is refused rather
than trivially satisfied, so a fresh install does not let any authenticated user
through.

## API

All under `/v1/admin/purge`, all requiring deployment admin.

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/preview` | Create a job in `previewed`, return per-kind per-environment counts |
| `POST` | `/{id}/confirm` | `previewed` → `pending`; body `{confirm_text}` must equal the app slug |
| `POST` | `/{id}/cancel` | Stop further batches |
| `GET` | `/{id}` | One job with its counts and progress |
| `GET` | `/` | Job history |

### Preview → confirm contract

Preview takes the full scope — app, environments, time range, kinds — counts
what it matches, and **freezes that scope into the job row**. Confirm accepts
no scope fields at all, only `confirm_text`. It is therefore structurally
impossible for confirm to widen what preview counted and displayed. This is the
same contract `inspector.rs::confirm_mask` uses, and for the same stated reason:
the realistic failure is not a mis-click, it is acting on the wrong app because
the operator saw a problem and forgot which app was selected. A typed literal
like `PURGE` proves intent and proves nothing about scope; the slug proves
scope.

Previews expire after a TTL (reuse `inspector_preview_ttl_secs`' pattern with
its own `purge_preview_ttl_secs`), so a stale count can never be confirmed
against data that has moved on.

**No row cap.** `INSPECTOR_MASK_MAX_ROWS` has an analogue here that is
deliberately omitted: purging millions of junk events is the motivating case, so
a cap would block the operation the feature exists to perform. The async job
model plus mid-run cancellation replaces the cap as the safety mechanism.

### Counting

Preview counts directly against the event tables on
`(app_id, environment_id, occurred_at)`. `environment_id` is a plain nullable
column on both `analytics_events` and `error_events`, so this is an ordinary
indexed predicate.

It must **not** be built on the analytics endpoints' query builders. Those
introduce a time-unbounded correlated `EXISTS`/`LATERAL` when an environment is
supplied, which sequentially scans every partition and whose cost scales with
retained data rather than with the requested window — the measured cause of the
30s `TimeoutLayer` 503s on the analytics endpoints. A preview that reused that
shape would time out on exactly the large, badly-polluted app the feature is
for.

## Data model

Migration 57, `purge_jobs`, mirroring `inspector_mask_actions`:

```
id, org_id
app_id, app_slug, app_name         -- snapshotted, see below
environment_ids   jsonb            -- null = all environments, including unattributed
kinds             jsonb            -- array of kind slugs
range_start       timestamptz null
range_end         timestamptz null
all_time          bool             -- explicit, see below
status            text             -- previewed | pending | running | done | cancelled | failed
phase             text             -- delete | recompute
estimated_counts  jsonb            -- per kind, from preview
deleted_counts    jsonb            -- per kind, actual
cold_rows_skipped int8
cold_boundary_at  timestamptz null
kind_cursor       text null        -- which kind the delete pass is on
cursor_occurred_at timestamptz null
cursor_id         uuid null
requested_by, requested_by_email
cancelled_by, cancelled_by_email, cancelled_at
requested_at, previewed_at, confirmed_at, started_at, finished_at
worker_id, claimed_at
ingest_active     bool             -- observed at job start, see Risks
error             text
```

**No foreign key on `app_id`**, and `app_slug` / `app_name` are snapshotted at
request time. Purge history must remain readable after the app it purged is
deleted. A `REFERENCES apps(id) ON DELETE SET NULL` would blank the identifying
column on every historical entry, so filtering history by the app you destroyed
would return nothing — which is the exact question someone reads this table to
answer. Ids here are inert snapshots. This is the rule the `audit_log` table
established; `org_id` is the one exception, as the partitioning key.

**`all_time` is a real boolean, not "both dates are NULL".** An empty date field
must never be able to mean *everything*. Wiping an app's entire history for a
kind is a legitimate operation, but it has to be a distinct affirmative choice
in the UI rather than the accidental consequence of a field left blank.

### Scratch table

`purge_touched_keys (job_id uuid, kind text, key text)`, unlogged, rows deleted
with the job. The touched-key set can reach millions of entries, so it cannot
live in a jsonb column on the job row. Unlogged is correct: if the database
restarts mid-job the table is truncated, and the job must restart its recompute
phase from the delete cursor anyway.

## Kinds

Two categories, with different semantics.

**Raw kinds** — `error_events`, `analytics_events`, `transactions`, and
`inspector` (`inspector_scans` / `inspector_findings` /
`inspector_masked_keys`). Ticking one deletes its rows within
(app, environments, range).

**Rollup kinds** — `issues`, `sessions`, `devices`, `persons` (`event_users` +
`event_user_environments` + `identities`) and `workflows`. Ticking one deletes
rows whose **entire** activity span lies inside the range. A row straddling a
range boundary is kept and recomputed instead.

`workflows` belongs here despite looking like a signal table: it carries
`events_count`, `errors_count`, `started_at` and `last_event_at` and is upserted
by the pipeline's `workflow()` fold, so it has exactly the monotonic-counter
shape of `sessions` and `devices`. Treating it as a raw table — deleting its
rows and nothing else — would leave workflow counters as stale as the ones this
feature exists to repair.

### What the counters actually count

From the pipeline's three `acc.rollup(…)` call sites, the deltas are:

| Signal | `events_count` | `errors_count` |
| --- | --- | --- |
| `analytics_events` | +1 | 0 |
| `error_events` | 0 | +1 |
| `transactions` | 0 | 0 |

**Transactions contribute to no rollup counter at all.** `events_count` must be
recomputed from `analytics_events` alone — counting transactions into it, which
the name invites, would inflate every session, device and person on the first
purge. The recompute is not "count the surviving rows"; it is one count per
source table, mapped through this table.

**But "no surviving evidence" is a different question from "counters are
zero", and conflating them deletes live data.** The pipeline runs its rollup
fold for a transaction too — it creates the session, device and person rows and
bumps neither counter. So a session whose only signals are transactions
legitimately sits at `events_count = 0, errors_count = 0` in normal operation.
If the zero-evidence deletion rule read the counters, the first purge touching
that app would delete every transaction-only session, destroying data the
operator never selected and that no deletion had removed.

Recompute therefore carries a third quantity, `evidence` = surviving rows of
*every* kind including transactions. Counters come from the delta table above;
deletion is decided by `evidence == 0`.

For the same reason, purging `transactions` **does** schedule a repair pass
even though it moves no counter — the rollup rows it created would otherwise
survive as orphans describing occurrences that no longer exist. `inspector` is
the only raw kind that genuinely repairs nothing.

The straddle rule exists because a rollup row is not a point in time. A session
spans `started_at`→`last_event_at`; a device spans `first_seen`→`last_seen`; an
issue spans `first_seen`→`last_seen`. There is no coherent way to "partially
delete" such a row for a sub-range, so the only two honest outcomes are delete
it whole or repair it, and which applies is decided by whether the range
contains it.

**Recompute always runs** for every rollup touched by a raw deletion, whether or
not that rollup's kind was ticked. Ticking a rollup kind adds outright deletion
of fully-contained rows; it is never what causes repair. A rollup left with zero
surviving evidence after recompute is deleted rather than kept at zero.

`environment_ids = null` means all environments **including unattributed**.
`event_user_environments.environment_id` is nullable because
`EnvFilter::Unattributed` is a real row, not an absence, and a purge that
silently skipped those rows would leave the most confusing possible remainder.

### Not every kind is environment-scoped

Only some of these tables carry an `environment_id` at all:

| Environment-scoped | App-scoped only |
| --- | --- |
| `analytics_events`, `error_events`, `transactions`, `workflows`, `sessions`, `event_user_environments`, `inspector_findings` | `devices`, `issues`, `event_users`, `identities`, `inspector_scans` |

(`workflows.environment_id` is the one that is `NOT NULL`; everywhere else it is
nullable, which is what makes the unattributed case above real.)

**`environment_id` holds the ENROLLMENT id (`app_environments.id`), not the
catalogue id (`environments.id`).** The migration text for `analytics_events`
and `error_events` says `REFERENCES environments(id)` and is wrong: migration
000033 renamed the tables while keeping their OIDs, so the recorded DDL now
lies about its own target. Verified against `pg_constraint` on a freshly
migrated database — the real referent is `app_environments`.

This matters because both ids are UUIDs and neither the type system nor the
foreign key will catch the confusion. Validating a request against the
catalogue id accepts a value that then matches no event row, producing an
environment-scoped purge that reports success and deletes nothing. The
enrollment id is also what `?environment_id=` means everywhere else in the API,
so the UI needs no special case.

`devices`, `issues` and `event_users` have no `environment_id` column — a device
or a person exists across environments, and an issue's per-environment figures
are computed on read (`apply_issue_env_stats`) rather than stored.

So when `environment_ids` is non-null, those three kinds are **recompute-only**:
their rows cannot be deleted outright, because no predicate can decide whether a
given row "belongs to" the selected environment. Outright deletion of them
requires all-environments scope. The UI must disable those three checkboxes
when an environment filter is active and say why, rather than accepting the tick
and silently doing something narrower than it appears to promise.

This is not a limitation to work around by inferring membership from the
surviving raw rows. A device whose only remaining events are in another
environment is not thereby a different device, and deleting it would destroy
data outside the requested scope.

## Recompute

The delete pass records the distinct rollup keys it touched —
`session_id`, `device_key`, `distinct_id`, `issue_id` — into
`purge_touched_keys` as it goes. The recompute phase then re-derives each
touched rollup's counters and spans from **hot plus cold**.

Hot is Postgres. Cold is DuckDB over the Parquet copies, which is possible
because the export is `COPY (SELECT *, …)` (`crates/sauron-tier/src/duck.rs`),
so the cold files carry `session_id`, `device_key`, `distinct_id`,
`issue_id` and `environment_id` alongside everything else. New aggregates
grouped by each rollup key are needed; the existing `counts_by_app` groups only
by app, and `merge_day_counts` merges per-day rather than per-key, so neither is
directly reusable — but `merge_day_counts` establishes the hot+cold merge
pattern to follow.

Reading cold during recompute is not optional. A Postgres-only recompute would
silently *undercount* every rollup by whatever `sauron-tier` had already
exported — turning a purge intended to correct the numbers into a second,
subtler corruption of them, and one that looks like success.

This is also why decrementing the counters by the deleted quantity was rejected
as the cheaper alternative: `issues.users_seen` and
`event_user_environments.sessions_count` are DISTINCT counts, not sums.
Subtracting the number of deleted rows from a distinct count is simply wrong,
and no amount of bookkeeping during the delete pass makes it right.

## Cold tier

Deletion is **hot only**. Cold Parquet is not rewritten.

`cold_boundary_at` — the tiering watermark — is captured at preview, and rows in
range that fall on the cold side of it are counted and reported as
`cold_rows_skipped`. The admin therefore sees exactly what will survive the
purge *before* confirming, rather than discovering it afterwards. This follows
`inspector_mask_actions`, which carries the same two fields for the same reason.

The consequence is explicit: bad data that has already rotated to cold survives
the purge and remains in the charts. For the motivating case this is usually
moot, since data bad enough to purge is normally noticed while it is still hot.
Recompute still reads cold, so the counters remain truthful about what actually
remains — they will simply reflect the surviving cold rows.

The job model and report are shaped so a cold-rewrite phase could be added later
without changing the API or the UI.

## Execution

A third supervised loop in the existing **`sauron-tier`** worker, alongside its
tiering and restore loops. No new binary, no new systemd unit, no new entry in
`packaging/rpm/binaries.txt`.

**Not `sauron-inspector`, which was the first choice.** That binary's
`Cargo.toml` states it must not link DuckDB — *"No Redis and NO DuckDB,
deliberately: this binary must not inherit the unbundled libduckdb constraint
across a fourth build path"* — and it currently links clean without
`libduckdb.so` on the path, because its `sauron-tier` dependency is declared but
unused. Since recompute **must** read the cold half (a Postgres-only recompute
silently undercounts, see above), putting the purge there would have forced
libduckdb into a fourth build path: the RPM spec, the Dockerfile, and the local
dev environment would each have needed updating, and the failure mode is a link
error at package time rather than anything a `cargo check` would surface.

`sauron-tier` is the natural home regardless of that constraint: it already
links DuckDB, already owns the hot/cold watermark the purge needs for its
boundary, and already runs a claim/lease worker loop (`run_one_restore`) with
the exact shape the purge needs.

The loop claims a `pending` job (`worker_id` + `claimed_at`), advances through
kinds in batches driven by the cursor triple, then transitions `phase` from
`delete` to `recompute` and drains `purge_touched_keys`. Cancellation is checked
between batches: it stops further work but does **not** restore already-deleted
rows, and the job report shows how far it got.

## Audit

New actions `data_purge.preview`, `data_purge.confirm`, `data_purge.cancel`
added to `AUDITED` in `bins/sauron-api/src/audit.rs`, with `data_purge` as the
entity type and its own `changes` allowlist covering scope and counts only.

`routes::purge` must **also** be added to the `SOURCES` list in
`tests/audit_coverage.rs`. This is a known hole in the drift guard rather than a
speculative one: `routes::failures` was added to `AUDITED` without being added
to `SOURCES`, so `audited_handlers_actually_call_record` verified nothing for
those handlers and the suite still went green. Adding the module to one list and
not the other produces coverage that is reported but not checked.

## Testing

**Pure unit** (no database): kind dependency resolution; the rollup straddle
rule against a span and a range; cursor advance and batch boundaries;
`all_time` versus null-range validation.

**Integration against real Postgres** in `sauron-db`: delete pass correctness
per kind; recompute restoring counters to the truth after a partial delete;
zero-evidence rollup removal; the unattributed-environment case. These require
`dangerouslyDisableSandbox` and host-network containers — the Bash sandbox has
its own network namespace, so DB-backed tests otherwise return early while
printing `ok`, and a fully green run can mean nothing ran.

**Runtime drive** via `docker compose`: seed an app with known signal, purge a
sub-range, and assert the Sessions / Devices / Users / Issues screens agree with
the surviving raw rows. The rollup-consistency bug class this feature exists to
fix is invisible to unit tests by construction, since it lives in the
disagreement between two tables.

## Four bugs implementation found, and what caught each

Recorded because each is a class, not a one-off, and three of them produce a
purge that *looks* like it worked.

1. **The worker fence gated the wrong thing.** Every batch ends with
   `UPDATE purge_jobs … WHERE id = $n AND worker_id = $m`, which reads like a
   lease check. In Postgres a data-modifying CTE runs *regardless* of whether
   the outer statement's `WHERE` matches: the `DELETE` executed, the `UPDATE`
   matched nothing, and the caller got "I lost the claim" while the rows were
   already gone and no counter recorded them. Fixed with a `fence` CTE that
   every mutating arm gates on. **Caught by:** a real-Postgres test asserting
   the surviving row count, not the return value. No unit test can see it.

2. **`environment_id` means the enrollment id, not the catalogue id.** The
   migration text says `REFERENCES environments(id)` and is wrong — migration
   000033 renamed tables while keeping OIDs. Both are UUIDs, so nothing catches
   the confusion; an env-scoped purge would have matched zero rows and reported
   success. **Caught by:** querying `pg_constraint` on a freshly migrated
   database instead of trusting the DDL.

3. **`issue_id` exists only on `error_events`.** The recompute probed all three
   raw tables for whatever the key column was, so purging issues failed the job
   at the recompute phase — *after* the delete phase had run, leaving rows gone
   and every counter stale. **Caught by:** a live end-to-end drive. The
   integration test missed it because it called `apply_recomputed_rollup`
   directly and never went through `hot_counts_for_key`. The regression test now
   loops over the real kind vocabulary, so a kind added later is covered
   automatically.

4. **`event_users` has no counters at all.** They live on
   `event_user_environments`, one row per environment, so a person's repair is a
   per-environment re-aggregation rather than a single row update. Same failure
   shape as (3): delete done, recompute dead. **Caught by:** the same live
   drive, one fix later.

The pattern across (1), (3) and (4): the delete phase succeeds and the
recompute phase fails, which is the *worst* outcome the two-phase design has —
strictly worse than not running at all. A purge job that ends `failed` after
its delete phase should be treated as an incident, not a retry.

## Risks

**Live ingest races the recompute.** If the app is still receiving events while
the job runs, counters recomputed mid-flight drift as soon as they are written.
The mitigation is procedural, not technical: the UI states that the bad sender
should be stopped first. The job additionally records `ingest_active` as
observed at start, so a confusing result can be explained afterwards rather than
becoming a mystery.

**No undo.** Preview, the typed slug, and cancellation are the only protections.
Once a batch commits, those rows are gone. Cold rows are the accidental
exception, and they are reported rather than relied upon.

**Recompute is the expensive phase.** It is per touched key across two storage
engines, so a purge touching many distinct sessions costs far more than the
row count alone suggests. The cursor and cancellation are what keep that
bounded.
