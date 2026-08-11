# Sauron — Setup & Operations (RPM install)

Assumes the RPMs from [INSTALL.md](INSTALL.md) are installed. Sauron needs an
external **PostgreSQL** and **Redis/Valkey**; they can be on this host or remote.

## 1. Provision PostgreSQL

Local example (Fedora):

```bash
sudo dnf install -y postgresql-server
sudo postgresql-setup --initdb
sudo systemctl enable --now postgresql
sudo -u postgres psql <<'SQL'
CREATE ROLE sauron LOGIN PASSWORD 'change-me-strong';
CREATE DATABASE sauron OWNER sauron;
SQL
```

Remote Postgres: just note its host, port, database, user, password.

## 2. Provision Redis / Valkey

```bash
sudo dnf install -y valkey        # Fedora 41+ (use 'redis' on RHEL)
sudo systemctl enable --now valkey
```

## 3. Configure the shared connection

Edit `/etc/sauron/sauron.env`:

```sh
DATABASE_URL=postgres://sauron:change-me-strong@localhost:5432/sauron
REDIS_URL=redis://127.0.0.1:6379
RUST_LOG=info,sauron=info
```

Per-service tunables live in `/etc/sauron/{api,ingest,monitor,alerts,tier,inspector}.env`
— the defaults are fine to start. `sauron.env` also holds the settings **more than
one** unit reads (`TIER_HOT_DAYS`, `INSPECTOR_POLICY_CACHE_SECS`,
`INSPECTOR_TAIL_SWEEP_SECS`); moving one of those into a per-service file lets the
services that still read the shared copy disagree with the one that does not.

## 4. Secrets

`/etc/sauron/secret.env` is generated on first install with a random
`JWT_SECRET` **and** a random `NOTIFY_SECRET_KEY`. Verify both are present:

```bash
sudo grep -c '^JWT_SECRET=\|^NOTIFY_SECRET_KEY=' /etc/sauron/secret.env   # expect 2
```

`sauron-api`, `sauron-monitor` and `sauron-alerts` all refuse to start without
either one, and all three read this same file, so they cannot disagree.

To rotate `JWT_SECRET` (invalidates existing sessions and nothing else):

```bash
sudo sh -c 'umask 077; sed -i "s|^JWT_SECRET=.*|JWT_SECRET=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d " \n")|" /etc/sauron/secret.env'
sudo chgrp sauron /etc/sauron/secret.env && sudo chmod 0640 /etc/sauron/secret.env
sudo systemctl restart sauron-api
```

Note the `sed -i` — do **not** overwrite the file with `>`, which is what older
versions of this document told you to do. That would take `NOTIFY_SECRET_KEY`
with it.

> **`NOTIFY_SECRET_KEY` cannot be rotated, and losing it is unrecoverable.**
> It is the only key that decrypts `notification_channels.config_enc` and
> `secret_enc` — the webhook URLs, request headers, SMTP passwords and bot
> tokens of every configured channel. Back this file up. If the value is lost or
> changed, the channels cannot be recovered by any means and must be deleted and
> re-created in the dashboard.
>
> **Upgrading from a release before this key was required?** `%post` appends
> `NOTIFY_SECRET_KEY` set to your existing `JWT_SECRET`, because that is what the
> older build actually derived the cipher from. Do not "tidy" it to a fresh
> random value — the services will start cleanly and then fail every delivery
> with `secret decrypt failed`.

`/etc/sauron/secret.env` is the file for **every** credential the services read
from the environment, not just `JWT_SECRET`. If you enable transactional email,
put the relay password there rather than in `/etc/sauron/api.env`. Both files are
`0640 root:sauron`, so this is not a permissions difference: `secret.env` is
generated at first install instead of being shipped in the package, so an upgrade
never rewrites it and never leaves an `.rpmnew` beside it, and it is the one file
you already back up and protect:

