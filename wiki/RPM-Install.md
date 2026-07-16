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
RPM**, all named `<name>-0.1.0-1.fc44.<arch>`. Binary RPMs land in `~/rpmbuild/RPMS/<arch>/`
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
sudo dnf install ./sauron-0.1.0-*.rpm ./sauron-server-0.1.0-*.rpm \
                 ./sauron-dashboard-0.1.0-*.rpm ./sauron-cli-0.1.0-*.rpm
```

Backend-only host:

```bash
sudo dnf install ./sauron-0.1.0-*.rpm ./sauron-server-0.1.0-*.rpm
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

All `/etc/sauron/*.env` are `%config(noreplace)` — upgrades never overwrite your edits.

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
| Tier can't write cold | confirm `/var/lib/sauron/cold` is owned by `sauron` (see `tmpfiles`) |

## Upgrade / uninstall

```bash
sudo dnf upgrade ./sauron-*-0.1.1-*.rpm     # config files preserved
sudo systemctl stop 'sauron-*'
sudo dnf remove sauron-server sauron-dashboard sauron-cli sauron
```

Removal leaves `/var/lib/sauron` and the `sauron` user in place (standard practice); delete them manually if you want a clean slate.
