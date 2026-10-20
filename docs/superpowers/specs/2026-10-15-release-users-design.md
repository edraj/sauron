# Release-aware Users page — design

Date: 2026-10-15. Status: approved in chat ("Full", approach A), implemented same day.

## Problem

Picking a release in the Topbar switcher left the Users page (`/users`) unchanged. That was
by design in the 2026-09-14 release-filter programme (filter-only scope, lists only), but the
Users page reads rollups keyed by (app, env, day) and `event_users`, none of which carry a
release. A per-release answer needs release-keyed storage; raw scans would bring back the
30 s `TimeoutLayer` 503s the environment rollups removed.

## Semantics (user-approved)

With release `R` picked (`?release=R`, `none` = Unknown = SQL NULL release):

| Widget | Meaning under `R` |
|---|---|
| Total users | people with ≥1 event, error, transaction or session on `R` (ever) |
| Active (window) | people whose LAST signal on `R` falls in the window |
| New (window) | people whose FIRST signal on `R` falls in the window (adoption) |
| identified splits | same sets, `event_users.identified_at IS NOT NULL` (current flag) |
| DAU / WAU / MAU, active-per-day chart | distinct people with an analytics or error event on `R` that day/span (HLL) |
| new-per-day chart | people whose first signal on `R` is that day |
| avg / median session | sessions whose latest reported release is `R` (the session row's `release`) |
| persons list (default `release_mode=used`) | people who used `R`; first/last seen and counts are per-release |
| persons list `release_mode=current` | as `used`, but only people whose most recent signal (in the env scope) is on `R` |

One person counts under every release they touched. No release picked = today's behaviour,
byte-identical (none of the new code runs).

## Storage (migration 82, `2026-10-15-000082_release_users`)

Release is stored `text NOT NULL` with `''` meaning Unknown. Ingest normalises blank → NULL
and `backfill-releases` repaired history, so `''` can never be a real release; storing it
NOT NULL keeps every unique index plain and every read a single `release = $n`.

- `person_releases (app_id, environment_id NULL, release, distinct_id, first_seen, last_seen,
  events_count, errors_count, sessions_count, updated_at)` — the `event_user_environments`
  twin with a release column. Unique `(app_id, release, distinct_id, COALESCE(env, nil))`
  (person before env, so the representative-row probe is a prefix);
  `(app_id, release, last_seen DESC)` / `(app_id, release, first_seen)` for page-first reads;
  `(app_id, distinct_id, last_seen DESC)` for the `current` probe.
- `user_activity_release_daily (app_id, environment_id NULL, release, day, hll_all,
  hll_identified, updated_at)` — the two sketches Users reads (no event/error counts, no
  analytics-only sketch). Unique `(app_id, release, day, COALESCE(env, nil))` plus `(day)` for
  the consistency rebuild's per-day delete. Sketch merge is idempotent, so re-folding a day
  never inflates it.
- `session_stats_release_daily (app_id, environment_id NULL, release, day, sessions,
  duration_ms_sum, duration_hist, updated_at)` — REPLACE-maintained like `session_stats_daily`.
- `release_rollup_epoch` (one row, stamped in the migration) and `release_rollup_backfill
  (app_id PK)` markers. Read gate `release_users::is_ready(app)` = marker OR app created at/after
  the epoch (the `rollups::is_ready` shape).

## Write paths

- Batch ingest: `Acc` folds `PersonReleaseBump` keyed `(app, did, env, release)` beside the
  person-env fold (same signals, transactions included); `write_rows_once` credits newly
  inserted sessions to the session's release and upserts `person_releases` in the same
  transaction, after `bump_person_envs` (lock order: sorted by conflict key). A new session is credited to
  the release of its FIRST signal in the batch (`WriteSet::session_start_releases`), matching the per-item path.
- Per-item ingest: `repo::bump_person_release`, same conflict arm, best-effort like its siblings.
- Rollup fold: `ARow`/`ERow` gain `release`; the analytics/error folds also build release
  sketches and `add_release_activity` merges them in the same advisory-locked transaction.
  `fold_day_body` folds them too and its `delete_first` list includes the release table.
- `recompute_sessions` also REPLACEs `session_stats_release_daily` for the dirty (app, day)s.
- Identity merge (`identity_merge::fold_rollups`) moves `person_releases` rows alias → person
  (DELETE … RETURNING + additive upsert). Purge: `person_releases` is a Persons rollup
  companion and `recompute_person` re-derives it from surviving raw rows.

## Backfill

`release_users::backfill_all(pool)`, run by the unattended `auto_backfill` (after the existing
steps) and by `sauron-migrate backfill-release-users`:

1. Day pass, resumable (`release_rollup_backfill_days` records finished days in the same
   transaction as their writes): for every day from the oldest raw row to today, fold the
   day's analytics + error rows into release sketches (merge — idempotent, safe to overlap the
   live fold) and REPLACE the day's release session stats.