```bash
sudo sh -c 'umask 077; printf "SMTP_PASSWORD=%s\n" "your-relay-password" >> /etc/sauron/secret.env'
sudo chgrp sauron /etc/sauron/secret.env && sudo chmod 0640 /etc/sauron/secret.env
sudo systemctl restart sauron-api
```

Everything else about the relay — host, port, username, TLS mode — goes in
`/etc/sauron/api.env`, which ships with the whole block commented out. See
section 6 for enabling it.

## 5. Run database migrations

Migrations run **automatically**: every `sauron-*` daemon unit carries
`Requires=sauron-migrate.service` + `After=sauron-migrate.service`, so starting
any of them applies pending migrations first and refuses to start if that fails.
`sauron-migrate` has no `RemainAfterExit`, so it re-runs on every daemon start; a
no-op run costs ~30 ms (measured, 46 migrations already applied).

You can still run it explicitly — useful on a fresh install to see the output
before anything is serving:

```bash
sudo systemctl start sauron-migrate
journalctl -u sauron-migrate --no-pager | tail
```

Expected: `migrations up to date`. Re-runnable safely (idempotent).

`sauron-migrate` waits up to `MIGRATE_WAIT_SECS` (default **120**, set in
`/etc/sauron/sauron.env`) for Postgres to accept connections before giving up.
Only the *connect* is retried — a migration that fails on its own SQL fails
immediately and loudly. The unit's `TimeoutStartSec=300` is the hard ceiling, so
keep `MIGRATE_WAIT_SECS` below it.

> **This couples daemon availability to the database.** A Postgres outage longer
> than `MIGRATE_WAIT_SECS` fails the migrate start job, and a failed *start job*
> is never retried by `Restart=on-failure` — the daemons stay down with
> `NRestarts=0` even after Postgres comes back. Recovery is manual:
> `sudo systemctl start sauron-api sauron-ingest sauron-monitor sauron-alerts sauron-tier`
> (plus `sauron-inspector` if enabled). That is the deliberate trade for never
> running a new binary against an old schema. Size `MIGRATE_WAIT_SECS` to cover
> your Postgres restart window.

## 6. Enable and start the services

```bash
sudo systemctl enable --now sauron-api sauron-ingest sauron-monitor sauron-alerts sauron-tier sauron-storesync
systemctl --no-pager status 'sauron-*'
```

- `sauron-api` → `:8080` (dashboard API)
- `sauron-ingest` → `:8081` (SDK ingest)
- `sauron-monitor`, `sauron-alerts`, `sauron-tier`, `sauron-storesync` → no listener
- `sauron-inspector` → installed but **off**; see below

**Do not omit `sauron-alerts`.** It is the sole owner of metric-rule evaluation
(error spike/threshold, event threshold, latency) *and* of draining
`notification_queue`. Without it those rules are creatable in the dashboard and
simply never fire, and no per-user notification is ever delivered — silently, with
nothing in the UI indicating a problem. Monitor up/down alerts fire inline from
`sauron-monitor` and are the only ones that still work.

`sauron-storesync` is idle until an app-store credential is configured in the
dashboard, and it is enabled for the same reason `sauron-alerts` is: the
credential is entered in the UI, so a disabled syncer means the admin sees
"Waiting for the first sync" forever with nothing explaining why.

The shipped vendor preset `/usr/lib/systemd/system-preset/50-sauron.preset`
enables those same six daemons on **first install**, so on a new host the command
above is belt-and-braces. Run it anyway on a host installed before the preset
shipped — that is where the missing `sauron-alerts` lives.

Each of these pulls `sauron-migrate` in first (section 5), so a first start on an
unconfigured `DATABASE_URL` now fails with a dependency-job error after
`MIGRATE_WAIT_SECS` instead of starting and serving 500s.

### Enabling transactional email (optional)

Password reset needs a relay and a link base. Without them `sauron-api` boots and
serves normally and logs one INFO line explaining what is missing, so this can be
done later.

