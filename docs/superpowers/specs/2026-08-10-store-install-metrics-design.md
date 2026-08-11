# App Store install & uninstall metrics

Date: 2026-08-10
Status: approved design, not yet implemented

## Summary

Pull daily install and uninstall counts from Google Play and the Apple App
Store, store them per app per store per day, and show them as one diverging-bar
chart on Overview — visible only when the environment the admin designated as
"the store version" is selected.

Three user-facing pieces:

1. An admin designates one environment per app as the store version.
2. App Settings holds each store's identifiers and credentials.
3. Overview grows a store section, conditional on that environment being
   selected.

## Background: what the stores actually give you

Neither store answers a simple REST call with install counts. This shapes
everything below, so it is stated first.

**Google Play.** Daily installs *and* uninstalls come from the Play Console's
Google Cloud Storage reports bucket (`gs://pubsite_prod_rev_*`), as one CSV per
package per month at
`stats/installs/installs_{package}_{YYYYMM}_overview.csv`, read with a service
account. The Play Developer Reporting API covers vitals (crash rate, ANR rate),
not installs. Two properties of these files cost a day each if you meet them by
surprise:

- The CSV is **UTF-16LE with a BOM**. Reading it as UTF-8 yields garbage that
  parses as a valid single-column CSV.
- Files are **monthly**, so a 90-day backfill is four object fetches, not
  ninety.

Relevant columns: `Daily Device Installs`, `Daily Device Uninstalls`.

**Apple.** The classic Sales & Trends API reports downloads but has no concept
of an uninstall. Deletions exist only in the App Store Connect **Analytics
Reports API**, which is request-then-poll:

1. `POST /v1/analyticsReportRequests` with `accessType: ONGOING`, once per app.
2. `GET /v1/analyticsReportRequests/{id}/reports`, filtered to the
   "App Store Installations and Deletions" report.
3. `GET /v1/analyticsReports/{id}/instances?filter[granularity]=DAILY`
4. `GET /v1/analyticsReportInstances/{id}/segments` → a URL to a gzipped CSV.

Apple takes roughly 24–48h after the request is created before the first
instance appears, and retains about 365 days. That waiting period is a normal
state, not a failure — see "Sync states" below.

**Consequence for environments.** Both stores key their data to a package name
or bundle id. Neither knows your environments exist. Designating an environment
as "the store version" is therefore a *visibility and attribution* choice, not a
data partition, and the schema does not pretend otherwise:
`store_daily_metrics` has no `environment_id`.

## Decisions

| # | Decision | Rationale |
|---|---|---|
| 1 | Apple uses the Analytics Reports API (installs + deletions), not Sales & Trends | Parity with Google. Without it the "combined" chart has no Apple uninstall series at all |
| 2 | The store-environment designation is a field on the app, configured in the store settings card | One build ships to both stores, so one designation covers both. Keeps all store setup on one page |
| 3 | The chart is diverging bars: installs above the zero line, uninstalls below, each stacked by store | Net growth is readable at a glance; four numbers per day stay legible at 90 days, which grouped bars do not |
| 4 | Sync runs in a new `sauron-storesync` binary backed by a `sauron-store` crate | Mirrors `sauron-monitor`. A store-API outage cannot touch probing or alerting; separate schedule and log stream |
| 5 | Credentials reuse `NOTIFY_SECRET_KEY` and `sauron_alerts::SecretCipher` | Same class of secret. A second key is an ops hazard: a mismatch is invisible until the moment it matters |

## Data model

Migration `2026-08-10-000049_store_metrics`.

### `apps.store_environment_id`

Nullable UUID referencing `app_environments(id) ON DELETE SET NULL`.

This is the store-version designation. It lives on `apps` rather than in its own
1:1 table because it is a single nullable property of the app, and it references
the *enrollment* id (not the catalogue id) because that is the id the dashboard's
environment switcher and `?environment_id=` already carry.

`ON DELETE SET NULL`, so retiring an environment degrades to "the section is
hidden" rather than leaving a dangling reference or blocking the delete.

### `app_store_connections`

One row per (app, store).

| Column | Type | Notes |
|---|---|---|
| `id` | UUID PK | |
| `app_id` | UUID NOT NULL | → `apps(id) ON DELETE CASCADE` |
| `store` | TEXT NOT NULL | CHECK in `('google_play','app_store')` |
| `enabled` | BOOL NOT NULL DEFAULT true | |
| `identifiers` | JSONB NOT NULL DEFAULT `'{}'` | Non-secret, displayable |
| `secret_enc` | BYTEA NULL | AES-GCM, `SecretCipher` |
| `sync_state` | JSONB NOT NULL DEFAULT `'{}'` | |
| `next_sync_at` | TIMESTAMPTZ NOT NULL DEFAULT now() | Claim key |
| `last_synced_at` | TIMESTAMPTZ NULL | |
| `last_error` | TEXT NULL | |
| `created_at`, `updated_at` | TIMESTAMPTZ NOT NULL | |

