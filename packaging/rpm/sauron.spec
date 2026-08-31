# Rust release binaries; skip the debuginfo subpackage for this build.
%global debug_package %{nil}

# Prebuilt mode (`rpmbuild --with prebuilt`, driven by build-rpm.sh --prebuilt):
# %%build is skipped and %%install consumes binaries + dashboard/dist staged into
# the source tree by CI, so packaging costs seconds instead of recompiling.
# (%% escapes the section names so el9's rpm 4.16 doesn't expand them in this comment.)
%bcond_with prebuilt

Name:           sauron
Version:        1.8.0
Release:        1%{?dist}
Summary:        Unified error reporting and product analytics platform

License:        LGPL-3.0-only
URL:            https://github.com/splimter/sauron
Source0:        %{name}-%{version}.tar.gz

# Auxiliary sources staged into SOURCES by packaging/rpm/build-rpm.sh
Source10:       sauron-api.service
Source11:       sauron-ingest.service
Source12:       sauron-monitor.service
Source13:       sauron-tier.service
Source14:       sauron-migrate.service
Source15:       sauron-alerts.service
Source16:       sauron-inspector.service
Source17:       sauron-storesync.service
Source20:       sauron.sysusers
Source21:       sauron.tmpfiles
Source30:       sauron.env
Source31:       api.env
Source32:       ingest.env
Source33:       monitor.env
Source34:       tier.env
Source35:       dashboard.env
Source36:       alerts.env
Source37:       inspector.env
Source38:       storesync.env
Source40:       sauron-dashboard.conf
Source41:       sauron-dashboard-config
# Prebuilt libduckdb.so (DuckDB C library) matching the libduckdb-sys crate pin,
# staged into SOURCES by packaging/rpm/build-rpm.sh via fetch-libduckdb.sh. The
# workspace links this instead of compiling the DuckDB C++ amalgamation.
Source50:       libduckdb.so
%if %{with prebuilt}
# Overlay tarball of precompiled binaries (backend/target/release/*) and dashboard
# static assets (dashboard/dist/*), staged by build-rpm.sh --prebuilt and unpacked
# in %%prep so %%build is a no-op. Present only in prebuilt mode.
Source51:       sauron-prebuilt.tar.gz
%endif

BuildRequires:  cargo >= 1.82
BuildRequires:  rust >= 1.82
# Only a C compiler + perl remain (ring's C/asm). DuckDB is linked prebuilt (no
# C++ amalgamation), reqwest uses the ring TLS backend (no aws-lc/cmake/clang), and
# zstd links the system library via pkgconfig(libzstd) — so gcc-c++, cmake and
# clang are no longer needed.
BuildRequires:  gcc
BuildRequires:  perl-interpreter
BuildRequires:  pkgconfig(libzstd)
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
%if %{with prebuilt}
# Lay precompiled binaries + dashboard/dist into the tree so %%build is a no-op and
# %%install finds artifacts at the same paths as a from-source build.
tar xzf %{SOURCE51}
%endif

%build
%if %{without prebuilt}
# Dashboard SPA and the Rust workspace are independent — overlap them so the
# npm build hides under the (longer) cargo compile.
(cd dashboard && npm ci && npm run build) &
dashboard_build=$!

# Link DuckDB against the prebuilt libduckdb (Source50) rather than compiling the
# C++ amalgamation from source — the single slowest item in the workspace build.
mkdir -p _libduckdb
cp -p %{SOURCE50} _libduckdb/libduckdb.so
export DUCKDB_LIB_DIR="$PWD/_libduckdb"

# redhat-rpm-config injects RUSTFLAGS with -Cdebuginfo=2 -Cstrip=none: that
# generates debuginfo we discard (debug_package is %%{nil}) and defeats the
# release `strip`. Append last-wins overrides to undo both while keeping the
# hardening/link flags redhat-rpm-config also sets.
#
# `-Ccodegen-units=16` used to be appended here too, for build speed. It is not
# any more: RUSTFLAGS wins over the profile, so it silently overrode
# `codegen-units = 1` in backend/Cargo.toml and shipped RPM binaries that were
# optimized differently from every binary the project benchmarks. Restoring it
# would reintroduce that divergence, not just speed the build up.
export RUSTFLAGS="${RUSTFLAGS:-} -Cdebuginfo=0 -Cstrip=symbols"