1. Uncomment and set `SMTP_HOST`, `SMTP_PORT`, `SMTP_FROM` and `SMTP_FROM_NAME`
   in `/etc/sauron/api.env`.
2. Put `SMTP_PASSWORD` in `/etc/sauron/secret.env` (section 4), not in `api.env`.
3. Set `DASHBOARD_URL` in `/etc/sauron/api.env` to the origin a **browser** uses
   to reach the dashboard — the nginx vhost, not `http://localhost:8080`. Every
   link in every email is built from it, and it has no default: an email whose
   links point at the recipient's own machine reports success everywhere on the
   server side.
4. `sudo systemctl restart sauron-api`, then confirm with
   `journalctl -u sauron-api -e | grep -i "transactional email"` that it is **not**
   reporting the feature disabled.

### Enabling the PII inspector (optional)

`sauron-inspector` is installed but **off**: with the shipped
`INSPECTOR_ENABLED=false` the unit starts, logs one line and idles without reading
a single telemetry row. Enabling it is deliberate because the scanner reads the
same partitions the ingest path writes.

1. Raise Postgres `max_connections` to at least **150** — see the note in
   section 11. The inspector's own 4-connection pool is what pushes a stock
   host over the edge, but the failure appears as `sauron-api` 500s.
2. On an **upgraded** host, first delete the `TIER_HOT_DAYS=` line from
   `/etc/sauron/tier.env` (section 11). It now lives in `sauron.env`, and a
   leftover copy makes the masker and the tier worker disagree about which
   partitions are still hot.
3. Set `INSPECTOR_ENABLED=1` in `/etc/sauron/inspector.env`, then
   `sudo systemctl enable --now sauron-inspector`.

Masking reaches **hot Postgres only**. Rows already exported to cold Parquet,
anything sitting in the Redis dead-letter queue, and alerts already delivered
still hold the original bytes; the dashboard says so before every confirm.

## 7. Dashboard

1. Set the **browser-facing** URLs in `/etc/sauron/dashboard.env` (public/reverse-proxied addresses):

   ```sh
   API_BASE_URL=https://sauron.example.com/api
   INGEST_BASE_URL=https://sauron.example.com/ingest
   ```

2. Regenerate `config.js` and (re)load nginx:

   ```bash
   sudo /usr/libexec/sauron/sauron-dashboard-config
   sudo systemctl enable --now nginx
   sudo systemctl reload nginx
   ```

   Fedora's stock nginx ships a default `server { listen 80 default_server; }`.
   Either remove it from `/etc/nginx/nginx.conf` or add `default_server` +
   `server_name` to `/etc/nginx/conf.d/sauron-dashboard.conf` so the dashboard is
   served on `:80`. For TLS, terminate at nginx or a fronting proxy.

## 8. Firewall

```bash
sudo firewall-cmd --add-service=http --permanent          # dashboard :80
sudo firewall-cmd --add-port=8080/tcp --permanent         # API (if reached directly)
sudo firewall-cmd --add-port=8081/tcp --permanent         # ingest (SDKs)
sudo firewall-cmd --reload
```

Prefer fronting `:8080`/`:8081` with nginx/TLS rather than exposing them directly.

## 9. Verify

```bash
curl -fsS http://localhost:8080/health && echo               # API up
curl -fsS http://localhost:8081/health && echo               # ingest up
curl -fsS http://localhost:18081/metrics                      # ingest accounting counters
curl -fsS http://localhost/config.js                          # dashboard runtime config
journalctl -u sauron-api -u sauron-ingest --no-pager | tail
```

(If a service exposes a different health path, check `journalctl` for the bound
address logged at startup.)

### Did anything get accepted and then dropped?

`sauron-ingest` answers an SDK `202` the moment an envelope reaches the Redis
stream, so everything after that point — a trim, an exhausted connection pool, a
wedged worker — can lose telemetry the SDK was told had arrived. Four counters on
`/metrics` are the number for that:

