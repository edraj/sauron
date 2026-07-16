# Sauron — Installing from RPM (Fedora / RHEL)

Sauron ships four RPMs from one spec:

| Package | Contents |
|---|---|
| `sauron` | shared `sauron` user, `/var/lib/sauron`, `/etc/sauron/sauron.env` (pulled in automatically) |
| `sauron-server` | API, ingest, monitor, tier, migrate binaries + systemd units |
| `sauron-dashboard` | static web UI + nginx vhost (requires `nginx`) |
| `sauron-cli` | `crebain` load generator, `sauron-symcli` |

## 1. Build the RPMs

Requires the build toolchain (`sudo dnf install rust cargo gcc gcc-c++ cmake clang perl-interpreter nodejs npm rpm-build systemd-rpm-macros`):

```bash
git clone <repo> sauron && cd sauron
./packaging/rpm/build-rpm.sh
```

Artifacts land in `~/rpmbuild/RPMS/<arch>/` and `~/rpmbuild/SRPMS/`. The first build
compiles the Rust workspace (including a bundled DuckDB) and the dashboard — expect
several minutes. Use `./packaging/rpm/build-rpm.sh --srpm` to produce just the source RPM.

> **Using rustup / nvm** instead of the Fedora `rust`/`cargo`/`nodejs`/`npm` packages?
> `rpmbuild` resolves `BuildRequires` against the RPM database, not `$PATH`, so it reports
> `cargo >= 1.82 is needed` even though `cargo` works in your shell. `build-rpm.sh`
> auto-detects this (cargo on `$PATH` but no `cargo` RPM) and adds `--nodeps` for you — your
> toolchain still does the build. Force it with `./packaging/rpm/build-rpm.sh --nodeps`, or
> install the distro toolchain to satisfy the check natively.

## 2. Install

Pick what a given host needs. On an all-in-one box:

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

## What gets installed

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

Next: **[SETUP.md](SETUP.md)** to configure and start the stack.

## Upgrade / uninstall

```bash
sudo dnf upgrade ./sauron-*-0.1.1-*.rpm     # config files preserved
sudo systemctl stop 'sauron-*'
sudo dnf remove sauron-server sauron-dashboard sauron-cli sauron
```

Removal leaves `/var/lib/sauron` and the `sauron` user in place (standard practice); delete them manually if you want a clean slate.