(cd backend && cargo build --release --workspace)

wait "$dashboard_build"
%else
# Prebuilt mode: binaries (backend/target/release) and dashboard/dist were staged
# into the source tree by build-rpm.sh --prebuilt. Nothing to compile.
:
%endif

%install
# --- binaries ---
# packaging/rpm/binaries.txt is the single source of truth for what ships; CI's
# prebuilt assemble step and build-rpm.sh read the same file, so the lists can't
# drift apart (a binary in the spec but absent from the prebuilt overlay used to
# fail here with "install: cannot stat"). Which subpackage owns each one is still
# declared in %%files below, and rpm errors on installed-but-unpackaged files.
for b in $(grep -vE '^[[:space:]]*(#|$)' packaging/rpm/binaries.txt); do
    install -Dm0755 backend/target/release/$b %{buildroot}%{_bindir}/$b
done

# --- systemd units ---
install -Dm0644 %{SOURCE10} %{buildroot}%{_unitdir}/sauron-api.service
install -Dm0644 %{SOURCE11} %{buildroot}%{_unitdir}/sauron-ingest.service
install -Dm0644 %{SOURCE12} %{buildroot}%{_unitdir}/sauron-monitor.service
install -Dm0644 %{SOURCE13} %{buildroot}%{_unitdir}/sauron-tier.service
install -Dm0644 %{SOURCE14} %{buildroot}%{_unitdir}/sauron-migrate.service
install -Dm0644 %{SOURCE15} %{buildroot}%{_unitdir}/sauron-alerts.service
install -Dm0644 %{SOURCE16} %{buildroot}%{_unitdir}/sauron-inspector.service
install -Dm0644 %{SOURCE17} %{buildroot}%{_unitdir}/sauron-storesync.service

# --- systemd preset ---
# Installed from the unpacked source tree rather than as a SourceN, on purpose:
# packaging/rpm/build-rpm.sh stages every SourceN into SOURCES by hand, so a new
# Source line there and no matching line here fails the build with "cannot open".
# packaging/ is already inside the Source0 tarball (same reason
# packaging/rpm/binaries.txt is readable in the %%install loop above), so this
# works in both from-source and --prebuilt mode with no change outside the spec.
install -Dm0644 packaging/rpm/systemd/50-sauron.preset \
    %{buildroot}%{_presetdir}/50-sauron.preset

# --- sysusers / tmpfiles ---
install -Dm0644 %{SOURCE20} %{buildroot}%{_sysusersdir}/sauron.conf
install -Dm0644 %{SOURCE21} %{buildroot}%{_tmpfilesdir}/sauron.conf

# --- config ---
install -Dm0640 %{SOURCE30} %{buildroot}%{_sysconfdir}/sauron/sauron.env
install -Dm0640 %{SOURCE31} %{buildroot}%{_sysconfdir}/sauron/api.env
install -Dm0640 %{SOURCE32} %{buildroot}%{_sysconfdir}/sauron/ingest.env
install -Dm0640 %{SOURCE33} %{buildroot}%{_sysconfdir}/sauron/monitor.env
install -Dm0640 %{SOURCE34} %{buildroot}%{_sysconfdir}/sauron/tier.env
install -Dm0640 %{SOURCE36} %{buildroot}%{_sysconfdir}/sauron/alerts.env
install -Dm0640 %{SOURCE37} %{buildroot}%{_sysconfdir}/sauron/inspector.env
install -Dm0640 %{SOURCE38} %{buildroot}%{_sysconfdir}/sauron/storesync.env
install -Dm0644 %{SOURCE35} %{buildroot}%{_sysconfdir}/sauron/dashboard.env

# --- data dirs (also created at runtime by tmpfiles) ---
install -dm0750 %{buildroot}%{_sharedstatedir}/sauron
install -dm0750 %{buildroot}%{_sharedstatedir}/sauron/cold