```
sauron_ingest_items_accepted_total       # ITEMS enqueued and answered 202 for
sauron_ingest_items_persisted_total      # ITEMS actually written to Postgres
sauron_ingest_items_deadlettered_total   # ITEMS that failed their own write
sauron_ingest_entries_deadlettered_total # whole ENTRIES whose payload would not decode
```

`accepted - persisted`, **summed across every replica**, is the loss.

> **Do NOT subtract `deadlettered`.** An earlier version of this section gave the
> formula as `accepted - persisted - deadlettered`, which treats dead-lettering as
> a durable outcome. It is not one. `sauron:ingest:dlq` has no reader, no reaper,
> no `MAXLEN` and no TTL, and the worker acks the stream entry after
> dead-lettering — so a dead-lettered item is **destroyed**, and the SDK was
> already told `202`. Measured 2026-08-08 by driving the real worker with Postgres
> unreachable: 3 items accepted, 0 persisted, 3 dead-lettered, entry acked. The old
> formula evaluated to `3 - 0 - 3 = 0` and reported no loss for three permanently
> destroyed events. Treat `deadlettered` as a **named subset of the loss** — it
> tells you *why* those items are missing, not that they are safe.

Read it as a rate, and mind four things:

* **Sum before subtracting.** The Redis consumer group is shared, so one
  replica's edge is drained by another replica's workers. A per-host difference
  is meaningless.
* **Small negative excursions are redeliveries**, not a bug — a reclaimed
  unacked entry is written twice. Only a persistently growing positive gap is
  loss.
* **`persisted` is not rows.** `identify()` and breadcrumb items legitimately
  write no event row. Measured on a clean 3,050-item run with no loss at all:
  2,250 event rows for 3,050 persisted items, so a rows-based check would have
  reported 26% loss that did not happen.
* **The `sauron_ingest_stream_*` gauges are in ENTRIES, not items** (one entry is
  a whole envelope), and `sauron_ingest_stream_unread_trimmed` is a *live* gauge:
  Redis folds the trimmed gap into `entries-read` as the group catches up, so it
  falls back to 0 after recovery. The durable record is the item counters.

The endpoint is served on its own loopback listener, `INGEST_METRICS_ADDR`,
defaulting to `127.0.0.1:<INGEST_PORT + 10000>` — **not** on `:8081`, which
section 8 tells you to open to the internet for SDKs. Set
`INGEST_METRICS_ADDR=off` to disable it, or a different `host:port` to move it. A
metrics listener that cannot bind logs a warning and ingest carries on without
it, so check for `metrics listening` in `journalctl -u sauron-ingest` before
concluding the endpoint is broken. `INGEST_METRICS_SAMPLE_SECS` (default 15) is
how often the Redis-side gauges are refreshed; `0` serves the counters alone.

## 10. Troubleshooting

| Symptom | Check |
|---|---|
| Service fails immediately | `journalctl -u sauron-<svc> -e` — usually `DATABASE_URL` wrong/unreachable |
| `A dependency job for sauron-<svc>.service failed` | The migration failed. `journalctl -u sauron-migrate -e`. Postgres unreachable within `MIGRATE_WAIT_SECS` (default 120) or a migration errored. Fix the DB, then start the daemons by hand — a failed *start job* is never auto-retried (section 5) |
| `systemctl is-active sauron-migrate` says `inactive` | **Normal.** `Type=oneshot` with no `RemainAfterExit`; it is dead between runs. Use `systemctl is-failed sauron-migrate` instead |
| Metric alert rules never fire / no notification emails | `sauron-alerts` not enabled. `systemctl enable --now sauron-alerts` (section 6). Only monitor up/down alerts work without it, and nothing in the UI flags the gap |
| `DATABASE_URL is required` | `/etc/sauron/sauron.env` not set or unreadable by the `sauron` user |
| API 401 / login broken | `secret.env` missing or changed since sessions issued — rotate & restart |
| Dashboard shows wrong API URL | edit `/etc/sauron/dashboard.env`, re-run `sauron-dashboard-config`, reload nginx, hard-refresh |
| Ingest 429 | raise `INGEST_RATE_LIMIT_PER_MIN` in `/etc/sauron/ingest.env`, restart |
| Redis backlog grows / ingest drains far slower than expected | check for a stale `WORKER_CONCURRENCY=4` left in `/etc/sauron/ingest.env` by an upgrade (`%config(noreplace)`); the tuned default is 8. Nothing logs the effective worker count, so the journal will not tell you — read the file. See section 11 |
| Tier can't write cold | confirm `/var/lib/sauron/cold` is owned by `sauron` (see `tmpfiles`) |

