# Sauron RPM Packaging — Design

**Date:** 2026-07-16
**Status:** Approved (design), pending implementation
**Scope:** Native RPM packaging of the Sauron backend + dashboard for Fedora and RHEL-family
distributions, plus operator-facing install & setup documentation.

---

## 1. Goal

Produce installable `.rpm` artifacts so Sauron can be deployed on Fedora / RHEL / Rocky / Alma
without Docker, driven by systemd, following the Filesystem Hierarchy Standard (FHS) and Fedora
packaging conventions. Ship the documentation an operator needs to install and configure the stack.

Non-goals (explicitly out of scope for this cut):

- COPR / Koji submission, signed repositories, `.deb` packaging.
- Auto-provisioning or hard-requiring Postgres / Redis — treated as external services and documented.
- Bundling a vendored crate archive in the shipped spec (a `cargo vendor` offline variant is noted as
  a future follow-up for COPR builds, not built now).

## 2. Decisions (locked)

| Question | Decision |
|---|---|
| Package layout | **Subpackages**: `sauron` (base) + `sauron-server` + `sauron-dashboard` + `sauron-cli` |
| Build method | **Build from source in the spec** (`cargo build --release`; `npm run build`) |
| Postgres / Redis | **External, documented** — not hard-required, not auto-configured |

## 3. What is being packaged

Discovered from the workspace (`backend/Cargo.toml`, per-bin manifests, `docker-compose.yml`,
`sauron-core/src/config.rs`):

**Long-running services (systemd units):**

- `sauron-api` — dashboard API, TCP `:8080`. Needs Postgres, Redis, `JWT_SECRET`. Reads cold tier RO.
- `sauron-ingest` — SDK edge + co-located worker pool, TCP `:8081`. Needs Postgres, Redis.
- `sauron-monitor` — uptime prober, no listener. Needs Postgres.
- `sauron-tier` — hot/cold Parquet tiering, no listener. Needs Postgres; writes cold path.

**One-shot:**

- `sauron-migrate` — applies embedded migrations then exits. Needs only `DATABASE_URL`.
  Migrations are compiled in via `embed_migrations!("../../migrations")`
  ([sauron-db/src/lib.rs:23](../../../backend/crates/sauron-db/src/lib.rs)); **no migration files
  ship in the RPM.**

**CLI tools (not services):**

- `crebain` — load / benchmark generator.
- `sauron-symcli` — symbolication CLI. (Bin package name in Cargo is `sauron-symcli`.)

**Dashboard:**

- Vite + Svelte 5 SPA. Built to `dashboard/dist/`. Served as static files by nginx. Runtime config
  (`API_BASE_URL`, `INGEST_BASE_URL`) is injected into `config.js` from `config.template.js` via
  `envsubst` — these are the **browser-facing** URLs, not the service bind addresses.

### Runtime dependency notes (why the RPMs are lean)

- `diesel` uses `postgres_backend` (query builder only) + `diesel-async` for I/O → **no libpq linkage**.
- `reqwest` uses `rustls-tls` → **no OpenSSL linkage**.
- `duckdb` is built with the `bundled` feature → **statically compiled into `sauron-tier`**; no
  external duckdb package.
- `ldd` on a release binary confirms no pq / ssl / duckdb dynamic dependencies (glibc only).

Consequence: `Requires` on the RPMs is essentially glibc + systemd + (for the dashboard) nginx.
Postgres / Redis are external and declared only as weak `Recommends`.

## 4. Package structure

One spec, `packaging/rpm/sauron.spec`, `Name: sauron`, `Version` tracks the workspace version
(`0.1.0`), producing four binary RPMs:

| RPM | Contents | Requires | Recommends |
|---|---|---|---|
| `sauron` (base) | `sauron` system user (sysusers.d), `/var/lib/sauron` + `cold` (tmpfiles.d), shared `/etc/sauron/sauron.env`, `LICENSE`, top-level docs | `shadow-utils`, systemd scriptlet deps | — |
| `sauron-server` | `sauron-{api,ingest,monitor,tier,migrate}` binaries; systemd units; `/etc/sauron/{api,ingest,monitor,tier}.env`; `secret.env`; INSTALL/SETUP docs | `sauron = %{version}-%{release}` | `postgresql-server`, `valkey` |
| `sauron-dashboard` | built SPA under `/usr/share/sauron/dashboard/`; `config.template.js`; nginx vhost; `sauron-dashboard-config` generator; `/etc/sauron/dashboard.env` | `sauron = %{version}-%{release}`, `nginx` | — |
| `sauron-cli` | `crebain`, `sauron-symcli` | — | — |

Rationale: the base package owns the shared user / data dir / common env so that `sauron-server` and
`sauron-dashboard` compose without duplication. `sauron-cli` is standalone (its tools do not need the
service user or `/etc/sauron`).

## 5. Installed file layout (FHS)