# --- vendored libduckdb (dynamically linked by sauron-tier) ---
# Shipped in a private lib dir + an ld.so.conf.d drop-in so the loader resolves
# it (ldconfig runs in %%post server). No rpath is baked into the binary.
install -Dm0755 %{SOURCE50} %{buildroot}%{_libdir}/sauron/libduckdb.so
install -dm0755 %{buildroot}%{_sysconfdir}/ld.so.conf.d
printf '%s\n' '%{_libdir}/sauron' > %{buildroot}%{_sysconfdir}/ld.so.conf.d/sauron.conf

# --- dashboard static + generator + nginx vhost ---
mkdir -p %{buildroot}%{_datadir}/sauron/dashboard
cp -a dashboard/dist/. %{buildroot}%{_datadir}/sauron/dashboard/
# config.js is generated per-host by %%post; ship only the template.
rm -f %{buildroot}%{_datadir}/sauron/dashboard/config.js
install -Dm0644 %{SOURCE40} %{buildroot}%{_sysconfdir}/nginx/conf.d/sauron-dashboard.conf
install -Dm0755 %{SOURCE41} %{buildroot}%{_libexecdir}/sauron/sauron-dashboard-config

%pre
%sysusers_create_compat %{SOURCE20}

%post
%tmpfiles_create %{_tmpfilesdir}/sauron.conf

%post server
# On first install only, this runs `systemctl preset` over the listed units, and
# the answer comes from %%{_presetdir}/50-sauron.preset shipped by this package:
# the five daemons enabled, sauron-inspector explicitly disabled (opt-in).
#
# sauron-migrate.service stays in this list even though it is static (no
# [Install] section): preset on a static unit is a documented no-op, rc=0, and
# is-enabled keeps reporting "static". Listing it costs nothing and keeps the
# %%post/%%preun lists identical to %%files. It is deliberately NOT in the preset
# file — it is pulled in by every daemon's Requires=sauron-migrate.service.
%systemd_post sauron-api.service sauron-ingest.service sauron-monitor.service sauron-alerts.service sauron-tier.service sauron-inspector.service sauron-storesync.service sauron-migrate.service
# Refresh the dynamic linker cache so sauron-tier finds the vendored
# %%{_libdir}/sauron/libduckdb.so via the ld.so.conf.d drop-in.
/sbin/ldconfig
# Generate a JWT secret on first install if none present.
if [ "$1" -eq 1 ] && [ ! -s %{_sysconfdir}/sauron/secret.env ]; then
    umask 027
    printf 'JWT_SECRET=%s\n' "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" > %{_sysconfdir}/sauron/secret.env
    printf 'NOTIFY_SECRET_KEY=%s\n' "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" >> %{_sysconfdir}/sauron/secret.env
    chgrp sauron %{_sysconfdir}/sauron/secret.env 2>/dev/null || :
    chmod 0640 %{_sysconfdir}/sauron/secret.env
fi
# UPGRADES from a build that predates the fail-closed notification key.
#
# api, monitor and alerts now refuse to start without NOTIFY_SECRET_KEY, so an
# upgraded host would come back with three dead units. Backfill it with the
# EXISTING JWT_SECRET, not a fresh random value: older builds derived the
# channel cipher from JWT_SECRET when the dedicated key was unset, so that is
# the key every stored channel secret was actually encrypted under. Generating
# a new one here would boot cleanly and then fail every delivery with
# "secret decrypt failed" — silent, total loss of every configured channel.
#
# The value is captured and TESTED before anything is written. An operator who
# moved JWT_SECRET into api.env leaves this file with no JWT_SECRET= line, and
# the unguarded version appended a literal `NOTIFY_SECRET_KEY=` with an empty
# right-hand side. sauron-core's var() filters empty strings, so
# require_notify_secret_key() then took the None branch and api, monitor and
# alerts all refused to start — the exact three-dead-units outcome this backfill
# exists to prevent. It was also self-masking: the `! grep -q` guard below
# matches an empty assignment, so re-running %%post could never repair it.
#
# When the source value is missing we deliberately write NOTHING and say so.
# Generating a fresh random key here would be worse than failing: older builds
# derived the channel cipher from JWT_SECRET, so a new key boots cleanly and
# then fails every delivery with "secret decrypt failed" — silent, total loss of
# every configured channel.
if [ -s %{_sysconfdir}/sauron/secret.env ] && \
   ! grep -q '^NOTIFY_SECRET_KEY=' %{_sysconfdir}/sauron/secret.env; then
    sauron_jwt=$(sed -n 's/^JWT_SECRET=//p' %{_sysconfdir}/sauron/secret.env | head -n 1)
    if [ -n "$sauron_jwt" ]; then
        umask 027
        printf 'NOTIFY_SECRET_KEY=%s\n' "$sauron_jwt" >> %{_sysconfdir}/sauron/secret.env
        chgrp sauron %{_sysconfdir}/sauron/secret.env 2>/dev/null || :
        chmod 0640 %{_sysconfdir}/sauron/secret.env
    else
        echo "sauron: WARNING no JWT_SECRET= in %{_sysconfdir}/sauron/secret.env, so" >&2
        echo "sauron: NOTIFY_SECRET_KEY could not be backfilled. sauron-api, sauron-monitor" >&2
        echo "sauron: and sauron-alerts will refuse to start until you add it. Use the SAME" >&2
        echo "sauron: value older builds encrypted channel secrets under (your previous" >&2
        echo "sauron: JWT_SECRET) - a fresh random key makes every stored channel secret" >&2
        echo "sauron: permanently unreadable. See SETUP.md section 6." >&2
    fi
    unset sauron_jwt
