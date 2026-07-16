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