```
/usr/bin/sauron-api
/usr/bin/sauron-ingest
/usr/bin/sauron-monitor
/usr/bin/sauron-tier
/usr/bin/sauron-migrate
/usr/bin/sauron-symcli
/usr/bin/crebain

/usr/lib/systemd/system/sauron-api.service
/usr/lib/systemd/system/sauron-ingest.service
/usr/lib/systemd/system/sauron-monitor.service
/usr/lib/systemd/system/sauron-tier.service
/usr/lib/systemd/system/sauron-migrate.service          # Type=oneshot

/usr/lib/sysusers.d/sauron.conf                         # creates 'sauron' system user + group
/usr/lib/tmpfiles.d/sauron.conf                         # /var/lib/sauron, /var/lib/sauron/cold perms

/etc/sauron/sauron.env                                  # DATABASE_URL, REDIS_URL, RUST_LOG (shared)
/etc/sauron/api.env                                     # API_PORT, JWT TTLs, CORS_ALLOWED_ORIGINS, SYMBOLS_*
/etc/sauron/ingest.env                                  # INGEST_PORT, WORKER_CONCURRENCY, rate limit, SYMBOLS_*
/etc/sauron/monitor.env                                 # MONITOR_* tunables
/etc/sauron/tier.env                                    # TIER_* tunables (TIER_COLD_PATH=/var/lib/sauron/cold)
/etc/sauron/dashboard.env                               # API_BASE_URL, INGEST_BASE_URL (browser-facing)
/etc/sauron/secret.env             (0640 root:sauron)   # JWT_SECRET — generated on first install

/var/lib/sauron/                                        # owned by sauron:sauron (StateDirectory)
/var/lib/sauron/cold/                                   # cold-tier Parquet output

/usr/share/sauron/dashboard/                            # built SPA + config.template.js + config.js
/etc/nginx/conf.d/sauron-dashboard.conf                 # server block (root -> /usr/share/sauron/dashboard)
/usr/libexec/sauron/sauron-dashboard-config             # regenerates config.js from dashboard.env

/usr/share/doc/sauron-server/INSTALL.md
/usr/share/doc/sauron-server/SETUP.md
/usr/share/licenses/sauron/LICENSE
```

All `/etc/sauron/*.env` files are marked `%config(noreplace)` so upgrades never overwrite operator
edits.

## 6. Build phase (`%build` / `%install`)

**BuildRequires:** `cargo`, `rust >= 1.82`, `gcc`, `gcc-c++`, `cmake`, `perl`, `nodejs`, `npm`,
`systemd-rpm-macros`.

- `gcc-c++` / `cmake` are required by the bundled DuckDB C++ amalgamation (`sauron-tier`) and by the
  rustls crypto backend (aws-lc-rs / ring).
- Exact `BuildRequires` will be confirmed during the verification build and tightened.

**`%build`:**