fi

%preun server
# Erase only ($1 -eq 0): `disable --now` each unit. sauron-migrate is listed for
# symmetry with %%post/%%files; on a static unit disable is a no-op (rc=0), and
# because it is inactive between runs the --now half is a no-op too.
%systemd_preun sauron-api.service sauron-ingest.service sauron-monitor.service sauron-alerts.service sauron-tier.service sauron-inspector.service sauron-storesync.service sauron-migrate.service

%postun server
# sauron-migrate.service is DELIBERATELY ABSENT from this list. Do not "fix" it
# by adding it — that was tested and it is provably a no-op.
#
# On both Fedora and RHEL 9 this macro no longer runs `systemctl try-restart`.
# It expands to `systemd-update-helper mark-restart-system-units`, i.e.
# `systemctl set-property <unit> Markers=+needs-restart`, and the transaction
# file trigger later runs `systemctl reload-or-restart --marked` — try-restart
# semantics. sauron-migrate has no RemainAfterExit, so between runs it sits in
# ActiveState=inactive and the marker matches nothing: emulating the full
# upgrade sequence with migrate marked produced ZERO ExecStart runs on
# AlmaLinux 9 (systemd 252) and Fedora 41 (systemd 256) alike.
#
# Migrations on upgrade are handled instead by Requires=sauron-migrate.service
# in each of the six daemon units: this macro restarts them, and each restart
# pulls the migrator in, ordered ahead of the daemon by After=. Verified end to
# end. Adding migrate here would only work if it also gained
# RemainAfterExit=yes, and that variant is rejected because it would then run
# roughly once per boot instead of once per daemon start — see the comment in
# sauron-migrate.service.
%systemd_postun_with_restart sauron-api.service sauron-ingest.service sauron-monitor.service sauron-alerts.service sauron-tier.service sauron-inspector.service sauron-storesync.service
# Rebuild the linker cache after the vendored libduckdb is added/removed.
/sbin/ldconfig

%post dashboard
%{_libexecdir}/sauron/sauron-dashboard-config || :

%files
%license LICENSE COPYING
%doc README.md
%dir %{_sysconfdir}/sauron
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/sauron.env
%{_sysusersdir}/sauron.conf
%{_tmpfilesdir}/sauron.conf
%attr(0750,sauron,sauron) %dir %{_sharedstatedir}/sauron
%attr(0750,sauron,sauron) %dir %{_sharedstatedir}/sauron/cold

