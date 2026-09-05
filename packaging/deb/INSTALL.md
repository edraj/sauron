# Sauron — Installing from .deb (Debian / Ubuntu)

> Installing for the first time? Start at the top-level **[INSTALL.md](../../INSTALL.md)** — it covers Docker, Fedora/RHEL and Debian/Ubuntu side by side, plus the environment variables that matter and where
> each one lives. This document is the Debian / Ubuntu detail underneath it.

Sauron ships four `.deb` packages from one source, mirroring the RPM split:

| Package | Contents |
|---|---|
| `sauron` | shared `sauron` user, `/var/lib/sauron`, `/etc/sauron/sauron.env` (pulled in automatically) |
| `sauron-server` | API, ingest, monitor, alerts, tier, inspector, storesync, migrate binaries + systemd units |
| `sauron-dashboard` | static web UI + nginx vhost (requires `nginx`) |
| `sauron-cli` | `crebain` load generator, `sauron-symcli` |

**Configuration and operations are documented in
[`SETUP.md`](../rpm/SETUP.md)**, which is shipped in `sauron-server` at
`/usr/share/doc/sauron-server/SETUP.md`. Every operator-facing path is identical between the
`.deb` and the `.rpm`, so that document applies unchanged — read it before starting the services.

## Supported targets

| Target | glibc | Package version |
|---|---|---|
| Debian 12 (bookworm) | 2.36 | `<version>-1~deb12` |
| Ubuntu 22.04 (jammy) | 2.35 | `<version>-1~ubuntu22.04` |

Two builds, not one, because neither glibc covers the other. The Debian 12 package will
**not** start on Ubuntu 22.04. Newer releases in each family are covered by their own line's
build (the Debian 12 package runs on Debian 13; the Ubuntu 22.04 package runs on 24.04).

## 1. Build the packages

Requires `debhelper`, `dpkg-dev` and a Rust/Node toolchain. **The distro `rustc` and `nodejs`
packages are too old on both targets** — Debian 12 has rustc 1.63 and Node 18, Ubuntu 22.04 has
rustc 1.75 and Node 12, against floors of Rust 1.82 and Node 22 — so use rustup and NodeSource,
as CI does:

```bash
sudo apt-get install -y debhelper dpkg-dev gcc libc6-dev perl pkg-config libzstd-dev curl unzip git

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o rustup-init.sh
sh rustup-init.sh -y --profile minimal --default-toolchain stable
. "$HOME/.cargo/env"

curl -fsSL https://deb.nodesource.com/setup_22.x -o nodesource_setup.sh
sudo bash nodesource_setup.sh && sudo apt-get install -y nodejs
```

```bash
git clone <repo> sauron && cd sauron
./packaging/deb/build-deb.sh
```

Artifacts land in `build/deb/` (override with `DEB_BUILD_TOPDIR`).

`debian/rules` never compiles anything — every `dh_auto_*` target is a no-op. `build-deb.sh`
owns compilation, stages the result, and only then calls `dpkg-buildpackage`. This is why the
packaging step needs no toolchain at all in `--prebuilt` mode.

DuckDB is **not** compiled from source: `build-deb.sh` fetches a prebuilt `libduckdb` (version
derived from the `libduckdb-sys` pin in `Cargo.lock`), links `sauron-tier` against it, and ships
the `.so` inside `sauron-server` at `/usr/lib/sauron/libduckdb.so` with an `ld.so.conf.d`
drop-in. First build needs outbound network; the download is cached under `.cache/duckdb/`.

Other modes:

- `./packaging/deb/build-deb.sh --prebuilt DIR` — package precompiled binaries + dashboard
  assets from `DIR` (`DIR/bin/*`, `DIR/dist/`, `DIR/libduckdb.so`) without recompiling. CI uses
  this to split the slow compile from packaging — see `.github/workflows/release.yml`.
- `./packaging/deb/build-deb.sh --distro deb12` — override the version suffix, which is
  otherwise derived from the building host's `/etc/os-release`.

## 2. Install

`apt-get` rather than `dpkg -i`, so the dependencies (`nginx`, and the `systemd`
alternatives that provide `systemd-sysusers`) are resolved. On an all-in-one box:

