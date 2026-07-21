#!/usr/bin/env bash
# Build the Sauron RPMs.
#
#   ./packaging/rpm/build-rpm.sh                  # compile, then build source + binary RPMs
#   ./packaging/rpm/build-rpm.sh --srpm           # source RPM only (fast, no compile)
#   ./packaging/rpm/build-rpm.sh --prebuilt DIR   # package precompiled artifacts (no compile)
#   ./packaging/rpm/build-rpm.sh --nodeps         # skip rpmbuild's BuildRequires check
# Flags may be combined (except --srpm with --prebuilt).
#
# DuckDB is linked against a prebuilt libduckdb (fetched by fetch-libduckdb.sh and
# shipped in the sauron-server package) instead of compiling the C++ amalgamation.
#
# --prebuilt DIR decouples the (slow) compile from the (fast) packaging step. DIR:
#   DIR/bin/{sauron-api,sauron-ingest,sauron-monitor,sauron-tier,sauron-migrate,sauron-symcli,crebain}
#   DIR/dist/          dashboard build output (contents of dashboard/dist)
#   DIR/libduckdb.so   the prebuilt DuckDB library the binaries were linked against
# In this mode no toolchain is required, so --nodeps is added automatically.
#
# On --nodeps: rpmbuild resolves BuildRequires against the RPM DATABASE, not $PATH.
# If Rust/Node come from rustup/nvm rather than the Fedora RPMs, the check fails
# even though the tools work; that case is AUTO-DETECTED and --nodeps is added.
#
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

# --- parse flags (fail fast on typos, before the slow staging) ---
mode=-ba
nodeps=""
prebuilt_dir=""
while [ $# -gt 0 ]; do
    case "$1" in
        --srpm)     mode=-bs; shift ;;
        --nodeps)   nodeps=--nodeps; shift ;;
        --prebuilt) prebuilt_dir="${2:-}"; [ -n "$prebuilt_dir" ] || { echo "--prebuilt needs a directory" >&2; exit 2; }; shift 2 ;;
        -h|--help)  sed -n '4,7p' "$0" | sed 's/^#   //'; exit 0 ;;
        *) echo "unknown argument: $1 (accepts --srpm, --prebuilt DIR, --nodeps)" >&2; exit 2 ;;
    esac
done
if [ "$mode" = -bs ] && [ -n "$prebuilt_dir" ]; then
    echo "--srpm and --prebuilt are mutually exclusive" >&2; exit 2
fi

name=sauron
version="$(awk -F'"' '/^version *= *"/{print $2; exit}' backend/Cargo.toml)"
[ -n "$version" ] || { echo "could not read version from backend/Cargo.toml" >&2; exit 1; }

topdir="${RPMBUILD_TOPDIR:-$HOME/rpmbuild}"
mkdir -p "$topdir"/{SOURCES,SPECS,BUILD,BUILDROOT,RPMS,SRPMS}

# --- stage the prebuilt libduckdb.so (Source50) — shipped in the RPM either way ---
if [ -n "$prebuilt_dir" ]; then
    [ -f "$prebuilt_dir/libduckdb.so" ] || { echo "missing $prebuilt_dir/libduckdb.so" >&2; exit 1; }
    install -m0755 "$prebuilt_dir/libduckdb.so" "$topdir/SOURCES/libduckdb.so"
else
    echo ">> Fetching prebuilt libduckdb"
    duckdb_dir="$("$repo_root/packaging/rpm/fetch-libduckdb.sh")"
    install -m0755 "$duckdb_dir/libduckdb.so" "$topdir/SOURCES/libduckdb.so"
fi