%files server
%doc packaging/rpm/INSTALL.md packaging/rpm/SETUP.md packaging/rpm/post-upgrade.sh
%{_bindir}/sauron-api
%{_bindir}/sauron-ingest
%{_bindir}/sauron-monitor
%{_bindir}/sauron-alerts
%{_bindir}/sauron-tier
%{_bindir}/sauron-inspector
%{_bindir}/sauron-storesync
%{_bindir}/sauron-migrate
%{_unitdir}/sauron-api.service
%{_unitdir}/sauron-ingest.service
%{_unitdir}/sauron-monitor.service
%{_unitdir}/sauron-alerts.service
%{_unitdir}/sauron-tier.service
%{_unitdir}/sauron-inspector.service
%{_unitdir}/sauron-storesync.service
%{_unitdir}/sauron-migrate.service
# Vendor preset: which daemons `systemctl preset` enables on first install.
# Not %%config — operator overrides belong in /etc/systemd/system-preset/.
%{_presetdir}/50-sauron.preset
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/api.env
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/ingest.env
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/monitor.env
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/tier.env
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/alerts.env
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/inspector.env
%attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/storesync.env
%ghost %attr(0640,root,sauron) %config(noreplace) %{_sysconfdir}/sauron/secret.env
# Vendored DuckDB C library (linked by sauron-tier) + loader path.
%dir %{_libdir}/sauron
%{_libdir}/sauron/libduckdb.so
%config %{_sysconfdir}/ld.so.conf.d/sauron.conf

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
* Mon Aug 31 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.8.0-1
- Relicensed from AGPL-3.0-only to LGPL-3.0-only. LGPLv3 is a set of additional
  permissions on top of GPLv3, so the package now ships both texts: LICENSE
  (LGPLv3) and COPYING (GPLv3). The AGPL network-use clause no longer applies —
  running a modified Sauron as a hosted service does not by itself oblige an
  operator to offer users the corresponding source.
* Wed Aug 19 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.7.0-1
- "Crash-free sessions" is renamed "Unhandled-exception-free sessions" and now
  means what the new name says: it counts only errors the SDK
  reported as UNCAUGHT (`mechanism.handled = false`), instead of every row in
  `error_events` at any level. Previously a single handled, caught,
  warning-level exception marked a whole session "crashed" (migration 000069).
- The rate is now omitted rather than guessed when it cannot be measured. An SDK
  that never reports handledness produces zero crashes by construction, which is
  indistinguishable from a healthy app; the Overview tile shows "no crash data"
  instead of a confident 100%. Node, Python and C# ship uncaught-error capture
  OFF by default (`autoCaptureUnhandled`), so this is the common case.
- Overview cache key bumped v1 -> v2: cached payloads carry the previous
  meaning, and would otherwise be served for up to 24 h after upgrade.
- Environment-scoped members can reach the Monitor/Explore/Analyze pages again.
  The page gate could not express an environment-level grant at all, so a member
  scoped to one environment reached none of the 28 gated pages and saw an empty
  sidebar, despite the API supporting exactly that read.
- Source Maps is gated on the permission its list endpoint actually needs
  (`issue:read`), so a member who may read the artifact list is no longer shown
  a full-page denial; the upload and delete controls stay locked.
- Navigation locks instead of hiding: unreachable pages render disabled with the
  permission they require, rather than vanishing. Locked controls now stay
  keyboard-reachable and announce their reason (`aria-disabled` plus a tooltip
  on hover AND focus), where before they used a plain `disabled` that removed
  them from the tab order entirely.
* Sun Aug 16 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.6.0-1
- Developer-supplied `tags` and `extra` on performance transactions: attach a
  request body, a response body, an order id or a retry count to a span, from
  any of the five SDKs (migration 000063).
- New searched per-transaction list (`GET /v1/apps/{id}/transactions`) and a
  Transactions page, so individual spans are reachable and their `extra` is
  searchable — the aggregates under /performance group by operation and cannot
  show a single call.
- Transaction bodies are gated on `event:read`, with the free-text search reach
  narrowed from the same predicate so a `?q=` probe cannot read a column the
  response withholds.
* Fri Aug 14 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.5.0-1
- Admin data purge: delete signal data by app, environment and time range, then
  recompute the affected rollups (migration 000057).
- Guest-to-identified identity merge: a guest's history is folded into the
  person they identify as, instead of being stranded under the anonymous id
  (migration 000058).
- Device/environment rollup table backing the device-groups and persons
  environment-scoped queries, replacing the correlated scans over every event
  partition (migration 000059).
