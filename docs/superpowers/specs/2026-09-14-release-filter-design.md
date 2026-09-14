# Release as a global dashboard filter

Date: 2026-09-14
Status: approved design, not yet planned

## Goal

Make `release` a first-class dimension in the dashboard so the drill-down
reads org → project → app → release → environment, and make `release`
mandatory at SDK init so every event ingested by a current SDK carries it.

This iteration is filter-only. Release and environment stay independent
dimensions on the same events; one build of release `1.4.0` may report from
staging and prod under the same ingest key. No rollup is re-keyed.

## Decisions taken during brainstorming

| Question | Decision |
| --- | --- |
| Is "app version" a new concept or the existing `release`? | The existing `release` field, promoted. No rename on the wire. |
| Nesting of release and env | Filter ordering only. Envs are still enrolled per app; ingest still derives env from the public key. |
| Strictness of "mandatory" | SDK init throws without a release. Ingest keeps accepting envelopes with no release and stores NULL. |
| Reach of the global selector | List pages only. Rollup-backed aggregate pages ignore release this iteration and say so. |

## Out of scope (explicitly deferred)

- Any per-release aggregate: adoption over time, crash-free rate per
  release, session share, perf percentiles per release. These need a
  release-keyed rollup (`release_stats_daily` was sketched) and a backfill.
- Re-keying existing rollups on release.
- A managed release catalogue with retire/notes state. `app_releases` below
  is an observed table, not a managed one.

## 1. Data model — migration 78

### `app_releases`

```sql
CREATE TABLE app_releases (
    id             BIGSERIAL PRIMARY KEY,
    app_id         UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id UUID NULL REFERENCES app_environments(id) ON DELETE CASCADE, -- enrollment id, NULL = unattributed
    release        TEXT NOT NULL,
    first_seen_at  TIMESTAMPTZ NOT NULL,
    last_seen_at   TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX app_releases_identity_idx
    ON app_releases (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'), release);
```

The unique index carries the identity because Postgres cannot put an
expression in a PRIMARY KEY; the COALESCE follows the rollups' convention
for the unattributed environment. The ingest upsert targets this index
with `ON CONFLICT (app_id, COALESCE(environment_id, ...), release)`.

`environment_id` is the enrollment id (`app_environments.id`), never the
catalogue id, per the warning in migration 59.

Maintained by `sauron-ingest`: after each accepted batch, one
`INSERT ... ON CONFLICT DO UPDATE SET last_seen_at = GREATEST(...)` per
distinct (app, env, release) in the batch. Batches are ≤ 50 envelopes, so
this is one statement per batch in the common case. Envelopes with no
release write nothing. The upsert runs as the batch's LAST stage, after every
row it writes has committed: a row here is a claim that this app has telemetry
on that release, nothing ever deletes from this table, and a batch that failed
mid-way wrote no telemetry at all.

**`first_seen_at` and `last_seen_at` are server RECEIVE times** —
`received_at`, not `occurred_at`. Both seeding paths use it: the pipeline
stamps the job's `received_at`, and the backfill below takes
`MIN`/`MAX(received_at)`. One clock, for two reasons. A catalogue built on
`occurred_at` would make a backfilled release's `first_seen_at` jump backwards
the first time that release was re-sighted live, because the two paths would
be reading different columns; and `occurred_at` is a device clock, so a phone
set to 2019 could date a release to 2019 and pin it to the top of a
`first_seen_at` ordering forever.

### History backfill (operator-run)

New `sauron-migrate backfill-releases` in the existing `COMMANDS` list.
It seeds `app_releases` from `error_events` and `analytics_events`, taking
`MIN(received_at)` / `MAX(received_at)` per (app, environment_id, release) —
see the clock note above. Sessions and transactions are not read for the seed;
the two event tables cover every release that ever reported.

It first REPAIRS those two tables, which is the only part of this feature that
writes to them: the edge's trim-and-blank-to-NULL rule is forward-only, so
history still holds `''`, `'\t'` and `' 1.4.0 '`, and a padded value is a
different release from its unpadded twin to every filter in the query layer.
Blank-ish values become NULL and padded ones are trimmed, on both tables,
before anything is seeded — and the seed then groups by the normalised value,
so `' 1.4.0 '` and `'1.4.0'` fold into one switcher entry. "Blank" means Rust's
`str::trim`, i.e. Unicode `White_Space`: Postgres's `\s` is a libc class that
does not contain U+00A0, so the SQL spells the set out (`sauron_db::releases`'s
`WS`).