1. Backend — from `backend/`: `cargo build --release` for the seven binaries (offline network access
   is available on the operator's Fedora box; not a Koji/COPR sandbox).
2. Dashboard — from `dashboard/`: `npm ci && npm run build` → `dist/`.

**`%install`:** copy the release binaries to `%{buildroot}/usr/bin`, the dashboard `dist/` +
`config.template.js` to `/usr/share/sauron/dashboard`, and install the systemd units, sysusers,
tmpfiles, nginx vhost, env templates, generator script, and docs from spec `SourceN` files.

## 7. systemd unit design

Common pattern (illustrated for `sauron-api`):

```ini
[Unit]
Description=Sauron dashboard API
After=network-online.target
Wants=network-online.target
# Optional ordering so migrations land first when enabled:
After=sauron-migrate.service

[Service]
Type=exec
User=sauron
Group=sauron
EnvironmentFile=/etc/sauron/sauron.env
EnvironmentFile=-/etc/sauron/api.env
EnvironmentFile=-/etc/sauron/secret.env
ExecStart=/usr/bin/sauron-api
Restart=on-failure
RestartSec=2

# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
StateDirectory=sauron
ReadWritePaths=/var/lib/sauron

[Install]
WantedBy=multi-user.target
```

Per-service deltas:

- `sauron-ingest`: also loads `secret.env`? No — ingest does not need JWT. Loads `ingest.env`.
- `sauron-monitor`: loads `monitor.env`; no cold-path access.
- `sauron-tier`: loads `tier.env`; needs RW on `/var/lib/sauron/cold` (covered by `ReadWritePaths`).
- `sauron-api`: needs RO read of the cold path for cross-tier reads (within `/var/lib/sauron`).
- `sauron-migrate`: `Type=oneshot`, loads only `sauron.env`; no hardening state dir needed beyond
  network; intended to be run once (`systemctl start sauron-migrate`).

Per Fedora policy the RPM **registers** units (`%systemd_post`) but does **not** enable or start them.
Restart-on-upgrade uses `%systemd_postun_with_restart` for the long-running units.

## 8. JWT secret handling

Production must not run on the insecure in-code default. The `sauron-server` `%post` scriptlet, on
first install only, generates a random secret into `/etc/sauron/secret.env` if that file does not
already contain a real value:

```
JWT_SECRET=<64 hex chars from a secure source>
```

`secret.env` is `0640 root:sauron` so only root writes it and the `sauron` service user reads it. It is
`%ghost`/generated (not shipped with a placeholder value) to avoid a known-secret footgun.

## 9. Dashboard runtime config

The Docker entrypoint's `envsubst` step becomes `/usr/libexec/sauron/sauron-dashboard-config`:

- Reads `/etc/sauron/dashboard.env` (`API_BASE_URL`, `INGEST_BASE_URL`).
- Renders `/usr/share/sauron/dashboard/config.template.js` → `config.js`.
- Invoked by the dashboard `%post`, and re-runnable by operators after editing the env file.
- nginx serves `config.js` with `no-store` (carried over from `dashboard/nginx.conf`), so URL changes
  take effect without a rebuild.

## 10. Documentation deliverables

- `packaging/rpm/INSTALL.md` — build the RPMs (helper script / raw `rpmbuild`), `dnf install
  ./sauron-*.rpm`, files-installed reference, upgrade & uninstall.
- `packaging/rpm/SETUP.md` — end-to-end operator runbook:
  1. Provision Postgres (create DB + role) — local or remote.
  2. Provision valkey / redis.
  3. Edit `/etc/sauron/sauron.env` (`DATABASE_URL`, `REDIS_URL`).
  4. Confirm / set `JWT_SECRET` (auto-generated; how to rotate).
  5. Run migrations: `systemctl start sauron-migrate` (or `sauron-migrate`).
  6. `systemctl enable --now sauron-{api,ingest,monitor,tier}`.
  7. Configure `/etc/sauron/dashboard.env`, regenerate config, enable nginx.
  8. firewalld ports (8080 / 8081 / 80).
  9. Verify: `curl` health, `journalctl -u`.
  10. Troubleshooting matrix.
- `packaging/rpm/build-rpm.sh` — stages a source tarball into `~/rpmbuild/SOURCES` and runs
  `rpmbuild -ba packaging/rpm/sauron.spec`.
- `README.md` — new "Install via RPM (Fedora / RHEL)" section linking the above.
- `wiki/RPM-Install.md` — mirror of the install/setup guide for the wiki, linked from `_Sidebar.md`
  and `Home.md`.

## 11. Repository layout (new files)

```
packaging/rpm/
  sauron.spec
  build-rpm.sh
  INSTALL.md
  SETUP.md
  systemd/
    sauron-api.service
    sauron-ingest.service
    sauron-monitor.service
    sauron-tier.service
    sauron-migrate.service
  config/
    sauron.env
    api.env
    ingest.env
    monitor.env
    tier.env
    dashboard.env
  sysusers/
    sauron.conf
  tmpfiles/
    sauron.conf
  nginx/
    sauron-dashboard.conf
  scripts/
    sauron-dashboard-config
docs/superpowers/specs/
  2026-07-16-sauron-rpm-packaging-design.md   (this file)
wiki/
  RPM-Install.md
```

These auxiliary files are referenced from the spec as `Source1..N`.

## 12. Verification plan

The build host is Fedora 44, so the spec is verified by actually building it, not by inspection:

1. `rpmlint packaging/rpm/sauron.spec` — clean or justified warnings.
2. `rpmbuild -bs` → source RPM builds.
3. `rpmbuild -bb` → the four binary RPMs build (fixing `BuildRequires` iteratively; the full backend
   compile incl. bundled DuckDB is slow but is run).
4. `rpm -qlp` on each RPM — confirm the file manifest matches §5.
5. Install sanity: `rpm -i` in a throwaway root / container; confirm the `sauron` user is created,
   units are present (`systemctl cat sauron-api`), `secret.env` is generated, and
   `sauron-migrate --help` / a binary smoke-run works.

Success = the four RPMs build and install cleanly, the file manifest matches, and the systemd units +
generated secret are in place.

## 13. Risks / open points

- **Build time & memory:** compiling the workspace with bundled DuckDB is heavy. The verification
  build may take many minutes; this is expected, not a failure.
- **Exact `BuildRequires`:** the rustls crypto backend and DuckDB may pull additional C toolchain deps;
  the list in §6 is the starting point and will be tightened during the verification build.
- **valkey vs redis naming:** Fedora 41+ ships `valkey` (redis was retired); RHEL ships `redis`. Weak
  `Recommends` + docs cover both rather than a hard `Requires`.
- **nginx as a hard dependency of `sauron-dashboard`:** acceptable since the subpackage is optional; an
  operator who serves the SPA elsewhere simply does not install `sauron-dashboard`.
