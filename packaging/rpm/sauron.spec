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
* Thu Jul 16 2026 Soheyb Merah <merah.soheyb@gmail.com> - 0.1.0-1
- Initial RPM packaging: sauron (base), sauron-server, sauron-dashboard, sauron-cli.
