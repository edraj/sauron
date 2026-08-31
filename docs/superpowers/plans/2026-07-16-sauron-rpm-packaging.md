# Sauron RPM Packaging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Package the Sauron backend services, dashboard, and CLI tools as native RPMs for Fedora/RHEL, driven by systemd, plus operator install & setup documentation.

**Architecture:** A single spec (`packaging/rpm/sauron.spec`) builds all seven Rust binaries from source (`cargo build --release`) and the Svelte dashboard (`npm run build`), then emits four subpackages — `sauron` (base: user + data dir + shared config), `sauron-server` (services + systemd units), `sauron-dashboard` (static SPA + nginx vhost), `sauron-cli` (crebain + symcli). Postgres/Redis are external and documented, not required. Auxiliary files (units, env templates, sysusers, tmpfiles, nginx conf, generator) live under `packaging/rpm/` and are referenced from the spec as `SourceN`.

**Tech Stack:** rpmbuild, Fedora `systemd-rpm-macros`, systemd units, Rust/cargo, Node/npm, nginx, sh.

## Global Constraints

- Design spec: [docs/superpowers/specs/2026-07-16-sauron-rpm-packaging-design.md](../specs/2026-07-16-sauron-rpm-packaging-design.md). Every task's requirements implicitly include it.
- **Version:** `0.1.0` — must match `[workspace.package] version` in `backend/Cargo.toml`. License: `AGPL-3.0-only`.
- **Binary names produced by `cargo build --release --workspace`:** `sauron-api`, `sauron-ingest`, `sauron-monitor`, `sauron-tier`, `sauron-migrate`, `sauron-symcli`, `crebain` (all land in `backend/target/release/`).
- **Config paths:** shared env `/etc/sauron/sauron.env`; per-service `/etc/sauron/{api,ingest,monitor,tier,dashboard}.env`; generated `/etc/sauron/secret.env`. Data under `/var/lib/sauron` (+ `/cold`). Dashboard webroot `/usr/share/sauron/dashboard`.
- **Only `DATABASE_URL` is strictly required by the binaries**; everything else defaults (see `sauron-core/src/config.rs`). Migrations are compiled into `sauron-migrate` (`embed_migrations!`) — no migration files ship.
- **Git policy (project rule — overrides the skill's "commit" steps):** do NOT run `git commit`. Each task ends by *staging* (`git add`) as a checkpoint; commits happen only on explicit user request.
- **Runtime deps are minimal:** binaries link glibc only (no libpq/OpenSSL/duckdb dynamic libs). `sauron-dashboard` requires `nginx`; server `Recommends` (weak) `postgresql-server` + `valkey`.
- Build host is **Fedora 44** (`rpmbuild`, `rpmspec`, `systemd-rpm-macros` present; `rpmlint` optional via `dnf install rpmlint`).

---

## File Structure

```
packaging/rpm/
  sauron.spec                       # the spec: 4 subpackages, build-from-source
  build-rpm.sh                      # stage working-tree tarball + aux sources, run rpmbuild -ba
  INSTALL.md                        # how to build & install the RPMs
  SETUP.md                          # operator runbook (DB/Redis → migrate → enable → dashboard)
  systemd/
    sauron-api.service              # Type=exec, :8080
    sauron-ingest.service           # Type=exec, :8081
    sauron-monitor.service          # Type=exec
    sauron-tier.service             # Type=exec, RW cold
    sauron-migrate.service          # Type=oneshot
  config/
    sauron.env                      # DATABASE_URL, REDIS_URL, RUST_LOG (shared)
    api.env  ingest.env  monitor.env  tier.env  dashboard.env
  sysusers/sauron.conf              # creates 'sauron' user
  tmpfiles/sauron.conf              # /var/lib/sauron perms
  nginx/sauron-dashboard.conf       # server block
  scripts/sauron-dashboard-config   # regenerate config.js from dashboard.env (sed)
wiki/RPM-Install.md                 # wiki mirror of install/setup
README.md                           # +"Install via RPM" section (modify)
wiki/_Sidebar.md                    # +link (modify)
```

---

### Task 1: Shared config, sysusers, tmpfiles

**Files:**
- Create: `packaging/rpm/config/sauron.env`, `api.env`, `ingest.env`, `monitor.env`, `tier.env`, `dashboard.env`
- Create: `packaging/rpm/sysusers/sauron.conf`
- Create: `packaging/rpm/tmpfiles/sauron.conf`

**Interfaces:**
- Produces: env var names consumed by the systemd units (Task 2) via `EnvironmentFile`; the `sauron` user/group consumed by tmpfiles + unit `User=`; `/var/lib/sauron` + `/var/lib/sauron/cold` dirs consumed by `sauron-tier` (RW) and `sauron-api` (RO). Env keys are exactly those parsed in `backend/crates/sauron-core/src/config.rs`.

- [ ] **Step 1: Write `packaging/rpm/config/sauron.env`**

```sh
# Shared Sauron configuration — sourced by every sauron-* systemd unit.
# Edit for your environment, then: systemctl restart 'sauron-*'

# PostgreSQL DSN (REQUIRED). Point at your Postgres instance.
DATABASE_URL=postgres://sauron:sauron@localhost:5432/sauron

# Redis / Valkey URL (used by sauron-api and sauron-ingest).
REDIS_URL=redis://127.0.0.1:6379

# Log filter. e.g. info | info,sauron=debug | warn
RUST_LOG=info,sauron=info
```

- [ ] **Step 2: Write `packaging/rpm/config/api.env`**

```sh
# sauron-api — dashboard JWT API (listens on API_PORT).
API_PORT=8080

# Comma-separated browser origins allowed by CORS (where the dashboard is served).
CORS_ALLOWED_ORIGINS=http://localhost

# Token lifetimes (seconds).
JWT_ACCESS_TTL_SECS=900
JWT_REFRESH_TTL_SECS=2592000

# Cold-tier Parquet path (read for cross-tier queries).
TIER_COLD_PATH=/var/lib/sauron/cold

# Symbolication (source maps / debug files).
SYMBOLS_CACHE_MB=256
# SYMBOLS_REDIS_URL=redis://127.0.0.1:6379/1
```

- [ ] **Step 3: Write `packaging/rpm/config/ingest.env`**

```sh
# sauron-ingest — SDK edge + worker pool (listens on INGEST_PORT).
INGEST_PORT=8081
WORKER_CONCURRENCY=4
INGEST_RATE_LIMIT_PER_MIN=6000
INGEST_MAX_BODY_BYTES=1048576

# Ingest-path symbolication time box (ms).
SYMBOLS_INGEST_TIMEOUT_MS=150
# SYMBOLS_REDIS_URL=redis://127.0.0.1:6379/1
```

- [ ] **Step 4: Write `packaging/rpm/config/monitor.env`**

```sh
# sauron-monitor — uptime prober.
MONITOR_TICK_MS=1000
MONITOR_BATCH=100
MONITOR_MAX_CONCURRENCY=50
MONITOR_CHECK_RETENTION_DAYS=30
# Set true ONLY to intentionally probe private/internal addresses (SSRF guard).
MONITOR_SSRF_ALLOW_PRIVATE=false
```

- [ ] **Step 5: Write `packaging/rpm/config/tier.env`**

```sh
# sauron-tier — hot/cold Parquet tiering.
TIER_HOT_DAYS=30
TIER_GRANULARITY=day
TIER_COLD_PATH=/var/lib/sauron/cold
TIER_DROP_LAG_HOURS=24
TIER_TICK_SECS=3600
TIER_PARTITION_AHEAD=7
```

- [ ] **Step 6: Write `packaging/rpm/config/dashboard.env`**

```sh
# sauron-dashboard — browser-facing base URLs injected into config.js.
# These are the URLs the USER'S BROWSER uses to reach the API and ingest
# (your public / reverse-proxied addresses), NOT the service bind ports.
# After editing: /usr/libexec/sauron/sauron-dashboard-config && systemctl reload nginx
API_BASE_URL=http://localhost:8080
INGEST_BASE_URL=http://localhost:8081
```

- [ ] **Step 7: Write `packaging/rpm/sysusers/sauron.conf`**

```
#Type Name    ID  GECOS             Home directory   Shell
u     sauron  -   "Sauron service"  /var/lib/sauron  /usr/sbin/nologin
```

- [ ] **Step 8: Write `packaging/rpm/tmpfiles/sauron.conf`**

```
#Type Path                  Mode UID    GID    Age Argument
d     /var/lib/sauron       0750 sauron sauron -   -
d     /var/lib/sauron/cold  0750 sauron sauron -   -
```

- [ ] **Step 9: Verify the env files parse as shell and keys match the code**

Run:
```bash
for f in packaging/rpm/config/*.env; do sh -n "$f" && echo "ok: $f"; done
grep -oE '^[A-Z_]+=' packaging/rpm/config/*.env | sed 's/.*://; s/=//' | sort -u > /tmp/plan_envkeys.txt
grep -oE '"[A-Z_]+"' backend/crates/sauron-core/src/config.rs | tr -d '"' | sort -u > /tmp/plan_codekeys.txt
comm -23 /tmp/plan_envkeys.txt /tmp/plan_codekeys.txt
```
Expected: every `ok: ...` prints; the final `comm` prints **nothing** (every env key we ship is a key the code reads — `POSTGRES_*` are not among them since those are compose-only).

- [ ] **Step 10: Verify sysusers/tmpfiles are well-formed (if systemd-sysusers available)**

Run:
```bash
systemd-sysusers --dry-run packaging/rpm/sysusers/sauron.conf 2>&1 | head
systemd-tmpfiles --dry-run --create packaging/rpm/tmpfiles/sauron.conf 2>&1 | head
```
Expected: no parse errors (the sysusers dry-run may report it *would* create user `sauron`; tmpfiles dry-run lists the two dirs). A "command not found" is acceptable to note and skip.

- [ ] **Step 11: Checkpoint (stage; hold commit)**

```bash
git add packaging/rpm/config packaging/rpm/sysusers packaging/rpm/tmpfiles
# Do NOT commit — project rule: commits only on explicit user request.
```

---

### Task 2: systemd unit files

**Files:**
- Create: `packaging/rpm/systemd/sauron-api.service`, `sauron-ingest.service`, `sauron-monitor.service`, `sauron-tier.service`, `sauron-migrate.service`

**Interfaces:**
- Consumes: `/etc/sauron/sauron.env` + per-service env files (Task 1); `User=sauron` (Task 1 sysusers); `/var/lib/sauron` (Task 1 tmpfiles); binaries at `/usr/bin/sauron-*` (installed by Task 4).
- Produces: unit names `sauron-{api,ingest,monitor,tier,migrate}.service` referenced by the spec scriptlets (Task 4) and the docs (Task 7).

- [ ] **Step 1: Write `packaging/rpm/systemd/sauron-api.service`**

```ini
[Unit]
Description=Sauron dashboard API
Documentation=file:///usr/share/doc/sauron-server/SETUP.md
After=network-online.target sauron-migrate.service
Wants=network-online.target

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
PrivateDevices=true
ProtectControlGroups=true
ProtectKernelModules=true
ProtectKernelTunables=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
StateDirectory=sauron
ReadWritePaths=/var/lib/sauron

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 2: Write `packaging/rpm/systemd/sauron-ingest.service`**

```ini
[Unit]
Description=Sauron ingest edge and worker pool
Documentation=file:///usr/share/doc/sauron-server/SETUP.md
After=network-online.target sauron-migrate.service
Wants=network-online.target

[Service]
Type=exec
User=sauron
Group=sauron
EnvironmentFile=/etc/sauron/sauron.env
EnvironmentFile=-/etc/sauron/ingest.env
ExecStart=/usr/bin/sauron-ingest
Restart=on-failure
RestartSec=2

NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectControlGroups=true
ProtectKernelModules=true
ProtectKernelTunables=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
StateDirectory=sauron
ReadWritePaths=/var/lib/sauron

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 3: Write `packaging/rpm/systemd/sauron-monitor.service`**

```ini
[Unit]
Description=Sauron uptime monitor
Documentation=file:///usr/share/doc/sauron-server/SETUP.md
After=network-online.target sauron-migrate.service
Wants=network-online.target

[Service]
Type=exec
User=sauron
Group=sauron
EnvironmentFile=/etc/sauron/sauron.env
EnvironmentFile=-/etc/sauron/monitor.env
ExecStart=/usr/bin/sauron-monitor
Restart=on-failure
RestartSec=2

NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectControlGroups=true
ProtectKernelModules=true
ProtectKernelTunables=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
StateDirectory=sauron

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 4: Write `packaging/rpm/systemd/sauron-tier.service`**

```ini
[Unit]
Description=Sauron hot/cold tiering worker
Documentation=file:///usr/share/doc/sauron-server/SETUP.md
After=network-online.target sauron-migrate.service
Wants=network-online.target

[Service]
Type=exec
User=sauron
Group=sauron
EnvironmentFile=/etc/sauron/sauron.env
EnvironmentFile=-/etc/sauron/tier.env
ExecStart=/usr/bin/sauron-tier
Restart=on-failure
RestartSec=2

NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectControlGroups=true
ProtectKernelModules=true
ProtectKernelTunables=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
StateDirectory=sauron
ReadWritePaths=/var/lib/sauron

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 5: Write `packaging/rpm/systemd/sauron-migrate.service`**

```ini
[Unit]
Description=Sauron database migrations (one-shot)
Documentation=file:///usr/share/doc/sauron-server/SETUP.md
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
User=sauron
Group=sauron
EnvironmentFile=/etc/sauron/sauron.env
ExecStart=/usr/bin/sauron-migrate

NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

# No [Install] section — run on demand: systemctl start sauron-migrate
```

- [ ] **Step 6: Verify unit files parse**

Run:
```bash
for u in packaging/rpm/systemd/*.service; do
  echo "--- $u ---"
  systemd-analyze verify --recursive-errors=no "$u" 2>&1 | grep -v 'Failed to prepare filesystem\|command\|/usr/bin/sauron' || echo "syntax ok"
done
```
Expected: no *syntax/parse* errors. Warnings that `ExecStart=` binary `/usr/bin/sauron-*` does not exist are **expected** (binaries are installed by the RPM, not present in the checkout) and are filtered/ignored. Absence of `systemd-analyze` is acceptable — fall back to `grep -c '^\[' each file` to confirm the `[Unit]/[Service]/[Install]` sections exist.

- [ ] **Step 7: Checkpoint (stage; hold commit)**

```bash
git add packaging/rpm/systemd
# Do NOT commit.
```

---

### Task 3: nginx vhost + dashboard config generator

**Files:**
- Create: `packaging/rpm/nginx/sauron-dashboard.conf`
- Create: `packaging/rpm/scripts/sauron-dashboard-config`

**Interfaces:**
- Consumes: `/etc/sauron/dashboard.env` (`API_BASE_URL`, `INGEST_BASE_URL`, from Task 1); webroot `/usr/share/sauron/dashboard` + its `config.template.js` (installed by Task 4). The template shape is `window.__SAURON_CONFIG__ = { apiBaseUrl: '${API_BASE_URL}', ingestBaseUrl: '${INGEST_BASE_URL}' }` (from `dashboard/static/config.template.js`).
- Produces: `/usr/libexec/sauron/sauron-dashboard-config` invoked by the dashboard `%post` (Task 4) and by operators; nginx server block consumed by nginx.

- [ ] **Step 1: Write `packaging/rpm/nginx/sauron-dashboard.conf`**

```nginx
# Sauron dashboard — static SPA served by the system nginx.
# NOTE: Fedora's stock /etc/nginx/nginx.conf already defines a `server { listen 80
# default_server; }`. To make THIS the site served on :80, either remove that
# default server block or give this one `listen 80 default_server;` and a real
# server_name. Add TLS as needed for production.
server {
    listen 80;
    server_name _;
    root /usr/share/sauron/dashboard;
    index index.html;

    # Runtime config must never be cached — ops changes the API URL without a rebuild.
    location = /config.js {
        add_header Cache-Control "no-store, no-cache, must-revalidate";
        expires -1;
        try_files $uri =404;
    }

    location = /index.html {
        add_header Cache-Control "no-store, no-cache, must-revalidate";
        expires -1;
    }

    # Fingerprinted build assets — cache aggressively.
    location /assets/ {
        add_header Cache-Control "public, max-age=31536000, immutable";
        try_files $uri =404;
    }

    # SPA fallback.
    location / {
        try_files $uri $uri/ /index.html;
    }

    gzip on;
    gzip_types text/css application/javascript application/json image/svg+xml;
    gzip_min_length 1024;
}
```

- [ ] **Step 2: Write `packaging/rpm/scripts/sauron-dashboard-config`**

Uses `sed` (not `envsubst`) so the dashboard subpackage needs no `gettext` dependency.

```sh
#!/bin/sh
# Regenerate the dashboard's runtime config.js from /etc/sauron/dashboard.env.
# Run after editing dashboard.env, then: systemctl reload nginx
set -eu

ENV_FILE="${SAURON_DASHBOARD_ENV:-/etc/sauron/dashboard.env}"
WEBROOT="${SAURON_DASHBOARD_ROOT:-/usr/share/sauron/dashboard}"
TEMPLATE="$WEBROOT/config.template.js"
OUTPUT="$WEBROOT/config.js"

# shellcheck disable=SC1090
[ -f "$ENV_FILE" ] && . "$ENV_FILE"
: "${API_BASE_URL:=http://localhost:8080}"
: "${INGEST_BASE_URL:=http://localhost:8081}"

if [ ! -f "$TEMPLATE" ]; then
    echo "sauron-dashboard-config: template not found: $TEMPLATE" >&2
    exit 1
fi

tmp="$(mktemp "${OUTPUT}.XXXXXX")"
sed -e "s|\${API_BASE_URL}|${API_BASE_URL}|g" \
    -e "s|\${INGEST_BASE_URL}|${INGEST_BASE_URL}|g" \
    "$TEMPLATE" > "$tmp"
mv -f "$tmp" "$OUTPUT"
echo "sauron-dashboard-config: wrote $OUTPUT (API_BASE_URL=$API_BASE_URL INGEST_BASE_URL=$INGEST_BASE_URL)"
```

- [ ] **Step 3: Make the generator executable**

Run:
```bash
chmod 0755 packaging/rpm/scripts/sauron-dashboard-config
```

- [ ] **Step 4: Test the generator end-to-end against the real template**

Run:
```bash
tmp="$(mktemp -d)"
mkdir -p "$tmp/web"
cp dashboard/static/config.template.js "$tmp/web/config.template.js"
printf 'API_BASE_URL=https://api.example.com\nINGEST_BASE_URL=https://ingest.example.com\n' > "$tmp/dashboard.env"
SAURON_DASHBOARD_ENV="$tmp/dashboard.env" SAURON_DASHBOARD_ROOT="$tmp/web" \
  sh packaging/rpm/scripts/sauron-dashboard-config
cat "$tmp/web/config.js"
```
Expected output contains:
```
apiBaseUrl: 'https://api.example.com',
ingestBaseUrl: 'https://ingest.example.com',
```
and no literal `${...}` remain. (If `shellcheck` is installed, also run `shellcheck packaging/rpm/scripts/sauron-dashboard-config` and expect no errors.)

- [ ] **Step 5: Checkpoint (stage; hold commit)**

```bash
git add packaging/rpm/nginx packaging/rpm/scripts
# Do NOT commit.
```

---

### Task 4: The RPM spec

**Files:**
- Create: `packaging/rpm/sauron.spec`

**Interfaces:**
- Consumes: all Task 1–3 files (as `SourceN`), the built binaries from `%build`, the dashboard `dist/` from `%build`, and `packaging/rpm/{INSTALL,SETUP}.md` (Task 7) + `LICENSE`/`README.md` (repo root) as `%doc`/`%license`.
- Produces: the four RPMs. `Source0` tarball name `sauron-0.1.0.tar.gz` with top dir `sauron-0.1.0/` (produced by Task 5's script).

- [ ] **Step 1: Write `packaging/rpm/sauron.spec`**

```spec
# Rust release binaries; skip the debuginfo subpackage for this build.
%global debug_package %{nil}

Name:           sauron
Version:        0.1.0
Release:        1%{?dist}
Summary:        Unified error reporting and product analytics platform

License:        AGPL-3.0-only
URL:            https://github.com/splimter/sauron
Source0:        %{name}-%{version}.tar.gz

# Auxiliary sources staged into SOURCES by packaging/rpm/build-rpm.sh
Source10:       sauron-api.service
Source11:       sauron-ingest.service
Source12:       sauron-monitor.service
Source13:       sauron-tier.service
Source14:       sauron-migrate.service
Source20:       sauron.sysusers
Source21:       sauron.tmpfiles
Source30:       sauron.env
Source31:       api.env
Source32:       ingest.env
Source33:       monitor.env
Source34:       tier.env
Source35:       dashboard.env
Source40:       sauron-dashboard.conf
Source41:       sauron-dashboard-config

BuildRequires:  cargo >= 1.82
BuildRequires:  rust >= 1.82
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  cmake
BuildRequires:  clang
BuildRequires:  perl-interpreter
BuildRequires:  nodejs
BuildRequires:  npm
BuildRequires:  systemd-rpm-macros

Requires:       shadow-utils
%{?sysusers_requires_compat}

%description
Sauron is a Sentry-style error reporting and PostHog-style product analytics
platform on one timeline. This base package provides the shared 'sauron' system
user, data directory, and common configuration used by the server and dashboard
subpackages.

%package server
Summary:        Sauron backend services (API, ingest, monitor, tier, migrate)
Requires:       %{name} = %{version}-%{release}
Recommends:     postgresql-server
Recommends:     valkey
%description server
The Sauron backend services managed by systemd: the JWT dashboard API, the SDK
ingest edge with its co-located worker pool, the uptime monitor, the hot/cold
tiering worker, and the one-shot migration runner.

%package dashboard
Summary:        Sauron web dashboard (static SPA served by nginx)
Requires:       %{name} = %{version}-%{release}
Requires:       nginx
%description dashboard
The Sauron dashboard single-page application, built to static assets and served
by nginx. Runtime API/ingest URLs are injected into config.js from
/etc/sauron/dashboard.env.

%package cli
Summary:        Sauron command-line tools (crebain load generator, symcli)
%description cli
Standalone Sauron command-line tools: 'crebain' load/benchmark generator and
'sauron-symcli' symbolication utility.

%prep
%autosetup -n %{name}-%{version}

%build
# Backend — all workspace binaries, release mode.
(cd backend && cargo build --release --workspace)
# Dashboard — static SPA.
(cd dashboard && npm ci && npm run build)

%install
# --- binaries ---
for b in sauron-api sauron-ingest sauron-monitor sauron-tier sauron-migrate sauron-symcli crebain; do
    install -Dm0755 backend/target/release/$b %{buildroot}%{_bindir}/$b
done

# --- systemd units ---
install -Dm0644 %{SOURCE10} %{buildroot}%{_unitdir}/sauron-api.service
install -Dm0644 %{SOURCE11} %{buildroot}%{_unitdir}/sauron-ingest.service
install -Dm0644 %{SOURCE12} %{buildroot}%{_unitdir}/sauron-monitor.service
install -Dm0644 %{SOURCE13} %{buildroot}%{_unitdir}/sauron-tier.service
install -Dm0644 %{SOURCE14} %{buildroot}%{_unitdir}/sauron-migrate.service

# --- sysusers / tmpfiles ---
install -Dm0644 %{SOURCE20} %{buildroot}%{_sysusersdir}/sauron.conf
install -Dm0644 %{SOURCE21} %{buildroot}%{_tmpfilesdir}/sauron.conf

# --- config ---
install -Dm0640 %{SOURCE30} %{buildroot}%{_sysconfdir}/sauron/sauron.env
install -Dm0640 %{SOURCE31} %{buildroot}%{_sysconfdir}/sauron/api.env
install -Dm0640 %{SOURCE32} %{buildroot}%{_sysconfdir}/sauron/ingest.env
install -Dm0640 %{SOURCE33} %{buildroot}%{_sysconfdir}/sauron/monitor.env
install -Dm0640 %{SOURCE34} %{buildroot}%{_sysconfdir}/sauron/tier.env
install -Dm0644 %{SOURCE35} %{buildroot}%{_sysconfdir}/sauron/dashboard.env

# --- data dirs (also created at runtime by tmpfiles) ---
install -dm0750 %{buildroot}%{_sharedstatedir}/sauron
install -dm0750 %{buildroot}%{_sharedstatedir}/sauron/cold

# --- dashboard static + generator + nginx vhost ---
mkdir -p %{buildroot}%{_datadir}/sauron/dashboard
cp -a dashboard/dist/. %{buildroot}%{_datadir}/sauron/dashboard/
# config.js is generated per-host by %post; ship only the template.
rm -f %{buildroot}%{_datadir}/sauron/dashboard/config.js
install -Dm0644 %{SOURCE40} %{buildroot}%{_sysconfdir}/nginx/conf.d/sauron-dashboard.conf
install -Dm0755 %{SOURCE41} %{buildroot}%{_libexecdir}/sauron/sauron-dashboard-config

%pre
%sysusers_create_compat %{SOURCE20}

%post
%tmpfiles_create %{_tmpfilesdir}/sauron.conf

%post server
%systemd_post sauron-api.service sauron-ingest.service sauron-monitor.service sauron-tier.service sauron-migrate.service
# Generate a JWT secret on first install if none present.
if [ "$1" -eq 1 ] && [ ! -s %{_sysconfdir}/sauron/secret.env ]; then
    umask 027
    printf 'JWT_SECRET=%s\n' "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" > %{_sysconfdir}/sauron/secret.env
    chgrp sauron %{_sysconfdir}/sauron/secret.env 2>/dev/null || :
    chmod 0640 %{_sysconfdir}/sauron/secret.env
fi

%preun server
%systemd_preun sauron-api.service sauron-ingest.service sauron-monitor.service sauron-tier.service sauron-migrate.service

%postun server
%systemd_postun_with_restart sauron-api.service sauron-ingest.service sauron-monitor.service sauron-tier.service

%post dashboard
%{_libexecdir}/sauron/sauron-dashboard-config || :

%files
%license LICENSE
%doc README.md
%dir %{_sysconfdir}/sauron
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/sauron.env
%{_sysusersdir}/sauron.conf
%{_tmpfilesdir}/sauron.conf
%attr(0750,sauron,sauron) %dir %{_sharedstatedir}/sauron
%attr(0750,sauron,sauron) %dir %{_sharedstatedir}/sauron/cold

%files server
%doc packaging/rpm/INSTALL.md packaging/rpm/SETUP.md
%{_bindir}/sauron-api
%{_bindir}/sauron-ingest
%{_bindir}/sauron-monitor
%{_bindir}/sauron-tier
%{_bindir}/sauron-migrate
%{_unitdir}/sauron-api.service
%{_unitdir}/sauron-ingest.service
%{_unitdir}/sauron-monitor.service
%{_unitdir}/sauron-tier.service
%{_unitdir}/sauron-migrate.service
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/api.env
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/ingest.env
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/monitor.env
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/tier.env
%ghost %attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/secret.env

%files dashboard
%dir %{_datadir}/sauron
%{_datadir}/sauron/dashboard/
%ghost %{_datadir}/sauron/dashboard/config.js
%{_libexecdir}/sauron/
%config(noreplace) %{_sysconfdir}/nginx/conf.d/sauron-dashboard.conf
%attr(0644,root,root) %config(noreplace) %{_sysconfdir}/sauron/dashboard.env

%files cli
%{_bindir}/crebain
%{_bindir}/sauron-symcli

%changelog
* Wed Jul 16 2026 Soheyb Merah <merah.soheyb@gmail.com> - 0.1.0-1
- Initial RPM packaging: sauron (base), sauron-server, sauron-dashboard, sauron-cli.
```

- [ ] **Step 2: Verify the spec parses**

Run:
```bash
rpmspec -P packaging/rpm/sauron.spec >/dev/null && echo "spec parses"
rpmspec -q --qf '%{name}-%{version}\n' packaging/rpm/sauron.spec
rpmspec -q --provides packaging/rpm/sauron.spec 2>/dev/null | sort -u
```
Expected: `spec parses`; the second command prints `sauron-0.1.0`; the provides list shows `sauron`, `sauron-server`, `sauron-dashboard`, `sauron-cli` (with arch/version suffixes). A parse error here is a spec bug to fix before proceeding.

- [ ] **Step 3: (Optional) lint**

Run:
```bash
command -v rpmlint >/dev/null && rpmlint packaging/rpm/sauron.spec || echo "rpmlint not installed — skipping"
```
Expected: no `E:` errors. `W:` warnings (e.g. `no-manual-page-for-binary`, `dev/urandom in scriptlet`) are acceptable; note any and move on.

- [ ] **Step 4: Checkpoint (stage; hold commit)**

```bash
git add packaging/rpm/sauron.spec
# Do NOT commit.
```

---

### Task 5: `build-rpm.sh` helper

**Files:**
- Create: `packaging/rpm/build-rpm.sh`

**Interfaces:**
- Consumes: the working tree (backend/, dashboard/, packaging/, LICENSE, README.md) and the spec (Task 4).
- Produces: `~/rpmbuild/SOURCES/sauron-0.1.0.tar.gz` (top dir `sauron-0.1.0/`) + the `SourceN` aux files copied into SOURCES with the names the spec expects (`sauron.sysusers`, `sauron.tmpfiles`, the `*.service`, `*.env`, `sauron-dashboard.conf`, `sauron-dashboard-config`), then runs `rpmbuild -ba`.

**Note:** the tarball is built from the **working tree** (via `tar`, not `git archive`) so uncommitted packaging files are included — required because the project does not commit until explicitly asked.

- [ ] **Step 1: Write `packaging/rpm/build-rpm.sh`**

```bash
#!/usr/bin/env bash
# Build the Sauron RPMs from the current working tree (uncommitted files included).
#
#   ./packaging/rpm/build-rpm.sh            # build source + binary RPMs
#   ./packaging/rpm/build-rpm.sh --srpm     # source RPM only (fast, no compile)
#
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

name=sauron
version="$(awk -F'"' '/^version *= *"/{print $2; exit}' backend/Cargo.toml)"
[ -n "$version" ] || { echo "could not read version from backend/Cargo.toml" >&2; exit 1; }

topdir="${RPMBUILD_TOPDIR:-$HOME/rpmbuild}"
mkdir -p "$topdir"/{SOURCES,SPECS,BUILD,BUILDROOT,RPMS,SRPMS}

echo ">> Staging source tarball ${name}-${version}.tar.gz"
tar czf "$topdir/SOURCES/${name}-${version}.tar.gz" \
    --exclude-vcs \
    --exclude='backend/target' \
    --exclude='dashboard/node_modules' \
    --exclude='dashboard/dist' \
    --exclude='tmp' \
    --transform "s,^,${name}-${version}/," \
    backend dashboard packaging LICENSE README.md

echo ">> Copying auxiliary SourceN files into SOURCES"
install -m0644 packaging/rpm/systemd/sauron-api.service      "$topdir/SOURCES/"
install -m0644 packaging/rpm/systemd/sauron-ingest.service   "$topdir/SOURCES/"
install -m0644 packaging/rpm/systemd/sauron-monitor.service  "$topdir/SOURCES/"
install -m0644 packaging/rpm/systemd/sauron-tier.service     "$topdir/SOURCES/"
install -m0644 packaging/rpm/systemd/sauron-migrate.service  "$topdir/SOURCES/"
install -m0644 packaging/rpm/sysusers/sauron.conf            "$topdir/SOURCES/sauron.sysusers"
install -m0644 packaging/rpm/tmpfiles/sauron.conf            "$topdir/SOURCES/sauron.tmpfiles"
install -m0644 packaging/rpm/config/sauron.env              "$topdir/SOURCES/"
install -m0644 packaging/rpm/config/api.env                 "$topdir/SOURCES/"
install -m0644 packaging/rpm/config/ingest.env             "$topdir/SOURCES/"
install -m0644 packaging/rpm/config/monitor.env            "$topdir/SOURCES/"
install -m0644 packaging/rpm/config/tier.env              "$topdir/SOURCES/"
install -m0644 packaging/rpm/config/dashboard.env         "$topdir/SOURCES/"
install -m0644 packaging/rpm/nginx/sauron-dashboard.conf    "$topdir/SOURCES/"
install -m0755 packaging/rpm/scripts/sauron-dashboard-config "$topdir/SOURCES/"

cp packaging/rpm/sauron.spec "$topdir/SPECS/sauron.spec"

if [ "${1:-}" = "--srpm" ]; then
    echo ">> Building source RPM only"
    rpmbuild -bs "$topdir/SPECS/sauron.spec"
else
    echo ">> Building source + binary RPMs (this compiles the Rust workspace — slow)"
    rpmbuild -ba "$topdir/SPECS/sauron.spec"
fi

echo ">> Done. Artifacts:"
find "$topdir/RPMS" "$topdir/SRPMS" -name "${name}*-${version}-*.rpm" 2>/dev/null | sort
```

- [ ] **Step 2: Make it executable and lint**

Run:
```bash
chmod 0755 packaging/rpm/build-rpm.sh
bash -n packaging/rpm/build-rpm.sh && echo "syntax ok"
command -v shellcheck >/dev/null && shellcheck packaging/rpm/build-rpm.sh || echo "shellcheck not installed — skipping"
```
Expected: `syntax ok`; shellcheck (if present) reports no errors.

- [ ] **Step 3: Dry-run the SRPM path (fast, no compile)**

Run:
```bash
./packaging/rpm/build-rpm.sh --srpm
```
Expected: prints the staging steps and finishes with a path to `~/rpmbuild/SRPMS/sauron-0.1.0-1*.src.rpm`. If `rpmbuild -bs` complains a `SourceN` file is missing, fix the corresponding `install` line's destination name to match the spec.

- [ ] **Step 4: Checkpoint (stage; hold commit)**

```bash
git add packaging/rpm/build-rpm.sh
# Do NOT commit.
```

---

### Task 6: Build and verify the RPMs

This is the real end-to-end verification. It compiles the full Rust workspace (including bundled DuckDB) and the dashboard — expect several minutes and significant memory. Iterate on `BuildRequires` if the build reports a missing tool.

**Files:** none created; may **Modify** `packaging/rpm/sauron.spec` (BuildRequires / %files fixes discovered here).

- [ ] **Step 1: Ensure INSTALL.md and SETUP.md exist**

The spec's `%files server` lists `%doc packaging/rpm/INSTALL.md packaging/rpm/SETUP.md`. If Task 7 has not run yet, create placeholders so the build succeeds, then let Task 7 fill them:
```bash
[ -f packaging/rpm/INSTALL.md ] || echo "# Sauron RPM — Install (see plan Task 7)" > packaging/rpm/INSTALL.md
[ -f packaging/rpm/SETUP.md ]   || echo "# Sauron RPM — Setup (see plan Task 7)"   > packaging/rpm/SETUP.md
```
Expected: both files present. (Preferred: run Task 7 before this task; then this step is a no-op.)

- [ ] **Step 2: Full build**

Run:
```bash
./packaging/rpm/build-rpm.sh 2>&1 | tee /tmp/sauron-rpmbuild.log | tail -40
```
Expected: ends with `Wrote: .../sauron-0.1.0-1*.rpm` lines and the script's `>> Done.` list showing four binary RPMs (`sauron`, `sauron-server`, `sauron-dashboard`, `sauron-cli`) plus the SRPM.

- [ ] **Step 3: If the build fails on a missing build tool, add it to BuildRequires and re-run**

Diagnose:
```bash
grep -iE 'error|not found|command not found|could not find|No such file' /tmp/sauron-rpmbuild.log | head
```
Then edit `packaging/rpm/sauron.spec` `BuildRequires:` (e.g. add `nasm`, `libstdc++-static`, or `python3` if the DuckDB/aws-lc build asks for them; install with `sudo dnf install <pkg>` and add the line), and re-run Step 2. Repeat until it builds. Record the final list.

- [ ] **Step 4: Inspect the produced file manifests**

Run:
```bash
v=0.1.0
for p in sauron sauron-server sauron-dashboard sauron-cli; do
  f=$(ls ~/rpmbuild/RPMS/*/${p}-${v}-*.rpm 2>/dev/null | head -1)
  echo "===== $f ====="; rpm -qlp "$f"
done
```
Expected (spot-check):
- `sauron` → `/etc/sauron/sauron.env`, `/usr/lib/sysusers.d/sauron.conf`, `/usr/lib/tmpfiles.d/sauron.conf`, `/var/lib/sauron`, `/var/lib/sauron/cold`, license.
- `sauron-server` → the five `/usr/bin/sauron-*` binaries, five `/usr/lib/systemd/system/sauron-*.service`, the four service `.env`, the ghost `secret.env`, INSTALL/SETUP docs.
- `sauron-dashboard` → `/usr/share/sauron/dashboard/index.html` + `assets/…` + `config.template.js`, ghost `config.js`, `/etc/nginx/conf.d/sauron-dashboard.conf`, `/usr/libexec/sauron/sauron-dashboard-config`, `/etc/sauron/dashboard.env`.
- `sauron-cli` → `/usr/bin/crebain`, `/usr/bin/sauron-symcli`.

If a file is missing or in the wrong package, fix `%install`/`%files` and re-run Steps 2 & 4.

- [ ] **Step 5: Verify dependencies are lean**

Run:
```bash
f=$(ls ~/rpmbuild/RPMS/*/sauron-server-0.1.0-*.rpm | head -1)
rpm -qp --requires "$f" | grep -iE 'libpq|openssl|duckdb' && echo "UNEXPECTED heavy dep" || echo "no libpq/openssl/duckdb — good"
rpm -qp --recommends "$f"
```
Expected: `no libpq/openssl/duckdb — good`; recommends lists `postgresql-server` and `valkey`.

- [ ] **Step 6: Install sanity check in a throwaway root (rpm test-install)**

Run (non-destructive — `--test` and a scratch root; no services touched):
```bash
mapfile -t rpms < <(ls ~/rpmbuild/RPMS/*/{sauron,sauron-server,sauron-dashboard,sauron-cli}-0.1.0-*.rpm)
rpm -Uvh --test "${rpms[@]}" && echo "dependency resolution OK (test install)"
```
Expected: `dependency resolution OK` — no unmet deps beyond `nginx` (install `nginx` first if the dashboard subpackage reports it). If nginx is missing and you don't want it, test the three non-dashboard RPMs.

- [ ] **Step 7: (Optional, if a disposable VM/container is available) real install smoke test**

On a throwaway Fedora system only:
```bash
sudo dnf install -y ./sauron-*.rpm
id sauron                              # user exists
systemctl cat sauron-api >/dev/null && echo "unit registered"
test -s /etc/sauron/secret.env && echo "secret generated"
sudo -u sauron sauron-migrate --help 2>&1 | head -1 || true
```
Expected: user exists, unit registered, secret present. **Do not** run this on the developer workstation — it creates a system user and drops files under `/etc`, `/usr`, `/var`.

- [ ] **Step 8: Checkpoint (stage any spec fixes; hold commit)**

```bash
git add packaging/rpm/sauron.spec
# Do NOT commit.
```

---

### Task 7: Install & Setup documentation

**Files:**
- Create: `packaging/rpm/INSTALL.md`
- Create: `packaging/rpm/SETUP.md`

**Interfaces:**
- Consumes: package names, file paths, unit names, and env keys defined in Tasks 1–5. These docs are also shipped as `%doc` in `sauron-server` (Task 4).

- [ ] **Step 1: Write `packaging/rpm/INSTALL.md`**

````markdown
# Sauron — Installing from RPM (Fedora / RHEL)

Sauron ships four RPMs from one spec:

| Package | Contents |
|---|---|
| `sauron` | shared `sauron` user, `/var/lib/sauron`, `/etc/sauron/sauron.env` (pulled in automatically) |
| `sauron-server` | API, ingest, monitor, tier, migrate binaries + systemd units |
| `sauron-dashboard` | static web UI + nginx vhost (requires `nginx`) |
| `sauron-cli` | `crebain` load generator, `sauron-symcli` |

## 1. Build the RPMs

Requires the build toolchain (`sudo dnf install rust cargo gcc gcc-c++ cmake clang nodejs npm rpm-build systemd-rpm-macros`):

```bash
git clone <repo> sauron && cd sauron
./packaging/rpm/build-rpm.sh
```

Artifacts land in `~/rpmbuild/RPMS/<arch>/` and `~/rpmbuild/SRPMS/`. The first build
compiles the Rust workspace (including a bundled DuckDB) and the dashboard — expect
several minutes. Use `./packaging/rpm/build-rpm.sh --srpm` to produce just the source RPM.

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
````

- [ ] **Step 2: Write `packaging/rpm/SETUP.md`**

````markdown
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

Per-service tunables live in `/etc/sauron/{api,ingest,monitor,tier}.env` — the
defaults are fine to start.

## 4. JWT secret

`/etc/sauron/secret.env` is generated with a random `JWT_SECRET` on first install.
Verify it exists:

```bash
sudo test -s /etc/sauron/secret.env && echo "JWT secret present"
```

To rotate it (invalidates existing sessions):

```bash
sudo sh -c 'printf "JWT_SECRET=%s\n" "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d " \n")" > /etc/sauron/secret.env'
sudo chgrp sauron /etc/sauron/secret.env && sudo chmod 0640 /etc/sauron/secret.env
sudo systemctl restart sauron-api
```

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
curl -fsS http://localhost/config.js                          # dashboard runtime config
journalctl -u sauron-api -u sauron-ingest --no-pager | tail
```

(If a service exposes a different health path, check `journalctl` for the bound
address logged at startup.)

## 10. Troubleshooting

| Symptom | Check |
|---|---|
| Service fails immediately | `journalctl -u sauron-<svc> -e` — usually `DATABASE_URL` wrong/unreachable |
| `DATABASE_URL is required` | `/etc/sauron/sauron.env` not set or unreadable by the `sauron` user |
| API 401 / login broken | `secret.env` missing or changed since sessions issued — rotate & restart |
| Dashboard shows wrong API URL | edit `/etc/sauron/dashboard.env`, re-run `sauron-dashboard-config`, reload nginx, hard-refresh |
| Ingest 429 | raise `INGEST_RATE_LIMIT_PER_MIN` in `/etc/sauron/ingest.env`, restart |
| Tier can't write cold | confirm `/var/lib/sauron/cold` is owned by `sauron` (see `tmpfiles`) |
````

- [ ] **Step 3: Verify the docs render and links resolve**

Run:
```bash
for f in packaging/rpm/INSTALL.md packaging/rpm/SETUP.md; do
  echo "--- $f ---"; grep -c '^#' "$f"
done
# sanity: no leftover template tokens
! grep -rnE 'TBD|TODO|FIXME|\$\{[A-Z_]+\}' packaging/rpm/INSTALL.md packaging/rpm/SETUP.md && echo "no placeholders"
```
Expected: both have multiple headings; `no placeholders` prints. (`${...}` only appears intentionally inside fenced examples referencing dashboard.env — confirm any hit is inside a code fence; the grep above should return none outside them since the examples use concrete values.)

- [ ] **Step 4: Checkpoint (stage; hold commit)**

```bash
git add packaging/rpm/INSTALL.md packaging/rpm/SETUP.md
# Do NOT commit.
```

---

### Task 8: README section + wiki page

**Files:**
- Modify: `README.md` (add "Install via RPM" section after "Quick start (Docker Compose)")
- Create: `wiki/RPM-Install.md`
- Modify: `wiki/_Sidebar.md` (add link)

**Interfaces:**
- Consumes: `packaging/rpm/INSTALL.md` + `SETUP.md` (Task 7) — the README/wiki point at them rather than duplicating.

- [ ] **Step 1: Add the README section**

In `README.md`, immediately after the "## Quick start (Docker Compose)" block (before "## Local development"), insert:

```markdown
## Install via RPM (Fedora / RHEL)

For a Docker-less deployment on Fedora/RHEL-family systems, Sauron ships as four
RPMs (`sauron`, `sauron-server`, `sauron-dashboard`, `sauron-cli`) driven by
systemd. Postgres and Redis/Valkey are external.

```bash
./packaging/rpm/build-rpm.sh                 # build the RPMs (needs rust, cargo, node, rpm-build)
sudo dnf install ~/rpmbuild/RPMS/$(uname -m)/sauron-*.rpm
```

Full instructions: **[packaging/rpm/INSTALL.md](packaging/rpm/INSTALL.md)** (build & install)
and **[packaging/rpm/SETUP.md](packaging/rpm/SETUP.md)** (configure DB/Redis, migrate,
enable services, dashboard).
```

- [ ] **Step 2: Create `wiki/RPM-Install.md`**

```markdown
# Install via RPM (Fedora / RHEL)

Sauron can be deployed without Docker on Fedora/RHEL-family systems using native
RPMs and systemd. The canonical, always-current instructions live in the repo and
are shipped inside `sauron-server` under `/usr/share/doc/sauron-server/`:

- **Build & install:** [`packaging/rpm/INSTALL.md`](https://github.com/splimter/sauron/blob/main/packaging/rpm/INSTALL.md)
- **Setup & operations:** [`packaging/rpm/SETUP.md`](https://github.com/splimter/sauron/blob/main/packaging/rpm/SETUP.md)

## At a glance

| Package | Role |
|---|---|
| `sauron` | shared user, data dir, `/etc/sauron/sauron.env` |
| `sauron-server` | API (:8080), ingest (:8081), monitor, tier, migrate + systemd units |
| `sauron-dashboard` | static SPA + nginx vhost |
| `sauron-cli` | `crebain`, `sauron-symcli` |

```bash
./packaging/rpm/build-rpm.sh
sudo dnf install ~/rpmbuild/RPMS/$(uname -m)/sauron-*.rpm
sudo systemctl start sauron-migrate
sudo systemctl enable --now sauron-api sauron-ingest sauron-monitor sauron-tier
```

Requires an external PostgreSQL and Redis/Valkey — see SETUP.md.
```

- [ ] **Step 3: Link it from the wiki sidebar**

Read `wiki/_Sidebar.md`, then add (following the existing bullet style, near Getting-Started) a line:

```markdown
- [Install via RPM](RPM-Install.md)
```

- [ ] **Step 4: Verify links and section placement**

Run:
```bash
grep -n "Install via RPM" README.md wiki/_Sidebar.md
grep -n "packaging/rpm/INSTALL.md\|packaging/rpm/SETUP.md" README.md wiki/RPM-Install.md
test -f packaging/rpm/INSTALL.md && test -f packaging/rpm/SETUP.md && echo "targets exist"
```
Expected: matches in README + sidebar; both doc links present; `targets exist`.

- [ ] **Step 5: Checkpoint (stage; hold commit)**

```bash
git add README.md wiki/RPM-Install.md wiki/_Sidebar.md
# Do NOT commit.
```

---

## Self-Review

**Spec coverage** (design §→task):
- §4 four subpackages → Task 4 spec (`%package server/dashboard/cli` + base). ✅
- §5 FHS layout → Task 4 `%install`/`%files`; verified in Task 6 Step 4. ✅
- §6 build-from-source (cargo + npm, BuildRequires) → Task 4 `%build`, Task 6 iterates BuildRequires. ✅
- §7 systemd units (hardening, oneshot migrate, no auto-start) → Task 2 + `%systemd_post/preun/postun` in Task 4. ✅
- §8 JWT secret auto-gen → Task 4 `%post server`; rotation in Task 7 SETUP §4. ✅
- §9 dashboard config generation → Task 3 generator + Task 4 `%post dashboard`. ✅
- §10 docs (INSTALL, SETUP, build-rpm.sh, README, wiki) → Tasks 5, 7, 8. ✅
- §11 repo layout → Tasks 1–3 file placement. ✅
- §12 verification (rpmlint, -bs, -bb, -qlp, install) → Task 4 Step 3, Task 5 Step 3, Task 6. ✅
- §13 risks (build time, BuildRequires, valkey/redis, nginx dep) → Task 6 Step 3, Task 7 SETUP. ✅

**Placeholder scan:** Task 6 Step 1 intentionally creates one-line INSTALL/SETUP stubs *only if* Task 7 hasn't run — the real content is in Task 7; recommended order runs Task 7 before Task 6, making the stub a no-op. No other placeholders; all file bodies are complete.

**Type/name consistency:** unit names (`sauron-{api,ingest,monitor,tier,migrate}.service`), binary names (`sauron-*`, `crebain`, `sauron-symcli`), env keys (match `config.rs`), `SourceN` destination filenames (`sauron.sysusers`/`sauron.tmpfiles` vs the `sauron.conf` in the tree — build-rpm.sh renames them; spec references the renamed forms), and paths (`/usr/libexec/sauron/sauron-dashboard-config`, `/usr/share/sauron/dashboard`) are consistent across Tasks 1–8.

---

## Execution Handoff

Two execution options:

1. **Subagent-Driven (recommended)** — a fresh subagent per task with review between tasks.
2. **Inline Execution** — batch execution in this session with checkpoints.
