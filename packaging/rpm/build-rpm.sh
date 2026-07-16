#!/usr/bin/env bash
# Build the Sauron RPMs from the current working tree (uncommitted files included).
#
#   ./packaging/rpm/build-rpm.sh              # build source + binary RPMs
#   ./packaging/rpm/build-rpm.sh --srpm       # source RPM only (fast, no compile)
#   ./packaging/rpm/build-rpm.sh --nodeps     # skip rpmbuild's BuildRequires check
#
# On --nodeps: rpmbuild resolves BuildRequires against the RPM DATABASE, not $PATH.
# If your Rust/Node come from rustup/nvm rather than the Fedora cargo/rust/nodejs/npm
# RPMs, the check fails ("cargo >= 1.82 is needed") even though the tools work. That
# case is AUTO-DETECTED and --nodeps is added for you; pass it explicitly to force it.
# Flags may be combined.
#
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

# --- parse flags (fail fast on typos, before the slow staging) ---
mode=-ba
nodeps=""
for arg in "$@"; do
    case "$arg" in
        --srpm)    mode=-bs ;;
        --nodeps)  nodeps=--nodeps ;;
        -h|--help) sed -n '4,6p' "$0" | sed 's/^#   //'; exit 0 ;;
        *) echo "unknown argument: $arg (accepts --srpm and/or --nodeps)" >&2; exit 2 ;;
    esac
done

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

# rpmbuild checks BuildRequires against the RPM db, not $PATH. Auto-skip when the
# cargo RPM is absent but cargo is on PATH (rustup/nvm toolchain) — binary build only.
if [ "$mode" = -ba ] && [ -z "$nodeps" ] \
   && ! rpm -q cargo >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
    echo ">> Note: 'cargo' is on PATH but not installed as an RPM (rustup?) — adding --nodeps."
    echo ">>       rpmbuild checks the RPM db, not PATH; your toolchain is still used to build."
    nodeps=--nodeps
fi

build_args=("$mode")
[ -n "$nodeps" ] && build_args+=("$nodeps")
build_args+=("$topdir/SPECS/sauron.spec")

if [ "$mode" = -bs ]; then
    echo ">> Building source RPM only${nodeps:+ (--nodeps)}"
else
    echo ">> Building source + binary RPMs (this compiles the Rust workspace — slow)${nodeps:+ (--nodeps)}"
fi
rpmbuild "${build_args[@]}"

echo ">> Done. Artifacts:"
find "$topdir/RPMS" "$topdir/SRPMS" -name "${name}*-${version}-*.rpm" 2>/dev/null | sort