## 11. Upgrading

**Migrations now run automatically on upgrade.** Every `sauron-*` daemon unit
carries `Requires=sauron-migrate.service` + `After=sauron-migrate.service`, and
the RPM transaction restarts the daemons, so each restart pulls the migrator and
waits for it. A daemon whose migration fails does **not** start — that is the
point: an old schema under a new binary produces scattered 500s or a feature that
silently does nothing, which is far worse than a unit that is honestly down.

Older releases of this document told you to run the migrator by hand after every
upgrade, because `sauron-migrate.service` has no `[Install]` section and `%post`
never starts it. That is still true of the unit, but it no longer matters: the
`Requires=` is what pulls it in.

Confirm after `dnf upgrade`:

```bash
systemctl is-active sauron-migrate                  # "inactive" is CORRECT — see below
systemctl --no-pager status 'sauron-*'
journalctl -u sauron-migrate --no-pager | tail
```

`sauron-migrate` is `Type=oneshot` with no `RemainAfterExit`, so it is `inactive
(dead)` between runs even on a healthy host. `failed` is the state that matters:
if `systemctl is-failed sauron-migrate` says `failed`, no daemon will start until
the database is fixed.

### Manual fallback

Still valid, and what to use when the daemons are down and you want the migration
output on its own:

```bash
# Stop everything that touches the schema, migrate, then bring it all back.
sudo systemctl stop sauron-api sauron-ingest sauron-tier sauron-alerts sauron-monitor sauron-inspector
sudo systemctl start sauron-migrate
journalctl -u sauron-migrate --no-pager | tail
sudo systemctl start sauron-api sauron-ingest sauron-tier sauron-alerts sauron-monitor
sudo systemctl start sauron-inspector      # only if you enabled it (section 6)
```

The stop list is **all six**, not just `sauron-api` and `sauron-ingest`. Every one
of them holds its own pool and issues its own queries against the same schema, so
any left running is a service reading a table mid-change:

- `sauron-tier` runs DDL of its own (partition drops/creates) against the very
  tables a migration may be altering.
- `sauron-alerts` evaluates metric rules and drains `notification_queue`; a tick
  failure is logged-and-swallowed by design, so breakage here is invisible.
- `sauron-monitor` writes check rows continuously.
- `sauron-inspector` reads the same partitions the ingest path writes — stop it
  too if you turned it on; the command is harmless if the unit is not enabled.

`systemctl stop sauron-migrate` is safe and is a no-op: because the oneshot is
inactive between runs, the `Requires=` from the daemons has nothing to propagate a
stop to. This only holds while the unit has **no** `RemainAfterExit=yes` — adding
it would make that one command stop all six daemons.

Then diff the shipped config against yours. `/etc/sauron/*.env` are
`%config(noreplace)`, so a release that adds new settings leaves them in
`api.env.rpmnew` and your actual file never sees them:

```bash
ls /etc/sauron/*.rpmnew 2>/dev/null && diff -u /etc/sauron/api.env /etc/sauron/api.env.rpmnew
```

### What breaks if a migration is skipped