```bash
sudo apt-get install ./sauron_*.deb ./sauron-server_*.deb ./sauron-dashboard_*.deb ./sauron-cli_*.deb
```

Backend-only host:

```bash
sudo apt-get install ./sauron_*.deb ./sauron-server_*.deb
```

## 3. Expect failed units on a fresh install — this is normal

**The .deb follows Debian policy and starts the services on install. The RPM does not.**

On a fresh host `/etc/sauron/*.env` has not been configured yet, so there is no database to
connect to and every daemon will fail to start:

```
● sauron-api.service - Sauron dashboard API
     Active: failed (Result: exit-code)
```

This is expected and the install itself succeeds — `dpkg` configures the packages cleanly,
because `deb-systemd-invoke` treats a failed start as non-fatal. Work through
[`SETUP.md`](../rpm/SETUP.md) (Postgres, Redis, `/etc/sauron/sauron.env`, migrations), then:

```bash
sudo systemctl start sauron-api sauron-ingest sauron-monitor sauron-alerts sauron-tier sauron-storesync
```

Six daemons are **enabled** on install. `sauron-inspector` is deliberately **not**: the PII
inspector is opt-in because its extra 4-connection pool pushes a stock Postgres past
`max_connections`, and the resulting failures surface as `sauron-api` 500s rather than as
inspector errors. To turn it on, set `INSPECTOR_ENABLED=1` in `/etc/sauron/inspector.env`, then
`sudo systemctl enable --now sauron-inspector`.

`sauron-migrate.service` is static and has no enabled/disabled state. It is pulled in by every
daemon's `Requires=`, so migrations run whenever a daemon starts.

## 4. Secrets

`sauron-server`'s `postinst` generates `/etc/sauron/secret.env` (`JWT_SECRET` and
`NOTIFY_SECRET_KEY`) on first install, `0640 root:sauron`. It is not a `dpkg` conffile, and it is
removed on `purge` but kept on `remove`.

On upgrade from a build that predates the fail-closed notification key, the `postinst`
backfills `NOTIFY_SECRET_KEY` **from the existing `JWT_SECRET`** — never a fresh random value,
because older builds derived the channel cipher from `JWT_SECRET` and a new key would silently
make every stored channel secret undecryptable. If `JWT_SECRET` is absent (you moved it into
`api.env`), nothing is written and a warning is printed; see `SETUP.md` section 6.

## What gets installed

Identical to the RPM except for two loader-internal paths.

| Path | Package |
|---|---|
| `/usr/bin/sauron-{api,ingest,monitor,alerts,tier,inspector,storesync,migrate}` | `sauron-server` |
| `/usr/bin/{crebain,sauron-symcli}` | `sauron-cli` |
| `/lib/systemd/system/sauron-*.service` | `sauron-server` |
| `/etc/sauron/*.env` (`0640 root:sauron`) | base / server / dashboard |
| `/usr/share/sauron/dashboard/` | `sauron-dashboard` |
| `/usr/libexec/sauron/sauron-dashboard-config` | `sauron-dashboard` |
| `/etc/nginx/conf.d/sauron-dashboard.conf` | `sauron-dashboard` |
| `/var/lib/sauron`, `/var/lib/sauron/cold` | `sauron` |
| `/usr/lib/sauron/libduckdb.so` (RPM: `/usr/lib64/sauron/`) | `sauron-server` |

The `.env` files are `dpkg` conffiles, so local edits survive upgrades and a changed default
prompts. Their `root:sauron 0640` ownership is applied by the `postinst`, not declared in the
package: `dpkg` unpacks before any maintainer script runs, so the `sauron` group does not exist
yet at unpack time.

## Upgrade / uninstall

```bash
sudo apt-get install ./sauron*_<newversion>_*.deb   # upgrade; daemons are restarted
sudo apt-get remove sauron-server                    # stop + remove, keep config and data
sudo apt-get purge sauron-server sauron-dashboard sauron-cli sauron   # also remove /etc/sauron and secrets
```

`purge` removes the conffiles, `secret.env` and the generated `config.js`. It leaves the
`sauron` system account and any non-empty `/var/lib/sauron` (cold-tier Parquet lives there)
alone.