`UNIQUE (app_id, store)`. Index on `(next_sync_at) WHERE enabled` for the claim
query.

`identifiers` is JSONB rather than seven columns that are half NULL on every
row, because the two stores need disjoint field sets. It deserializes into a
`store`-tagged Rust enum and is validated at the API boundary, so an unparseable
row is a loud error at read time rather than a silent empty sync:

- **Google Play**: `{ package_name, gcs_bucket }`
- **Apple**: `{ bundle_id, apple_app_id, issuer_id, key_id, vendor_number }`

`secret_enc` holds the Play service-account JSON or the Apple `.p8` private key.
`sync_state` holds Apple's `analyticsReportRequests` id, which is created once
and reused for the life of the connection.

### `store_daily_metrics`

| Column | Type |
|---|---|
| `app_id` | UUID NOT NULL → `apps(id) ON DELETE CASCADE` |
| `store` | TEXT NOT NULL |
| `day` | DATE NOT NULL |
| `installs` | BIGINT NOT NULL DEFAULT 0 |
| `uninstalls` | BIGINT NOT NULL DEFAULT 0 |
| `updated_at` | TIMESTAMPTZ NOT NULL |

`PRIMARY KEY (app_id, store, day)`.

Two properties this table must have, both easy to get wrong:

- **No `environment_id`.** See "Background". The store does not segment by
  environment and the schema must not imply it does.
- **Writes are `ON CONFLICT DO UPDATE SET`, never `+=`.** Both stores restate
  recent days as their pipelines settle. An additive upsert inflates every
  number on every sync, and the resulting chart looks plausible.

## The sync daemon

### `sauron-store` crate

```
crates/sauron-store/src/
  lib.rs      StoreConnector trait, shared types, the tick body
  google.rs   Play: OAuth2 + GCS object read + UTF-16LE CSV parse
  apple.rs    Apple: ES256 JWT + report request/instance/segment walk + gzip CSV parse
```

`StoreConnector` is the isolation boundary: given identifiers, a decrypted
secret, and a date range, return `Vec<DailyMetric>` or an error. It knows
nothing about Postgres, which is what makes both connectors testable against
fixture files with no network and no database.

### `sauron-storesync` binary

The `sauron-monitor` shape, deliberately:

1. Claim `enabled AND next_sync_at <= now()` with `FOR UPDATE SKIP LOCKED`.
2. Fetch concurrently under a semaphore.
3. Upsert into `store_daily_metrics`.
4. Reschedule `next_sync_at`; record `last_synced_at` or `last_error`.

One row's failure is written to that row's `last_error` and nothing else — the
other store, and every other tenant, syncs normally.

Configuration:

| Variable | Default | Meaning |
|---|---|---|
| `STORE_SYNC_INTERVAL_SECS` | `21600` (6h) | Reports are daily and lag 1–3 days |
| `STORE_SYNC_MAX_CONCURRENCY` | `8` | Pool sized to this + headroom, as the monitor does |
| `STORE_BACKFILL_DAYS` | `90` | On first sync of a connection |
| `NOTIFY_SECRET_KEY` | — | Existing. Fail-closed, no derivation fallback |

At startup the daemon performs a decrypt self-test against one stored
`secret_enc`, in the style of `repo::any_channel_secret_enc`, so a key mismatch
is a boot failure rather than a silent stream of sync errors.

### Parsing rules

Both parsers **map columns by header name, not by position**, and fail with an
error naming the missing header. Store report layouts change; an index-based
parser that shifts by one column produces numbers rather than errors.

**Apple's report is `Event`-shaped, not column-shaped.** This was the design's
one open unknown; it resolved against Apple's documentation *after* the first
implementation, and the first implementation was wrong. The report carries:

```
Date | Event | Counts | Unique Devices | App Apple Identifier | App Name |
App Version | Device | Territory | Platform Version | Source Type |
Source Info | Page Type | Page Title | Download Type | App Download Date
```

There are **no `Installations` / `Deletions` columns**. Installs and deletions
are *rows* discriminated by `Event`, and one calendar day is crossed by every
dimension above — so a day's figure is the SUM over its rows, not a cell.

Three mappings follow, each a named constant in `apple.rs`:

- **`Unique Devices` is the count column**, with `Counts` as a fallback when a
  report variant omits it. `Unique Devices` is the direct analogue of Play's
  `Daily Device Installs`; `Counts` totals events, so a redownload on one device
  counts twice and the two halves of the chart would be measuring different
  things.