- Autovacuum tuning for the event tables and a transactions device/environment
  index (migrations 000060, 000061).

* Tue Aug 11 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.4.0-1
- App store install and uninstall metrics. Daily counts are pulled from Google
  Play (monthly CSV reports from the Play Console's Cloud Storage bucket) and
  the Apple App Store (the App Store Connect Analytics Reports API, which is the
  only Apple source that reports deletions), stored per app per store per day,
  and charted on Overview as diverging bars — installs above the zero line,
  uninstalls below, both stores stacked in each direction on one shared scale.
- NEW DAEMON: sauron-storesync, enabled by the vendor preset. It claims due
  connections FOR UPDATE SKIP LOCKED, fetches concurrently and upserts; a store
  outage is recorded on that one connection and touches nothing else. Tunables
  live in /etc/sauron/storesync.env (STORE_SYNC_INTERVAL_SECS, default 6h;
  STORE_SYNC_MAX_CONCURRENCY; STORE_BACKFILL_DAYS).
- Store credentials (the Play service-account JSON, the App Store Connect .p8)
  are encrypted at rest under the EXISTING NOTIFY_SECRET_KEY — one at-rest key
  for the deployment, not a second one to keep in step. sauron-storesync proves
  it can decrypt what is stored at boot and refuses to start otherwise, rather
  than reporting every connection as broken hours later.
- Migration 2026-08-10-000049_store_metrics adds app_store_connections,
  store_daily_metrics and a nullable apps.store_environment_id. Purely additive
  and safe to apply with the daemons running. As always on this platform, an RPM
  upgrade does not re-run sauron-migrate by itself — the daemon units pull it in
  via Requires=, so a restart applies it.
- Apple publishes nothing for roughly 24-48 hours after its ongoing report is
  first requested. That window is surfaced as a "pending" state in App settings,
  deliberately not as an error.
- FIX: the API's CORS layer did not advertise PUT, so the first PUT route (the
  store-connection upsert) failed in browsers with net::ERR_FAILED while the
  preflight still answered 200. No route shipped before this release used PUT.

* Sun Aug 09 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.3.0-2
- SECURITY (migration 2026-08-09-000046_channel_config_enc): a notification
  channel's `config` was stored in CLEARTEXT in Postgres, and therefore in every
  base backup and every WAL archive. For the generic webhook kind that blob holds
  the target URL and an arbitrary `headers` map, so a configured
  `Authorization: Bearer ...` was on disk in the clear; for Slack and Discord the
  `webhook_url` in `config` IS the credential. It is now AES-256-GCM ciphertext in
  a new `notification_channels.config_enc`, under the same NOTIFY_SECRET_KEY that
  already protected `secret_enc`. Encrypting only the "sensitive leaves" was
  rejected: `headers` is an arbitrary map, so the sensitive set is not enumerable,
  and any per-kind allowlist drifts from the resolver — which is how the bug
  happened.
  UPGRADE ACTION: rotate the webhook URLs, bot tokens and request headers of every
  configured channel. The migration hides the plaintext going forward; it cannot
  un-leak what is already in your backups.
  The row conversion is NOT done by sauron-migrate — that binary has neither the
  cipher nor the key. It runs in Rust at the first sauron-api boot, is idempotent,
  and aborts startup rather than half-converting the table.
  DOWNGRADE: one-way. That first boot writes `config = '{}'` alongside
  `config_enc`, and an older binary knows nothing about `config_enc`, so it reads
  an empty config and every delivery silently goes NOWHERE, for every channel,
  with no error. `dnf downgrade` does not run down.sql, so the ciphertext survives
  — recovery is to roll forward (`dnf upgrade sauron-server`, restart sauron-api).
  NEVER `diesel migration revert` this migration: it drops the only remaining copy
  of every channel's destination and is UNRECOVERABLE, as its own down.sql says.