# --- prebuilt mode: overlay tarball of binaries + dashboard/dist (Source51) ---
prebuilt_with=()
if [ -n "$prebuilt_dir" ]; then
    bins=(sauron-api sauron-ingest sauron-monitor sauron-tier sauron-migrate sauron-symcli crebain)
    for b in "${bins[@]}"; do
        [ -f "$prebuilt_dir/bin/$b" ] || { echo "missing prebuilt binary: $prebuilt_dir/bin/$b" >&2; exit 1; }
    done
    [ -d "$prebuilt_dir/dist" ] || { echo "missing prebuilt dashboard: $prebuilt_dir/dist/" >&2; exit 1; }

    echo ">> Staging prebuilt overlay sauron-prebuilt.tar.gz"
    stage="$(mktemp -d)"; trap 'rm -rf "$stage"' EXIT
    mkdir -p "$stage/backend/target/release" "$stage/dashboard/dist"
    for b in "${bins[@]}"; do install -m0755 "$prebuilt_dir/bin/$b" "$stage/backend/target/release/$b"; done
    cp -a "$prebuilt_dir/dist/." "$stage/dashboard/dist/"
    tar czf "$topdir/SOURCES/sauron-prebuilt.tar.gz" -C "$stage" backend dashboard

    prebuilt_with=(--with prebuilt)
    # No toolchain in prebuilt mode → BuildRequires (cargo/rust/node) are irrelevant.
    [ -z "$nodeps" ] && nodeps=--nodeps
fi

echo ">> Staging source tarball ${name}-${version}.tar.gz"
tar czf "$topdir/SOURCES/${name}-${version}.tar.gz" \
    --exclude-vcs \
    --exclude='backend/target' \
    --exclude='dashboard/node_modules' \
    --exclude='dashboard/dist' \
    --exclude='.cache' \
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
# cargo RPM is absent but cargo is on PATH (rustup/nvm toolchain) — compile only.
if [ "$mode" = -ba ] && [ -z "$nodeps" ] && [ -z "$prebuilt_dir" ] \
   && ! rpm -q cargo >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
    echo ">> Note: 'cargo' is on PATH but not installed as an RPM (rustup?) — adding --nodeps."
    echo ">>       rpmbuild checks the RPM db, not PATH; your toolchain is still used to build."
    nodeps=--nodeps
fi

# Prefer a fast linker for the from-source compile if one is available (CI installs
# lld). Best-effort: inherited by rpmbuild's %build; harmless if it doesn't apply.
if [ -z "$prebuilt_dir" ] && [ "$mode" = -ba ] && [ -z "${RUSTFLAGS:-}" ]; then
    if command -v mold >/dev/null 2>&1; then
        export RUSTFLAGS="-C link-arg=-fuse-ld=mold"
        echo ">> Using mold linker (RUSTFLAGS=$RUSTFLAGS)"
    elif command -v ld.lld >/dev/null 2>&1; then
        export RUSTFLAGS="-C link-arg=-fuse-ld=lld"
        echo ">> Using lld linker (RUSTFLAGS=$RUSTFLAGS)"
    fi
fi

# Point rpmbuild at the same topdir we staged into (derives _sourcedir, _rpmdir,
# _srcrpmdir, _builddir …). Without this rpmbuild uses ~/rpmbuild and can't find
# the staged sources when RPMBUILD_TOPDIR differs from the default.
build_args=("$mode" --define "_topdir $topdir" "${prebuilt_with[@]}")
[ -n "$nodeps" ] && build_args+=("$nodeps")
build_args+=("$topdir/SPECS/sauron.spec")

if [ "$mode" = -bs ]; then
    echo ">> Building source RPM only${nodeps:+ (--nodeps)}"
elif [ -n "$prebuilt_dir" ]; then
    echo ">> Packaging prebuilt artifacts (no compile)${nodeps:+ (--nodeps)}"
else
    echo ">> Building source + binary RPMs (compiles the Rust workspace — slow)${nodeps:+ (--nodeps)}"
fi
rpmbuild "${build_args[@]}"

echo ">> Done. Artifacts:"
find "$topdir/RPMS" "$topdir/SRPMS" -name "${name}*-${version}-*.rpm" 2>/dev/null | sort
