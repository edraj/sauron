# Install via RPM (Fedora / RHEL)

Sauron can be deployed without Docker on Fedora/RHEL-family systems using native
RPMs and systemd. This page is the full, self-contained runbook: build → install →
provision → configure → start. The same instructions ship inside the `sauron-server`
package under `/usr/share/doc/sauron-server/` ([INSTALL.md](https://github.com/splimter/sauron/blob/main/packaging/rpm/INSTALL.md)
· [SETUP.md](https://github.com/splimter/sauron/blob/main/packaging/rpm/SETUP.md)).

> **Prerequisite:** the RPMs require an external **PostgreSQL** and **Redis/Valkey** — the
> packages do not install or configure them (they are declared only as weak `Recommends`).
> Provision them on this host or a remote one (steps 3–4 below) before starting the services.

## The artifacts

One `rpmbuild` run (step 1) emits **four binary RPMs** — one per component — plus a **source
RPM**, all named `<name>-1.0.0-1.fc44.<arch>`. Binary RPMs land in `~/rpmbuild/RPMS/<arch>/`
and the source RPM in `~/rpmbuild/SRPMS/`. Install only the binary RPMs a given host needs;
`dnf` pulls in the shared `sauron` base package automatically.

| Artifact | ~Size | What it is |
|---|---|---|
| `sauron-*.rpm` | ~25 KB | **Base** — the shared `sauron` system user, `/var/lib/sauron` data dir, and `/etc/sauron/sauron.env`. Auto-pulled as a dependency of server & dashboard. |
| `sauron-server-*.rpm` | ~31 MB | **Backend** — the `sauron-api` (:8080), `sauron-ingest` (:8081), `sauron-monitor`, `sauron-tier`, and `sauron-migrate` binaries + their systemd units. Large because DuckDB is compiled in statically (no external lib). |
| `sauron-dashboard-*.rpm` | ~130 KB | **Web UI** — the built Svelte SPA under `/usr/share/sauron/dashboard`, an nginx vhost, and the runtime-config generator. Requires `nginx`. |
| `sauron-cli-*.rpm` | ~2.6 MB | **Tools** — the `crebain` load/benchmark generator and the `sauron-symcli` symbolication utility. Standalone, no dependencies. |
| `sauron-*.src.rpm` | ~390 KB | **Source RPM** — bundles the spec + sources; rebuild on any Fedora/RHEL host with `rpmbuild --rebuild sauron-*.src.rpm`. |

Runtime footprint is lean: the binaries link only glibc/libstdc++ — **no libpq, OpenSSL, or
DuckDB shared libraries** (Postgres uses the pure-Rust diesel query builder, TLS is rustls,
DuckDB is static). The only external package dependency is `nginx`, for the dashboard.

## 1. Build the RPMs

Install the build toolchain, then run the helper:

```bash
sudo dnf install rust cargo gcc gcc-c++ cmake clang perl-interpreter nodejs npm rpm-build systemd-rpm-macros
git clone <repo> sauron && cd sauron
./packaging/rpm/build-rpm.sh
```

Artifacts land in `~/rpmbuild/RPMS/<arch>/` and `~/rpmbuild/SRPMS/`. The first build
compiles the Rust workspace (including a bundled DuckDB) and the dashboard — expect
several minutes. Use `./packaging/rpm/build-rpm.sh --srpm` to produce just the source RPM.

> **Using rustup / nvm** instead of the Fedora `rust`/`cargo`/`nodejs`/`npm` packages?
> `rpmbuild` resolves `BuildRequires` against the RPM database, not `$PATH`, so it reports
> `cargo >= 1.82 is needed` even though `cargo` works in your shell. `build-rpm.sh`
> auto-detects this and adds `--nodeps` for you (your toolchain still does the build); force
> it with `./packaging/rpm/build-rpm.sh --nodeps`, or install the distro toolchain to satisfy
> the check natively.

## 2. Install

All-in-one box:

```bash
cd ~/rpmbuild/RPMS/$(uname -m)
sudo dnf install ./sauron-1.0.0-*.rpm ./sauron-server-1.0.0-*.rpm \
                 ./sauron-dashboard-1.0.0-*.rpm ./sauron-cli-1.0.0-*.rpm
```

Backend-only host:

```bash
sudo dnf install ./sauron-1.0.0-*.rpm ./sauron-server-1.0.0-*.rpm
```

`dnf` pulls the base `sauron` package automatically and (for the dashboard) `nginx`.

### What gets installed

```
/usr/bin/sauron-{api,ingest,monitor,tier,migrate,symcli}   /usr/bin/crebain
/usr/lib/systemd/system/sauron-{api,ingest,monitor,tier,migrate}.service
/etc/sauron/sauron.env          shared: DATABASE_URL, REDIS_URL, RUST_LOG
/etc/sauron/{api,ingest,monitor,tier,dashboard}.env
/etc/sauron/secret.env          JWT_SECRET, auto-generated on first install (0640 root:sauron)
/var/lib/sauron/  /var/lib/sauron/cold        owned by the sauron user
/usr/share/sauron/dashboard/    static SPA
/etc/nginx/conf.d/sauron-dashboard.conf
/usr/libexec/sauron/sauron-dashboard-config
```

All `/etc/sauron/*.env` are `%config(noreplace)`: a file **you have edited** is kept
as-is on upgrade and the new one lands beside it as `.rpmnew`. A file you never
touched is *replaced* by the packaged version. So an upgrade never overwrites your
edits — but it also never applies a changed default to a file you did edit, which
is a trap when a release retunes one (see
[Upgrade / uninstall](#upgrade--uninstall)). Diff any `.rpmnew` after upgrading.

## 3. Provision PostgreSQL

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

## 4. Provision Redis / Valkey

```bash
sudo dnf install -y valkey        # Fedora 41+ (use 'redis' on RHEL)
sudo systemctl enable --now valkey
```

## 5. Configure the shared connection

Edit `/etc/sauron/sauron.env`:

```sh
DATABASE_URL=postgres://sauron:change-me-strong@localhost:5432/sauron
REDIS_URL=redis://127.0.0.1:6379
RUST_LOG=info,sauron=info
```

Per-service tunables live in `/etc/sauron/{api,ingest,monitor,tier}.env` — the
defaults are fine to start.

## 6. JWT secret

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

## 7. Run database migrations

```bash
sudo systemctl start sauron-migrate
journalctl -u sauron-migrate --no-pager | tail
```

Expected: `migrations up to date`. Re-runnable safely (idempotent).

## 8. Enable and start the services

```bash
sudo systemctl enable --now sauron-api sauron-ingest sauron-monitor sauron-tier
systemctl --no-pager status 'sauron-*'
```

- `sauron-api` → `:8080` (dashboard API)
- `sauron-ingest` → `:8081` (SDK ingest)
- `sauron-monitor`, `sauron-tier` → no listener

## 9. Dashboard

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

## 10. Firewall

```bash
sudo firewall-cmd --add-service=http --permanent          # dashboard :80
sudo firewall-cmd --add-port=8080/tcp --permanent         # API (if reached directly)
sudo firewall-cmd --add-port=8081/tcp --permanent         # ingest (SDKs)
sudo firewall-cmd --reload
```

Prefer fronting `:8080`/`:8081` with nginx/TLS rather than exposing them directly.

## 11. Verify

```bash
curl -fsS http://localhost:8080/health && echo               # API up
curl -fsS http://localhost:8081/health && echo               # ingest up
curl -fsS http://localhost/config.js                          # dashboard runtime config
journalctl -u sauron-api -u sauron-ingest --no-pager | tail
```

(If a service exposes a different health path, check `journalctl` for the bound
address logged at startup.)

## Troubleshooting

| Symptom | Check |
|---|---|
| Service fails immediately | `journalctl -u sauron-<svc> -e` — usually `DATABASE_URL` wrong/unreachable |
| `DATABASE_URL is required` | `/etc/sauron/sauron.env` not set or unreadable by the `sauron` user |
| API 401 / login broken | `secret.env` missing or changed since sessions issued — rotate & restart |
| Dashboard shows wrong API URL | edit `/etc/sauron/dashboard.env`, re-run `sauron-dashboard-config`, reload nginx, hard-refresh |
| Ingest 429 | raise `INGEST_RATE_LIMIT_PER_MIN` in `/etc/sauron/ingest.env`, restart |
| Redis backlog grows / ingest drains far slower than expected | check for a stale `WORKER_CONCURRENCY=4` left in `/etc/sauron/ingest.env` by an upgrade; the tuned default is 8. Nothing logs the effective worker count, so read the file rather than the journal |
| Tier can't write cold | confirm `/var/lib/sauron/cold` is owned by `sauron` (see `tmpfiles`) |

## Upgrade / uninstall

```bash
sudo dnf upgrade ./sauron-*-0.1.1-*.rpm     # config files preserved
sudo systemctl stop 'sauron-*'
sudo dnf remove sauron-server sauron-dashboard sauron-cli sauron
```

Removal leaves `/var/lib/sauron` and the `sauron` user in place (standard practice); delete them manually if you want a clean slate.

**An RPM upgrade does not run migrations.** `sauron-migrate.service` has no
`[Install]` section and `%post` never starts it, so new binaries meet the old
schema — the symptom is scattered 500s or a feature that silently does nothing,
not a crash. Run it by hand after every upgrade (see
[step 7](#7-run-database-migrations)).

### Remove the stale `WORKER_CONCURRENCY` after upgrading

The shipped `ingest.env` no longer declares `WORKER_CONCURRENCY`, so the binary's
tuned default of **8** applies (it was pinned at 4, which outranked the default —
the changelog announced 8 while installs kept running 4). Because `ingest.env` is
`%config(noreplace)`, a host whose operator **ever edited that file** keeps it
verbatim with the stale `WORKER_CONCURRENCY=4` still winning, and receives the new
file as `ingest.env.rpmnew`. Delete the line by hand:

```bash
# Anchored on purpose: the new file mentions WORKER_CONCURRENCY in comments, so an
# unanchored grep matches 3 lines on a correctly-upgraded host and reads as a problem.
sudo grep -n '^WORKER_CONCURRENCY=' /etc/sauron/ingest.env   # no output = already fine
sudo sed -i '/^WORKER_CONCURRENCY=/d' /etc/sauron/ingest.env
sudo systemctl restart sauron-ingest
```

Skipping this raises no error anywhere. The ingest just keeps draining at the old
rate — measured **12,910 items/s against 18,987** for the tuned default on the
same hardware — and since nothing logs the effective worker count, it presents as
slow storage rather than a config line. If you deliberately set a value, keep
`INGEST_DB_POOL` ≥ it, and read the README's Ingest gateway tuning note first:
the worker count interacts with `INGEST_BATCH_SIZE`, and raising it alone measured
*slower* at the old batch size.

### Upgrading to per-app environments

This release moves the ingest key from the app to the environment — the migration
drops `apps.public_key`. It is a **breaking schema change**: run `sudo -u sauron
sauron-migrate` before starting the new binaries, and remember that an RPM upgrade
does **not** run it for you (see [step 7](#7-run-database-migrations) above and
the note in the Troubleshooting table).

This is a **stop-the-world cutover, not a rolling upgrade** — once migrated, any
still-running old binary 500s on every ingest request, because the column its auth
check reads no longer exists. There is also no separate `sauron-worker` unit here:
`sauron-ingest` runs the edge **and** the co-located worker pool in one process
(see [Architecture §1](Architecture.md#1-the-ingest-pipeline)), and the process has
no graceful-shutdown handling, so `systemctl stop sauron-ingest` kills in-flight
work rather than draining it. Pull the host from your load balancer or firewall
first, or every in-flight signal is lost.

This sequence **replaces** the generic [Upgrade / uninstall](#upgrade--uninstall)
recipe above for this release, and the ordering matters: that recipe runs `dnf
upgrade` before stopping anything, which is backwards here. Packaging's `%postun`
scriptlet (`%systemd_postun_with_restart`) restarts any of these units that is
still active the moment the new binaries land, so upgrading before the drain
check passes force-restarts `sauron-ingest` onto the new binary — mid-drain,
against an un-migrated database. Stop first, install second:

```bash
# 1. Stop routing new traffic to this host (remove it from the load balancer, or
#    block the ingest port at the firewall). Leave sauron-ingest itself running
#    so its co-located workers keep draining what is already queued. Both
#    `pending` (delivered but not yet acked) and `lag` (never delivered) must
#    read 0 for the `workers` group before you go further. XLEN is NOT a drain
#    check: XACK marks an entry processed but never removes it from the stream
#    (the only trim anywhere is the approximate `MAXLEN ~ 1_000_000` cap on
#    XADD), so XLEN sits near that cap permanently and would never reach zero.
redis-cli XINFO GROUPS sauron:ingest:stream   # repeat until pending=0 and lag=0

# 2. Only once drained, stop the two units whose wire format/schema changed:
sudo systemctl stop sauron-ingest sauron-api

# 3. Install the new RPMs. Both units are already stopped, so the package's
#    postun try-restart is a no-op instead of racing the migration below.
sudo dnf upgrade ./sauron-*-*.rpm

# 4. Migrate. RPM upgrades do NOT run this automatically.
sudo -u sauron sauron-migrate

# 5. Start everything together, then re-admit traffic at the LB/firewall.
sudo systemctl start sauron-api sauron-ingest sauron-monitor sauron-alerts sauron-tier
```

Two reasons the drain matters. `IngestJob` gained a required `environment_id`, so a
job serialized by the old binary cannot deserialize against the new one — the
worker dead-letters it to `sauron:ingest:dlq`, which **nothing reads and nothing
trims**. Those signals are gone from the product even though the SDK already
received `202 Accepted`. And `sauron-api` and `sauron-ingest` must move together:
the DSN cache key prefix changed to `sauron:dsn:v2:` in this same release, so an
old `api` invalidating a rotated key under the old prefix writes to a slot the new
`ingest` no longer reads, leaving a revoked key live for up to the cache's 300s TTL.

Every deployed SDK stops reporting until its DSN is replaced with an environment
DSN, found under **Settings → app → Environments** in the dashboard. Existing
environments are preserved and each is issued a key; the app's old key is gone and
cannot be recovered.