The repair runs one statement per child partition rather than one against each
parent, so it locks a partition at a time instead of every partition of both
tables for the whole run (which would block `sauron-tier`'s `DETACH`); stop
`sauron-tier` for the run regardless.

Idempotent and resumable: re-running upserts with `LEAST`/`GREATEST`, the
repair is a no-op the second time, and an interrupted run keeps every
partition it finished. Runbook entry added next to the rollup backfills so it
is not forgotten on the remote.

### Indexes

- `sessions (app_id, release, last_event_at DESC)` — sessions list sorts on
  `last_event_at`. Partitioned table, so this is created per partition.
- `transactions (app_id, release, occurred_at DESC)`.

Both are `CREATE INDEX` inside the transactional migration, so they take a
lock while building on the remote. The migration note says to run at low
traffic. Error and analytics events already have the equivalent index.

## 2. SDKs

Wire name stays `release` in the envelope header. No ingest or pipeline
change other than the `app_releases` upsert.

Every SDK validates `release` at init the same way it validates `dsn`:

| SDK | Behaviour today | New behaviour |
| --- | --- | --- |
| JS | `release` optional → null | `resolveOptions` throws unless `release` is a non-empty string |
| Node | same | `init` throws unless `release` is a non-empty string |
| Python | `release=None` | present dsn + missing/empty release raises `ValueError`; empty dsn still means disabled and skips the check |
| Flutter | optional | `SauronOptions` asserts non-empty `release` when `dsn` is set; null/empty dsn still disables |
| C# | optional | `SauronClient` ctor throws `ArgumentException` when `Release` is null/empty |

Whitespace-only counts as empty. Value is trimmed before sending.

Each SDK gets one init-validation test and a minor version bump. README,
`sdks/PUBLISHING.md`, and the wiki init snippets are updated so every
example shows `release`.

## 3. Backend API

### `GET /v1/apps/{app_id}/releases`

Returns `[{ release, environment_ids: [uuid|null], first_seen_at, last_seen_at }]`
grouped by release across envs, ordered by `last_seen_at DESC`. Accepts
`?environment_id=` with the existing `EnvFilter` semantics to narrow to one
env. Needs the same read scope as the environments list. Not cached; the
table is tiny.

### `?release=` on list routes

Accepted on exactly these route families:

- issues list
- error occurrences list
- analytics events list
- sessions list
- transactions list

Semantics mirror `environment_id`: absent = all releases, literal `none` =
`release IS NULL`, any other value = exact match. Lowered by ANDing the
existing `Store::Column("release")` leaf into the prepared plan, so it
composes with search chips and the `release:` DSL term. Parsed by a
`parse_release` helper next to `parse_env` in `routes/scope.rs`.

Because `none` is that wire literal, a release *literally named* `none`
cannot be selected in the dashboard: the switcher would show two entries
meaning different things under one id, so the real release is skipped and only
the "Unknown release" pseudo-entry remains. Ingest still accepts and stores it
verbatim; it is reachable through the `release:none` DSL term and the API.

On Issues, `release=none` is a POSITIVE `EXISTS(… e.release IS NULL)` over the
issue's occurrences, mirroring `environment_id=none` — not "has no released
occurrence". An issue with both a `1.4.0` occurrence and a release-less one
therefore appears under BOTH selections, which is correct: `release` is a
property of occurrences, and `issues` has no release column of its own.

Ingest normalises `header.release` at the edge — trimmed, and an
all-whitespace value stored as NULL — so one build cannot appear as several
switcher entries. `sauron-migrate backfill-releases` applies the same rule
when seeding history written before it existed: "blank" is Rust's
`str::trim`, i.e. Unicode
`White_Space`, spelled out in SQL as `btrim(release, <the set>)` because
Postgres's `\s` is a libc class that does not contain U+00A0 (see §1 and
`sauron_db::releases`'s `WS`).

Every other **GET** route rejects a present `release=` (empty or not) with 400
via one middleware (`release_guard.rs`), following the fail-loud rule already
used for `environment_id`. Non-GET routes are not guarded: `POST
/artifacts?release=` reads `release` as the upload's own attribute.

## 4. Dashboard

### Store

`sessionStore.currentRelease: string | null`, persisted under
`sauron.release:{appId}` so switching apps restores each app's last pick.
`null` = all releases, `'none'` = unknown. Cleared when the app changes and
the stored value is not in the freshly loaded release list.

### Picker

`AppEnvPicker` becomes App → Release → Env. Release list comes from the new
endpoint. When a release is selected, the env dropdown shows only envs whose
id appears in that release's `environment_ids`; if the current env is not
among them, env resets to all. "All releases" is the first option.

### Request scoping

`scope.ts` gains a `RELEASE_SCOPED_URL` opt-in list matching the five route
families in section 3 and attaches `release` only there. A parity test
reads the server's accepting routes (the `parse_release` call sites) and
the client list from source, following the existing cross-source parity
pattern, so the two cannot drift.

### Aggregate pages

While a release is selected, every rollup-backed page (overview, screens,
journeys, perf, retention, users, device groups, persons) shows a small
inline note "Showing all releases" under the page title. Driven by a
`SHELL_FLAGS`-style constant per page so it is parity-tested rather than
hand-placed.

### i18n

New strings added to both English and Arabic catalogues. The
untranslated-string test is not trusted as proof; the Arabic page is checked
in the browser drive.

## 5. Testing

Backend
- Plan-lowering unit tests for `release=` on all five route families,
  including `none` and composition with a `release:` chip.
- Route tests on a real database for the releases endpoint, the 400 on
  unsupported routes, and env narrowing. Run outside the sandbox netns;
  sandboxed DB tests print green having run nothing.
- Ingest test: a batch with two releases across two envs produces the
  expected `app_releases` rows, and a second batch bumps `last_seen_at` only.
- Backfill test: seed events, run `backfill-releases`, assert rows and
  idempotence.

SDKs
- One test per SDK: init without release throws; with whitespace throws;
  with a value the envelope header carries it trimmed.

Dashboard
- Store persistence and reset-on-app-change.
- Interceptor attaches `release` only to the opt-in list.
- Parity test client list vs server accepting routes.
- Parity test for the "Showing all releases" page constant.
- Browser drive: pick a release, confirm env list narrows, confirm the
  issues list request carries `release=`, confirm the overview shows the
  note. The Browser pane must be visible during the drive.

## Rollout order

1. Migration 78 + ingest upsert + backfill command (backend only, no
   behaviour change until data exists).
2. API: releases endpoint, `release=` on the five lists, reject elsewhere.
3. Dashboard picker, store, interceptor, notes.
4. SDK validation and publish.

Steps 1–3 ship together in one backend + dashboard release. Step 4 can lag;
old SDKs keep working and land in "Unknown".