- **Installs = `Install` + `Reinstall`. `Update` is excluded.** Play reports
  upgrades in a separate column this connector already ignores, so excluding
  updates is what makes the stores comparable — not a judgement about which
  number is more interesting. Counting them would inflate the App Store line
  several-fold and still look plausible.
- **Uninstalls = `Delete` / `Deletion`** (both spellings; the column is
  documented by description rather than by enumerated value).

An `Event` value in none of those sets is counted as neither and named in a
`WARN`, rather than guessed at — silently folding an unknown value into installs
is worse than visibly under-reporting it.

The committed fixture is shaped like the real report (multi-row days, both count
columns, an `Update` row that must not be counted) but is synthetic. Replacing it
with a genuine downloaded segment remains worthwhile; it would confirm the
`Event` spellings, which are the one thing documentation describes rather than
enumerates.

### Network posture

Both connectors talk to fixed hosts (`storage.googleapis.com`,
`api.appstoreconnect.apple.com`, plus Google's OAuth endpoint). Those hosts are
pinned to an allowlist. The SSRF-guarding resolver used by `sauron-monitor` is
not needed here because no operator-supplied URL is ever fetched — the only
operator input is a bucket name and identifiers, which are interpolated into
paths on pinned hosts.

### Sync states

Surfaced verbatim in App Settings and, where relevant, on Overview:

| State | Meaning |
|---|---|
| `never_synced` | Saved, not yet picked up by a tick |
| `pending` | Apple only: report requested, no instance published yet (~24–48h) |
| `ok` | Last sync succeeded; `last_synced_at` shown |
| `error` | `last_error` shown verbatim |

`pending` is a first-class state, not an error. Rendering Apple's normal startup
delay as a failure trains admins to ignore a red badge that will later mean
something.

## API

All routes are app-scoped and use the existing app authorization helpers.

| Route | Gate | Notes |
|---|---|---|
| `GET /v1/apps/{id}/store-connections` | `app:read` | Both stores, present or not |
| `PUT /v1/apps/{id}/store-connections/{store}` | `app:update` | Upsert identifiers + optional secret |
| `DELETE /v1/apps/{id}/store-connections/{store}` | `app:update` | Drops the connection, keeps metrics |
| `POST /v1/apps/{id}/store-connections/{store}/sync` | `app:update` | Sets `next_sync_at = now()`, returns |
| `PATCH /v1/apps/{id}` | `app:update` | Gains `store_environment_id` |
| `GET /v1/apps/{id}/store-metrics?since_days=N` | Whatever `GET /v1/apps/{id}/overview/totals` uses today, read off that handler rather than re-derived | Chart feed |

Because `DELETE` keeps `store_daily_metrics`, removing and re-adding a
connection resumes against the existing history: the backfill fills gaps and
overwrites overlapping days, it does not duplicate them. That falls out of the
`(app_id, store, day)` primary key and is not special-cased.

Four rules that are part of the contract, not implementation detail:

**Secrets are write-only.** No response body ever contains `secret_enc` or its
plaintext. Reads return `has_secret: bool` and `secret_updated_at`. The test
asserts on the raw JSON body, not on a typed struct — a struct assertion passes
again the day someone adds the field back.

**Partial secret updates.** The `secret` field on `PUT` is
`Option<Option<String>>`: omitted leaves the stored secret unchanged, explicit
`null` clears it. This is the idiom already used for
`notification_channels.secret_enc` (`repo.rs:9704`). Without it, editing a
package name silently wipes the credential.

**`store_environment_id` is validated** to be an enrollment *of this app*.
Anything else is a 400, not a stored value that hides the section forever.

**Queued sync, honestly labeled.** `POST .../sync` only moves `next_sync_at`
forward; the daemon does the work. No multi-minute Apple download happens inside
an HTTP request. The button says "Queue sync" and the UI says the daemon will
pick it up, because "Sync now" followed by unchanged data is a lie told on every
click.

### `GET /store-metrics` response

```jsonc
{
  "series": [
    { "day": "2026-08-07",
      "google_play": { "installs": 1240, "uninstalls": 310 },
      "app_store":   { "installs": 880,  "uninstalls": 195 } }
  ],
  "pending_days": [
    { "day": "2026-08-09", "reason": "App Store has not published this day yet" }
  ],
  "stores": [
    { "store": "google_play", "state": "ok",
      "last_synced_at": "2026-08-10T02:14:00Z", "last_error": null },
    { "store": "app_store", "state": "pending",
      "last_synced_at": null, "last_error": null }
  ]
}
```

Days a store has not published are returned in `pending_days` and **omitted from
`series`**, never zero-filled. A zero bar asserts that nobody installed the app
that day, when the truth is that the report has not shipped. This mirrors the
existing `partial_days` field on `ActiveUsersSeries`, which exists for the same
reason at the hot/cold watermark.

## Dashboard

### New files

| File | Why |
|---|---|
| `lib/api/stores.ts` | Typed client, alongside the other per-domain clients |
| `lib/components/settings/StoreConnectionsCard.svelte` | `SettingsApp.svelte` is 242 lines; inlining this roughly doubles it |
| `lib/components/StoreSection.svelte` | `Overview.svelte` is 460 lines with five `CachedView`s already |
| `lib/components/StoreInstallsChart.svelte` | The diverging chart |

### App Settings

One card: a "Store environment" dropdown over the app's enrollments, then a
block per store with its identifier fields, a paste area for the credential
(service-account JSON / `.p8`), the sync state line, Queue sync, and Remove.
Remove confirms with explicit text that the collected history is kept.

### Overview

A `CachedView<StoreMetrics>` keyed with `viewKey('overview.stores', appId,
scopeKey, days)`, loaded in the existing `Promise.allSettled` batch so it paints
in parallel and its failure cannot abort the other sections.

Visibility, exhaustively:

| Condition | Result |
|---|---|
| No connection configured | Section absent |
| Connection but no `store_environment_id` | Section absent |
| Designated, but a different environment is selected | Section absent |
| Designated environment selected | Visible |
| Visible, Apple `pending` | Visible with Google's data and an inline note |

### Chart

Diverging bars, one column per day: installs stacked upward (two store colors),
uninstalls stacked downward in the same two colors at lower emphasis.

**One shared scale across both directions** — the denominator is the maximum of
(daily install total, daily uninstall total) across the range. Independent
scales for the two halves would put a 3-uninstall day level with a 300-install
day, which is the mistake `UserActivityChart` documents having already made
once.

Tooltip carries all four numbers plus the day. Stat tiles above the chart: total
installs, total uninstalls, net change over the range.

### Codebase traps this must respect

- House UI components only — `Card`, `Button`, `Icon`, `EmptyState`. No raw
  `<button>` or `<table>`.
- `viewKey` includes `scopeKey`. The endpoint takes no environment argument, so
  omitting it serves one environment's response under another's key.
- **`vendor_number` is a TEXT input.** `bind:value` on `<input type="number">`
  writes back `number | null`; because the disabled state is itself a derived,
  computing the guard is what throws, so the DOM freezes while the button still
  looks clickable.
- Values compared by identity use `$state.raw` — `$state` deep-proxies stored
  objects so `===` never matches.

## Testing

**Rust unit (no network, no database).**

- Play parser against a genuine UTF-16LE fixture, including the BOM.
- Apple parser against a real gzipped report segment.
- Both parsers assert a missing expected header produces a named error.
- `SecretCipher` round-trip for both credential shapes.
- Apple ES256 JWT claim/header shape.

**Rust integration (real Postgres).**

- Upsert idempotency: sync the same day twice, assert one row and unchanged
  values. This is the additive-upsert bug, caught directly.
- Claim query: two concurrent daemons do not double-claim a connection.

**HTTP.**

- RBAC matrix: `app:read` can list, cannot write; env-scoped grants cannot write.
- No response body contains a secret — asserted against raw JSON.
- `PUT` without a `secret` field preserves the stored one.
- `store_environment_id` from another app is rejected 400.

**Dashboard (vitest).**

- Diverging scale math, including an all-zero range and an uninstall-only day.
- The visibility table above, all five rows.
- `pending_days` renders as a note and contributes no bar.

**Running them.** Backend tests run with `dangerouslyDisableSandbox` and
host-network containers. Under the Bash sandbox's own netns every DB-backed test
returns early while printing `ok`; the real baseline is **1391** passing, and a
run reporting fewer with no failures means tests were skipped, not that they
passed.

## Out of scope

Deliberately excluded; each is a separate spec if wanted later.

- Country, app-version, and device breakdowns.
- Revenue, proceeds, and subscription metrics.
- Ratings and reviews.
- Alerting on install or uninstall spikes.
- Any attempt to attribute store numbers to individual environments.

## Deployment notes

- `sauron-storesync` is added to `packaging/rpm/binaries.txt` **and** to a
  matching `%files` section in `sauron.spec`. rpmbuild fails on an
  installed-but-unpackaged file, which is the check that caught the earlier
  `sauron-alerts` release failure.
- A systemd unit alongside the other daemons.
- RPM upgrades do not re-run `sauron-migrate`. Migration 49 must be applied
  manually after upgrading, or the new binaries meet an old schema.
