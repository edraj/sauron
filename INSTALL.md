# Installing Sauron

Three supported ways to run Sauron. Pick one:

| | Best for | Postgres & Redis | Ports |
|---|---|---|---|
| **[Docker Compose](#a-docker-compose)** | evaluation, development, single-box deployments | started for you | 10000 / 10001 / 10002 |
| **[Fedora / RHEL (RPM)](#b-fedora--rhel-rpm)** | production on RHEL-family hosts, no Docker | **you provide them** | 8080 / 8081 / 80 |
| **[Debian / Ubuntu (.deb)](#c-debian--ubuntu-deb)** | production on Debian-family hosts, no Docker | **you provide them** | 8080 / 8081 / 80 |

Both native paths treat PostgreSQL and Redis/Valkey as **external services**. They are neither
installed nor configured for you, and Sauron will not start until they exist and
`DATABASE_URL` points at one. Only the Compose path brings its own.

Once installed, configuration is the same problem for all three — jump to
**[Essential configuration](#essential-configuration)**.

---

## A. Docker Compose

The fastest path, and the only one that needs nothing on the host but Docker.

```bash
cp .env.example .env
```

Edit `.env` and change **`JWT_SECRET`** and **`NOTIFY_SECRET_KEY`** — both ship as obvious
placeholders and neither is safe to leave. Then:

```bash
docker compose up --build
```

- Dashboard → <http://localhost:10002>
- API → <http://localhost:10000>
- Ingest → <http://localhost:10001>

Migrations run on their own: the `migrate` service is a one-shot, and `api` waits on it with
`condition: service_completed_successfully`, so the API cannot come up against an old schema.

> The 10000 range is deliberate. Inside their containers the services still bind `:8080`,
> `:8081` and `:80`; remapping on the host leaves 3000/8080/8081 free for a local dev stack to
> run alongside. `.env.example` ships matching `API_BASE_URL` / `INGEST_BASE_URL` /
> `CORS_ALLOWED_ORIGINS` — **if you change the `ports:` mappings, change those three too.**

> First build compiles the Rust workspace three times, once per service image. Later builds are
> cached.

---

## B. Fedora / RHEL (RPM)

Sauron ships four RPMs from one spec: `sauron` (shared user, `/var/lib/sauron`, shared config),
`sauron-server` (daemons + systemd units), `sauron-dashboard` (SPA + nginx vhost), `sauron-cli`
(`crebain`, `sauron-symcli`).

**1. Build**

```bash
sudo dnf install -y rust cargo gcc perl-interpreter libzstd-devel nodejs npm rpm-build systemd-rpm-macros curl unzip
./packaging/rpm/build-rpm.sh
```

Fedora's own `rust`/`cargo` are new enough (1.98 on Fedora 44, against a 1.82 floor). No C++
toolchain is needed — DuckDB is linked against a prebuilt `libduckdb` that `build-rpm.sh`
fetches, rather than compiled from its C++ amalgamation.

> Using **rustup or nvm** instead of the distro packages? `rpmbuild` resolves `BuildRequires`
> against the RPM *database*, not `$PATH`, so it reports `cargo >= 1.82 is needed` even though
> `cargo` works in your shell. `build-rpm.sh` detects this and adds `--nodeps` for you; your
> toolchain still does the build.

**2. Install**

```bash
sudo dnf install ~/rpmbuild/RPMS/$(uname -m)/sauron-*.rpm
```

**3. Configure, then start**

The RPM **enables** the six daemons but starts nothing, so you configure first:

```bash
sudoedit /etc/sauron/sauron.env          # DATABASE_URL, REDIS_URL — see below
sudo systemctl start sauron-api sauron-ingest sauron-monitor sauron-alerts sauron-tier sauron-storesync
```

Migrations run automatically from then on: every daemon unit carries
`Requires=sauron-migrate.service`, so starting or restarting one applies pending migrations
first and the daemon refuses to start if that fails.

> The trade: a Postgres outage longer than `MIGRATE_WAIT_SECS` (default 120) leaves the daemons
> down until someone starts them by hand — systemd never retries a failed *start job*.

Full detail: **[packaging/rpm/INSTALL.md](packaging/rpm/INSTALL.md)** (build modes, what lands
where, upgrade/uninstall).

---

## C. Debian / Ubuntu (.deb)

The same four packages, for **Debian 12** and **Ubuntu 22.04**. These are separate builds and
not interchangeable: glibc 2.36 vs 2.35, so the Debian package will not start on Ubuntu 22.04.
Build on the distribution you intend to run on.

**1. Build**

Unlike Fedora, **neither target ships a usable toolchain** — Debian 12 has rustc 1.63 and Node
18, Ubuntu 22.04 has rustc 1.75 and Node 12, against floors of Rust 1.82 and Node 22. Installing
the distro `rustc`/`nodejs` packages will not work; use rustup and NodeSource, which is what CI
does:

```bash
sudo apt-get install -y debhelper dpkg-dev gcc libc6-dev perl pkg-config libzstd-dev curl unzip git

# Rust >= 1.82
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o rustup-init.sh
sh rustup-init.sh -y --profile minimal --default-toolchain stable
. "$HOME/.cargo/env"

# Node 22
curl -fsSL https://deb.nodesource.com/setup_22.x -o nodesource_setup.sh
sudo bash nodesource_setup.sh && sudo apt-get install -y nodejs
```

```bash
./packaging/deb/build-deb.sh
```

Artifacts land in `build/deb/`.

> Only the *build* needs that toolchain. Packaging already-compiled artifacts
> (`build-deb.sh --prebuilt DIR`) needs nothing but `debhelper` and `dpkg-dev`, because
> `debian/rules` never invokes a compiler.

**2. Install**

`apt-get`, not `dpkg -i`, so `nginx` and the `systemd-sysusers` provider get resolved:

```bash
cd build/deb
sudo apt-get install ./sauron_*.deb ./sauron-server_*.deb ./sauron-dashboard_*.deb ./sauron-cli_*.deb
```

**3. Expect failed units, then configure**

Unlike the RPM, the `.deb` follows Debian policy and **starts the services on install**. On a
fresh host nothing is configured yet, so every daemon fails immediately:

```
● sauron-api.service - Sauron dashboard API
     Active: failed (Result: exit-code)
```

This is expected — the install itself succeeds. Configure, then start:

```bash
sudoedit /etc/sauron/sauron.env          # DATABASE_URL, REDIS_URL — see below
sudo systemctl start sauron-api sauron-ingest sauron-monitor sauron-alerts sauron-tier sauron-storesync
```

Full detail: **[packaging/deb/INSTALL.md](packaging/deb/INSTALL.md)**.

---

## Essential configuration

Every backend service is configured **purely from the environment** — no config file, no config
crate. A variable that is unset or blank falls back to its default, and an unparseable value
falls back silently too, so a typo'd number is never fatal (and never announced).

These are the ones that actually matter on a first install. Everything else has a working
default.

| Setting | What it is | Docker Compose | RPM / `.deb` |
|---|---|---|---|
| `DATABASE_URL` | PostgreSQL DSN. **Required.** | composed from `POSTGRES_USER` / `POSTGRES_PASSWORD` / `POSTGRES_DB` in `.env` | `/etc/sauron/sauron.env` |
| `REDIS_URL` | Redis / Valkey URL | fixed to the compose service | `/etc/sauron/sauron.env` |
| `JWT_SECRET` | signs dashboard sessions | `.env` — **change it** | `/etc/sauron/secret.env` — generated at install |
| `NOTIFY_SECRET_KEY` | encrypts notification-channel credentials | `.env` — **change it** | `/etc/sauron/secret.env` — generated at install |
| `API_BASE_URL` | where the **browser** reaches the API | `.env` | `/etc/sauron/dashboard.env` |
| `INGEST_BASE_URL` | where **SDKs** reach ingest | `.env` | `/etc/sauron/dashboard.env` |
| `CORS_ALLOWED_ORIGINS` | origin the dashboard is served from | `.env` | `/etc/sauron/api.env` |
| `DASHBOARD_URL` | origin used inside emails (password resets) | `.env` (commented) | `/etc/sauron/api.env` (commented) |
| `TIER_HOT_DAYS` | hot/cold boundary, days | `.env` | `/etc/sauron/sauron.env` |
| `RUST_LOG` | log filter, e.g. `info,sauron=debug` | `.env` | `/etc/sauron/sauron.env` |

### Three that bite

**`API_BASE_URL` / `INGEST_BASE_URL` are browser-facing, not bind addresses.** They are baked
into the dashboard's `config.js` and resolved by the *user's* browser, so `localhost` works only
when the browser is on the same machine. Behind a reverse proxy or on a remote host these must
be the public URLs. On a native install, after editing `dashboard.env`:

```bash
sudo /usr/libexec/sauron/sauron-dashboard-config && sudo systemctl reload nginx
```

**`JWT_SECRET` and `NOTIFY_SECRET_KEY` are fail-closed.** `sauron-api`, `sauron-monitor` and
`sauron-alerts` refuse to start without `NOTIFY_SECRET_KEY` rather than run with encryption
disabled. On native installs both are generated for you at first install and you never need to
touch them.

**Never rotate `NOTIFY_SECRET_KEY` once notification channels exist.** Every stored channel
credential is encrypted under it. A new key boots cleanly and then fails every delivery with
"secret decrypt failed" — silent, total loss of every configured channel.

### Where to find everything else

| Source | What it gives you |
|---|---|
| **[`.env.example`](.env.example)** | every key Sauron reads, with comments and defaults. A Rust test (`config_keys_documented.rs`) fails the build if `config.rs` reads a key this file does not document, so it cannot go stale. |
| **[README → Configuration](README.md#configuration)** | the full table, including a **Used by** column naming which services consume each variable |
| **[`backend/crates/sauron-core/src/config.rs`](backend/crates/sauron-core/src/config.rs)** | the parser itself — the authority on defaults and clamping |
| `/etc/sauron/*.env` (native installs) | the same settings, split per service, with the reasoning inline |

On a native install the config files are split by service — `sauron.env` (shared),
`api.env`, `ingest.env`, `monitor.env`, `alerts.env`, `tier.env`, `inspector.env`,
`storesync.env`, `dashboard.env` — and each daemon loads only `sauron.env` plus its own. They
are marked as config files, so your edits survive upgrades.

---

## Verify

```bash
curl -fsS http://localhost:8080/health     # native  (10000 on Compose)
curl -fsS http://localhost:8081/health     # native  (10001 on Compose)
```

Then open the dashboard, register the first account, and create a project → an app. The app
starts with one environment (`dev`); copy its DSN from **Settings → Environments** into an SDK
and watch the first event land.

> The environment an event belongs to is proven by the ingest key it arrived with, so a client
> cannot spoof it.

---

## Next steps

- **[packaging/rpm/SETUP.md](packaging/rpm/SETUP.md)** — the operations guide: provisioning
  Postgres and Redis, secrets, migrations, enabling the PII inspector and transactional email,
  firewalling, troubleshooting, and the upgrade procedure. Written against the RPM layout, but
  every path is identical on the `.deb`, so it applies unchanged there.
- **[README.md](README.md)** — architecture, SDK setup, the query language, and the full
  configuration table.