| Migration | Skipping it means |
|---|---|
| `2026-08-01-000034_mail_outbox` | `sauron-api` queries a `mail_outbox` relation that does not exist. Password reset silently does nothing, because the enqueue error is swallowed behind a fixed 200. The drain logs one ERROR naming this section and then stays quiet. |
| `2026-08-01-000035_auth_sessions` | **Total authentication outage.** Without `auth_sessions` and `refresh_tokens.session_id`, `start_or_continue_session` fails on *every* login, register, refresh and password change — not a degraded feature, and on the exact path an operator would use to diagnose it. |
| `2026-08-01-000036_password_reset` | **Nobody can sign in.** This migration adds `users.credentials_invalidated_at`, and the API selects an explicit column list for the whole user row — so an upgraded binary against an unmigrated database fails `login`, `refresh` and `/v1/me` with a missing-column error. This is a deployment-wide authentication outage, not "the three password-reset routes return 500". |
| `2026-08-01-000037_notification_subscriptions` | `sauron-alerts` fails its subscription pass every tick. Tick failures are logged-and-swallowed by design, so it does this **quietly, forever**: no personal notification is ever evaluated, enqueued or delivered, and nothing in the dashboard indicates a problem. `POST /v1/me/notification-subscriptions` also 500s. |
| `2026-08-01-000038_event_users_identified` | `GET /v1/projects/{id}/active-users` returns `503 schema_migration_required`, and `sauron-ingest` records no identification at all. **Not recoverable later** — the backfill only sees stored traits and alias rows, so everyone first active during the gap is filed as a guest forever. Needs a maintenance window: the partial index blocks `event_users` writes while it builds, and that table holds roughly one row per page load per browser app. |
| `2026-08-01-000039_analytics_active_user_index` | Nothing breaks; the active-users query falls back to a full partition scan — measured at 3.8x the wall time and 23.6x the shared buffers of the index-only plan this migration enables (numbers and method in the migration's own header). **Stop `sauron-ingest` or drain the Redis stream before running.** It drops and rebuilds an index on a partitioned parent inside one transaction, blocking every `analytics_events` INSERT; the stream is trimmed with `XADD MAXLEN ~1000000` regardless of pending deliveries, so a long enough window silently discards undelivered events. Watch `sauron_ingest_items_accepted_total` minus `sauron_ingest_items_persisted_total` on `/metrics` (section 9) across the window — that gap is the discarded count, and nothing else reports it: measured on an isolated instance, 176,026 of 239,872 accepted items were dropped by a deliberately small trim with **zero WARN or ERROR lines** from the ingest. |
| `2026-08-01-000040_error_active_user_index` | Same as 000039, for `error_events`. **Run it in a separate window from 000039** — together they block both ingest write paths at once. |
| `2026-08-01-000041_pii_perms` | Custom roles holding `org:manage` never receive `pii:read`/`pii:manage`; the Owner and Admin presets keep working, so it looks like a role bug rather than a missed migration. |
| `2026-08-01-000042_inspector_scan` | Every `/v1/inspector/*` route 500s. |
| `2026-08-01-000043_inspector_mask_audit` | Worse: the ingest pipeline's `masked_keys_for_app` query fails on **every cache miss**, so forward masking is off deployment-wide with only a rate-limited log line. The enforcer fails stale rather than open, so ingest keeps flowing — which is exactly why nobody notices. |
| `2026-08-08-000044_tier_policy_and_pins` | **All tiering stops.** `runtime_settings` is read by the first `?` in `sauron-tier`'s `cycle()`, so the cycle aborts before any export or any drop — one WARN per tick, hourly by default (`TIER_TICK_SECS`), and no error anywhere else. Cold Parquet stops being written and hot partitions stop being dropped, so the disk grows until it fills. The Storage page's policy card 500s. Operationally free to apply: two new tables (`runtime_settings`, `tier_pins`), no lock on any hot table, nothing seeded — a fresh install behaves exactly as before, since absence of a row means "use the `TIER_HOT_DAYS` env value". |
| `2026-08-09-000045_cold_restore` | Cold-data restore from the dashboard is dead: the restore executor errors on every poll and the restore endpoints 500. Nothing else degrades. **Requires 000044** (`restore_jobs.pin_id` references `tier_pins`). Cheap to apply: `ADD COLUMN restored_pin_id UUID` with no `DEFAULT` on `error_events`, `analytics_events` and `transactions` is catalog-only on a partitioned parent — no rewrite, no table scan, no index build — plus one new `restore_jobs` table. Safe to run with ingest live. |
| `2026-08-09-000046_channel_config_enc` | **SECURITY FIX — apply it.** Before this migration a notification channel's `config` sat in **cleartext** in Postgres, in every base backup and in every WAL archive. For the generic webhook kind that blob holds the target URL *and* an arbitrary `headers` map, so a developer's `Authorization: Bearer …` was on disk in the clear; for Slack/Discord the `webhook_url` in `config` **is** the credential. This adds `notification_channels.config_enc`, AES-256-GCM under `NOTIFY_SECRET_KEY`, the same cipher already protecting `secret_enc`. Skipping it: every notification-channel read **and** write 500s and no alert of any kind is delivered — alerting is dead, not degraded. Rotating the exposed webhook URLs and headers after upgrading is the honest follow-up; the migration hides the plaintext, it cannot un-leak it. The row conversion is **not** done by `sauron-migrate` (that binary has neither the cipher nor the key) — it runs in Rust at the first `sauron-api` boot, is idempotent, and aborts startup rather than half-converting. See the downgrade warning below. |

| `2026-08-10-000049_store_metrics` | App-store install metrics are dead, but visibly so: `sauron-storesync` logs a missing-relation error every tick, and the "App stores" card in App settings renders its error inline instead of the form (the rest of the page is unaffected — the card catches its own load failure). The Overview store section simply never appears, because the designation it keys on lives in the column this migration adds. Purely additive — two new tables and one nullable column on `apps` — so it is safe to apply at any time with the daemons running. |

### DOWNGRADE WARNING — this release is one-way for notification channels

**Do not `dnf downgrade` past this release once `sauron-api` has booted on it.**

The first `sauron-api` boot converts every notification channel: it writes the
AES-256-GCM ciphertext to `config_enc` **and sets `config = '{}'` in the same
update**. The read rule is "`config_enc` when non-NULL, else `config`", so a
downgraded binary — which knows nothing about `config_enc` — reads the empty
legacy column and finds no SMTP host, no Matrix room, no webhook URL and no
headers. Deliveries do not error: they go **nowhere**, silently, for every
channel. Nothing in the dashboard says so.

`dnf downgrade` does **not** run `down.sql` — no scriptlet ever invokes a revert —
so the column and the ciphertext survive the downgrade intact. Recovery is
therefore to **roll forward**:

```bash
sudo dnf upgrade sauron-server            # back to the release that has config_enc
sudo systemctl restart sauron-api
```

> **Never run `diesel migration revert` on `2026-08-09-000046_channel_config_enc`.
> It is UNRECOVERABLE.** Its own `down.sql` says so in the first line: the revert
> drops `config_enc`, and on any converted row that ciphertext is the *only* copy
> of the destination — `config` is already `'{}'`. The migration cannot decrypt it
> back, because `NOTIFY_SECRET_KEY` lives outside the database. After a revert
> every notification channel must be re-created by hand. If the configurations
> matter, decrypt and write `config` back with a build that still has the cipher
> **before** reverting.

After upgrading to the release that ships the PII inspector, **remove the
`TIER_HOT_DAYS=` line from `/etc/sauron/tier.env` by hand.** That file is
`%config(noreplace)`, so if you ever edited it rpm keeps your version and
ships the new one as `.rpmnew`. Your stale line then wins for `sauron-tier`
alone, while `sauron-inspector` and `sauron-api` use the shared declaration in
`sauron.env` — and that divergence means the masker rewrites rows in a
partition the tier worker has already exported to Parquet. Do not set
`INSPECTOR_ENABLED=1` before you have done this.

**Remove the `WORKER_CONCURRENCY=` line from `/etc/sauron/ingest.env` by hand**
(this one applies to the release that retuned the ingest defaults, independently
of the inspector). The shipped file no longer declares it, so the binary's tuned
default of 8 applies — but the same `%config(noreplace)` rule bites: if you ever
edited that file, rpm keeps your version, stale `WORKER_CONCURRENCY=4` included,
and ships the new one as `ingest.env.rpmnew`. (A file you never touched *is*
replaced by the new one, so untouched hosts get the fix for free.) The cost of missing
this is not an error anywhere — the ingest simply drains at the old rate, which
measured **12,910 items/s where the tuned default measured 18,987** on the same
hardware. Nothing logs the effective worker count or pool size, so it looks like
a slow disk rather than a config line. If you *deliberately* set a value, keep
`INGEST_DB_POOL` ≥ it, or the surplus workers just queue on connection checkout.
The worker/batch-size interaction is real — raising the worker count alone
measured *slower* at the old batch size — so see the tuning note in the README's
Ingest gateway section before picking a number.

A meaningful `confirm_source` in the mask audit trail requires
`API_TRUST_FORWARDED_HEADERS=true` behind a proxy that **overwrites**
`X-Forwarded-For`. With the shipped nginx and the default `false`, every audit
row records the proxy's address.

**Postgres `max_connections` must be at least 150 before you enable the
inspector.** `sauron-inspector` opens one 4-connection pool, taking peak
pooled demand from 94 (`sauron-api` 16 + `sauron-ingest` 8 + `sauron-alerts`
8 + `sauron-tier` 4 + `sauron-monitor` 50 + 8) to 98 — against a stock
`max_connections` of 100 with 3 reserved for superusers. Exhaustion does not
surface as an inspector error: it surfaces as `sauron-api` 500s and
`sauron-ingest` accepting a 202 and then dropping the event — which is visible
as a growing `sauron_ingest_items_accepted_total` minus
`sauron_ingest_items_persisted_total` on `/metrics` (section 9). Check with
`sudo -u postgres psql -c 'SHOW max_connections'` and raise it in
`postgresql.conf` (the compose stack does this with
`command: postgres -c max_connections=200`; an RPM host has no such
override and defaults to 100).

**000035 needs a maintenance window.** It holds `AccessExclusiveLock` on
`refresh_tokens` across an `ADD COLUMN`, a full-table backfill and an index build
that scans the whole heap, in one transaction, on the table that authenticates
every request. `CONCURRENTLY` is unavailable inside a migration transaction, so
it cannot be softened. Measured runtime: **under 20 ms** on the reference dataset
(133 `refresh_tokens` rows, 160 kB) and **≈0.4 s** on a 1.7M-row / 473 MB copy of
it — the size the migration's own header predicts for a deployment live for a
year with 50 active sessions. Budget by table size, not by that first number:
`refresh_tokens` is never reaped, so it only ever grows.

### One-off behaviour change in this release

**Environment-filtered alert rules start firing again.** Since migration 33,
`alert_count_errors` and `alert_count_events` resolved an environment *name*
against the project-level `environments` catalogue, whose ids can never equal
the `app_environments` enrollment id an event row carries — so every
environment-narrowed alert rule had been counting zero and had never fired. This
release fixes the resolution, and those rules will fire for the first time on
the first tick after deploy. Each rule's own `throttle_seconds` bounds it to one
message per throttle period, but an operator with many environment-filtered
rules should expect a burst and may want to disable them for one tick. Rules
naming a *misspelled* environment resolve to an empty set and keep counting
zero, exactly as before — now deliberately.