- Migrations now run automatically. Every sauron-* daemon unit gained
  Requires=sauron-migrate.service alongside the After= it already had; After=
  on its own is pure ordering and pulled nothing into the transaction, so
  `dnf upgrade` had never run a migration and new binaries met whatever schema
  was there before — scattered 500s, or a feature that silently does nothing.
  A daemon whose migration fails now does not start. That is deliberate.
  sauron-migrate keeps RemainAfterExit unset so it re-runs on every daemon start
  (a no-op run measured ~30 ms with all 46 migrations applied) and so that
  `systemctl stop sauron-migrate` stays a no-op instead of propagating a stop to
  all six daemons.
  AVAILABILITY TRADE: a Postgres outage longer than MIGRATE_WAIT_SECS fails the
  migrate start job, and systemd never retries a failed START job — the daemons
  stay down after the database returns and must be started by hand. Previously
  sauron-api crash-looped through such an outage and recovered on its own.
- New MIGRATE_WAIT_SECS (default 120) in /etc/sauron/sauron.env: how long
  sauron-migrate waits for Postgres to accept connections before giving up. Only
  the CONNECT is retried; a migration that fails on its own SQL fails immediately
  and loudly. sauron-migrate previously made exactly one connection attempt with
  no timeout at all, so an unreachable host hung indefinitely. The unit now also
  sets TimeoutStartSec=300 — Type=oneshot defaults to infinity, which with
  Requires= would stall multi-user.target at boot. Keep MIGRATE_WAIT_SECS below it.
  sauron.env is %config(noreplace), so on a host you have edited the new commented
  default lands in sauron.env.rpmnew and the knob is absent from your file; the
  compiled default of 120 still applies.
- New vendor preset 50-sauron.preset enables sauron-api, sauron-ingest,
  sauron-monitor, sauron-alerts and sauron-tier on first install. SETUP.md's
  enable list had omitted sauron-alerts, so on a doc-following install the alert
  evaluator was installed, configured and permanently dead: metric rules were
  creatable in the dashboard and never fired, and notification_queue was never
  drained, with nothing anywhere reporting it. sauron-inspector is explicitly
  disabled in the preset — the PII inspector stays opt-in.
  UPGRADE ACTION: preset only runs on first install, so an existing host must
  `sudo systemctl enable --now sauron-alerts` by hand.
- New migration 2026-08-08-000044_tier_policy_and_pins: `runtime_settings`
  (runtime-tunable cold rotation age, no restart) and `tier_pins` (ranges
  sauron-tier must not drop). Skipping it stops ALL tiering — `runtime_settings`
  is the first query in sauron-tier's cycle, so the cycle aborts before any export
  or drop, one WARN per tick and no other symptom, and the disk grows until it
  fills. Cheap to apply: two new tables, no lock on any hot table, nothing seeded.
- New migration 2026-08-09-000045_cold_restore: restore cold Parquet data back
  into the live tables from the dashboard. Requires 000044. Adds `restore_jobs`
  and a nullable `restored_pin_id` to error_events, analytics_events and
  transactions — bare ADD COLUMN with no DEFAULT is catalog-only on a partitioned
  parent, so there is no rewrite, no table scan and no index build, and it is safe
  to run with ingest live. Skipping it only breaks the restore endpoints.

* Tue Aug 04 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.3.0-1
- Ingest write throughput. The worker used to spend roughly ten Postgres
  transactions and a stack of sequential Redis round trips on every single
  event; it now batches the writes, folds the workflow bump into the same
  transaction, sends breadcrumbs and HLL updates as pipelines, and commits once
  per batch instead of once per item. Measured end to end on the same hardware,
  the write path drains several times faster than 1.2.1 at the same load.
- An envelope is one Redis stream entry, not one entry per item. The header is
  serialized, stored and parsed once rather than N times, and stream trimming
  counts whole envelopes. Under sustained overload — where the stream is deep
  enough for MAXLEN to trim — accepted items were being discarded silently;
  holding an envelope together removes that loss at twice rated capacity.
- Retuned ingest defaults: WORKER_CONCURRENCY is 8 (was 4) and a batch reads up
  to 200 stream entries (was 50). The two knobs interact, so they were swept
  together — raising the worker count alone made throughput worse. New
  INGEST_BATCH_ITEMS (default 1000) bounds a batch in items rather than
  entries, which is what actually governs memory now that one entry can carry a
  whole envelope. INGEST_DB_POOL must stay >= WORKER_CONCURRENCY.
