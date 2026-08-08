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

## 4. JWT secret

`/etc/sauron/secret.env` is generated with a random `JWT_SECRET` on first install.
Verify it exists:

```bash
sudo test -s /etc/sauron/secret.env && echo "JWT secret present"
```

To rotate it (invalidates existing sessions):

```bash
sudo sh -c 'umask 077; printf "JWT_SECRET=%s\n" "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d " \n")" > /etc/sauron/secret.env'
sudo chgrp sauron /etc/sauron/secret.env && sudo chmod 0640 /etc/sauron/secret.env
sudo systemctl restart sauron-api
```

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

```bash
sudo systemctl start sauron-migrate
journalctl -u sauron-migrate --no-pager | tail
```

Expected: `migrations up to date`. Re-runnable safely (idempotent).

## 6. Enable and start the services

```bash
sudo systemctl enable --now sauron-api sauron-ingest sauron-monitor sauron-tier
systemctl --no-pager status 'sauron-*'
```

- `sauron-api` → `:8080` (dashboard API)
- `sauron-ingest` → `:8081` (SDK ingest)
- `sauron-monitor`, `sauron-tier` → no listener

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
| `DATABASE_URL is required` | `/etc/sauron/sauron.env` not set or unreadable by the `sauron` user |
| API 401 / login broken | `secret.env` missing or changed since sessions issued — rotate & restart |
| Dashboard shows wrong API URL | edit `/etc/sauron/dashboard.env`, re-run `sauron-dashboard-config`, reload nginx, hard-refresh |
| Ingest 429 | raise `INGEST_RATE_LIMIT_PER_MIN` in `/etc/sauron/ingest.env`, restart |
| Redis backlog grows / ingest drains far slower than expected | check for a stale `WORKER_CONCURRENCY=4` left in `/etc/sauron/ingest.env` by an upgrade (`%config(noreplace)`); the tuned default is 8. Nothing logs the effective worker count, so the journal will not tell you — read the file. See section 11 |
| Tier can't write cold | confirm `/var/lib/sauron/cold` is owned by `sauron` (see `tmpfiles`) |

## 11. Upgrading

**Run the migrator by hand after every upgrade.** `dnf upgrade` does not do it:
`sauron-migrate.service` has no `[Install]` section and `%post` never starts it,
so a new binary meets whatever schema was there before. The symptom is not a
crash — it is scattered 500s, or a feature that silently does nothing.

```bash
sudo systemctl stop sauron-api sauron-ingest
sudo systemctl start sauron-migrate
sudo systemctl start sauron-api sauron-ingest
```

Stop first: `sauron-api` and `sauron-ingest` must not be serving against a schema
that is halfway through changing.

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