2. Per app without a marker: aggregate `person_releases` from raw rows. Membership and
   first/last seen use ALL rows (LEAST/GREATEST are idempotent, so this closes the
   migration→new-binary gap); counts add only signals before the epoch (additive against the
   cutoff — never `DO NOTHING`, the migration-70 lesson). Marker in the same transaction.

## API

`?release=` now accepted (release guard allowlist + dashboard `RELEASE_SCOPED_URL`, parity
tested) on:

- `GET /v1/apps/{app_id}/users/summary` — same `UsersAnalytics` body.
- `GET /v1/apps/{app_id}/persons` and `GET /v1/apps/{app_id}/counts/persons` — also take
  `release_mode=used|current` (default `used`; ignored without `release`; anything else 400).

When the app's release tables are not ready: **503** with code `release_rollup_pending`. No
raw fallback.

## Dashboard

`RELEASE_AWARE['/users'] = true`; summary, list and count view keys use
`scopeKeyWithRelease`; a "Used it / Currently on it" toggle shows beside the list only while a
release is picked; `release_rollup_pending` renders "Release stats are still building" instead
of an error; the "charts and totals include all releases" note no longer shows on Users.

## Disclosed approximations (`docs/approximate-analytics.md`)

- Release identified split = the identified flag when the events were folded; the nightly
  `person_days` recompute has no release dimension, so a late `identify()` is not re-applied.
- Session duration tiles bucket a session by its LATEST reported release (the session row's `release`, overwritten by each newer non-null value); the persons list's per-release `sessions_count` credits the release the session was CREATED under (first signal — both ingest paths). They differ only for a session spanning an app update.
- `person_releases` counter drift: same accepted class as `event_user_environments`.

## Tests

- `crates/sauron-db/tests/release_users.rs`: 2 releases × 2 envs + Unknown fixture; the
  release reads vs raw-SQL oracles (exact legs exact, sketches ±2%), `current` mode, the
  readiness gate, backfill additive-against-epoch + idempotent rerun.
- Pipeline equivalence test diffs `person_releases` between batch and per-item paths.
- `http_release_scoping.rs` sweep picks up the 3 new allowlisted routes; a 503 test for the
  not-ready gate.
- Dashboard parity tests + a browser drive.

## Read cost (post-review)

Every read picks ONE representative `person_releases` row per person with a `NOT EXISTS` probe
(latest `last_seen`, earliest `first_seen`, or lowest environment), omitted under a
single-environment scope. Its own column then equals the person's aggregate, so:

- persons list, durable sorts: index order + `LIMIT` → O(page);
- `/counts/persons`: the window column's index range, stopped at the count cap;
- Active / New / new-per-day: the window's index range only;
- Total: O(people on the release), the release-blind page's `count(*)` class;
- count sorts (`events_count`, …): grouped then sorted, O(people on the release), as on the
  release-blind page.