- The shipped ingest.env no longer pins WORKER_CONCURRENCY, so the retuned
  default above actually takes effect. It had stayed pinned at 4 in
  /etc/sauron/ingest.env, which outranks the binary's default — the entry above
  announced 8 while every RPM install kept running 4. Nothing logs the effective
  worker count, so this was invisible on a running host.
  UPGRADE ACTION: ingest.env is %config(noreplace). A host whose operator ever
  edited it keeps their file verbatim, stale WORKER_CONCURRENCY=4 included, and
  gets the new one beside it as ingest.env.rpmnew — remove the line by hand.
  Hosts that never touched the file pick up the change automatically. See
  SETUP.md section 11.
- RPM binaries are built with the optimization settings the project benchmarks.
  The spec appended -Ccodegen-units=16 to RUSTFLAGS for build speed, and
  RUSTFLAGS wins over the profile, so it silently overrode codegen-units = 1
  from backend/Cargo.toml — every shipped RPM was compiled differently from
  every binary that had been measured.

* Sun Aug 02 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.2.1-1
- Privacy policies can be created from the dashboard. The Privacy page's Policy
  tab now carries a create form (scope, target, tracked keys, detectors); it
  previously pointed at an organization settings screen that does not exist, so
  no role — including Owner — could create a policy from the UI at all. The API
  and permissions were already correct; only the UI was missing.
- Chart bars label themselves on hover: the count sits above the bar and the
  date beneath the axis, replacing a pair of overlapping tooltips that showed
  the same two values twice.

* Sat Aug 01 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.2.0-1
- Session management: a login now has an identity that survives refresh-token
  rotation. Users can see and end their own sessions from the new Account page;
  admins with the new member:credential permission can sign a member out of every
  device from Members.
- Revoking anything (logout, sign-out, deactivation, password change, replay
  detection) now takes effect within AUTH_REVOCATION_POLL_SECS (default 5) instead
  of up to JWT_ACCESS_TTL_SECS.
- New auth_sessions table and refresh_tokens.session_id. Migration 000035 takes an
  AccessExclusiveLock on refresh_tokens for the duration — schedule a window, and
  run sauron-migrate after upgrading or authentication will fail outright.

* Sat Aug 01 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.1.0-2
- Transactional email foundation: a deployment-level SMTP relay, an HTML/plain
  email template engine, and a durable outbox drained by sauron-api.
- New `mail_outbox` table; RUN sauron-migrate AFTER UPGRADING (see SETUP.md
  section 11). Without it sauron-api queries a relation that does not exist and
  transactional email silently does nothing.
- New settings in /etc/sauron/api.env (shipped commented out, so they land in
  api.env.rpmnew on upgrade): SMTP_HOST/PORT/USERNAME/FROM/FROM_NAME/TLS/
  ALLOW_PRIVATE/TIMEOUT_MS/SINK, MAIL_DRAIN_TICK_SECS, MAIL_OUTBOX_RETENTION_DAYS
  and DASHBOARD_URL. SMTP_PASSWORD belongs in /etc/sauron/secret.env.
- No new binaries, no new units.

* Thu Jul 30 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.1.0-1
- Workflow grouping: apps can bound a named span of activity, and the events,
  errors and transactions captured inside it are grouped as one unit.
- New `workflows` table plus nullable workflow columns on analytics_events,
  error_events and transactions; run sauron-migrate after upgrading.

* Wed Jul 29 2026 Soheyb Merah <merah.soheyb@gmail.com> - 1.0.0-1
- Version 1.0.0: first stable release of the Sauron platform.

* Tue Jul 21 2026 Soheyb Merah <merah.soheyb@gmail.com> - 0.1.0-2
- Link DuckDB against a prebuilt libduckdb (vendored .so in sauron-server) instead
  of compiling the bundled C++ amalgamation — large build-time reduction.
- Strip release binaries; add `--with prebuilt` mode so CI can package precompiled
  artifacts without recompiling.

* Thu Jul 16 2026 Soheyb Merah <merah.soheyb@gmail.com> - 0.1.0-1
- Initial RPM packaging: sauron (base), sauron-server, sauron-dashboard, sauron-cli.
