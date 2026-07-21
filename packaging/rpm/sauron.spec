# Rust release binaries; skip the debuginfo subpackage for this build.
%global debug_package %{nil}

# Prebuilt mode (`rpmbuild --with prebuilt`, driven by build-rpm.sh --prebuilt):
# %%build is skipped and %%install consumes binaries + dashboard/dist staged into
# the source tree by CI, so packaging costs seconds instead of recompiling.
# (%% escapes the section names so el9's rpm 4.16 doesn't expand them in this comment.)
%bcond_with prebuilt

Name:           sauron
Version:        0.1.0
Release:        2%{?dist}
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

# redhat-rpm-config injects RUSTFLAGS with -Cdebuginfo=2 -Ccodegen-units=1
# -Cstrip=none: that generates debuginfo we discard (debug_package is %%{nil}),
# forces slow single-unit codegen, and defeats the release `strip`. Append
# last-wins overrides to restore fast, stripped codegen while keeping the
# hardening/link flags redhat-rpm-config also set.
export RUSTFLAGS="${RUSTFLAGS:-} -Cdebuginfo=0 -Ccodegen-units=16 -Cstrip=symbols"

(cd backend && cargo build --release --workspace)

wait "$dashboard_build"
%else
# Prebuilt mode: binaries (backend/target/release) and dashboard/dist were staged
# into the source tree by build-rpm.sh --prebuilt. Nothing to compile.
:
%endif

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
%systemd_post sauron-api.service sauron-ingest.service sauron-monitor.service sauron-tier.service sauron-migrate.service
# Refresh the dynamic linker cache so sauron-tier finds the vendored
# %%{_libdir}/sauron/libduckdb.so via the ld.so.conf.d drop-in.
/sbin/ldconfig
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
# Rebuild the linker cache after the vendored libduckdb is added/removed.
/sbin/ldconfig

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
* Tue Jul 21 2026 Soheyb Merah <merah.soheyb@gmail.com> - 0.1.0-2
- Link DuckDB against a prebuilt libduckdb (vendored .so in sauron-server) instead
  of compiling the bundled C++ amalgamation — large build-time reduction.
- Strip release binaries; add `--with prebuilt` mode so CI can package precompiled
  artifacts without recompiling.

* Thu Jul 16 2026 Soheyb Merah <merah.soheyb@gmail.com> - 0.1.0-1
- Initial RPM packaging: sauron (base), sauron-server, sauron-dashboard, sauron-cli.
