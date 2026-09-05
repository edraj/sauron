#!/usr/bin/env bash
# Build the Sauron .deb packages.
#
#   ./packaging/deb/build-deb.sh                  # compile, then build the .debs
#   ./packaging/deb/build-deb.sh --prebuilt DIR   # package precompiled artifacts (no compile)
#   ./packaging/deb/build-deb.sh --distro deb12   # override the version suffix
#
# The Debian counterpart of packaging/rpm/build-rpm.sh, and deliberately the same
# shape: --prebuilt decouples the (slow) compile from the (fast) packaging step so
# CI can compile once per distro and package in seconds with no toolchain present.
#
#   DIR/bin/<name>     one per line of packaging/rpm/binaries.txt
#   DIR/dist/          dashboard build output (contents of dashboard/dist)
#   DIR/libduckdb.so   the prebuilt DuckDB library the binaries were linked against
#
# UNLIKE the RPM path, debian/rules never compiles: every dh_auto_* is a no-op.
# Compilation lives here, in one place, for both modes. See packaging/deb/debian/rules.
#
# Output: <topdir>/*.deb  (topdir defaults to <repo>/build/deb, override with
# DEB_BUILD_TOPDIR). The build tree itself is <topdir>/sauron-<version>.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

# --- parse flags (fail fast on typos, before the slow staging) ---
prebuilt_dir=""
distro=""
while [ $# -gt 0 ]; do
    case "$1" in
        --prebuilt) prebuilt_dir="${2:-}"; [ -n "$prebuilt_dir" ] || { echo "--prebuilt needs a directory" >&2; exit 2; }; shift 2 ;;
        --distro)   distro="${2:-}";       [ -n "$distro" ]       || { echo "--distro needs a suffix" >&2; exit 2; };    shift 2 ;;
        -h|--help)  sed -n '4,6p' "$0" | sed 's/^#   //'; exit 0 ;;
        *) echo "unknown argument: $1 (accepts --prebuilt DIR, --distro SUFFIX)" >&2; exit 2 ;;
    esac
done

name=sauron
version="$(awk -F'"' '/^version *= *"/{print $2; exit}' backend/Cargo.toml)"
[ -n "$version" ] || { echo "could not read version from backend/Cargo.toml" >&2; exit 1; }

# The two packaging formats must not ship different versions from one commit.
# build-rpm.sh makes the same assertion for the same reason; without it a release
# run can publish sauron-server-1.8.1.rpm next to sauron-server_1.8.0_amd64.deb
# and nothing anywhere notices.
spec_version="$(awk '/^Version:/{print $2; exit}' packaging/rpm/sauron.spec)"
if [ "$version" != "$spec_version" ]; then
    echo "version mismatch: backend/Cargo.toml is $version but packaging/rpm/sauron.spec is $spec_version" >&2
    echo "bump both (plus dashboard/package.json) so the RPMs and .debs agree" >&2
    exit 1
fi

# --- distro suffix ---------------------------------------------------------
# MANDATORY, not cosmetic. Both release legs produce a package named
# sauron-server_<version>_amd64.deb; without a distinguishing suffix the Debian 12
# and Ubuntu 22.04 artifacts have byte-identical filenames and silently overwrite
# each other when release.yml merges both into dist/.
if [ -z "$distro" ]; then
    if [ -r /etc/os-release ]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        case "${ID:-}" in
            debian) distro="deb${VERSION_ID:-}" ;;
            ubuntu) distro="ubuntu${VERSION_ID:-}" ;;
            *)      distro="${ID:-unknown}${VERSION_ID:-}" ;;
        esac
    else
        echo "no /etc/os-release and no --distro: cannot derive a version suffix" >&2
        exit 1
    fi
fi
# '~' sorts BEFORE the empty string, so 1.8.1-1~deb12 < 1.8.1-1. That is the
# conventional Debian notation for a distribution-specific rebuild, and what
# matters for upgrades holds either way: within one distribution the version
# still increases monotonically across releases (1.8.1-1~deb12 -> 1.8.2-1~deb12).
deb_version="${version}-1~${distro}"

topdir="${DEB_BUILD_TOPDIR:-$repo_root/build/deb}"
builddir="$topdir/${name}-${version}"
rm -rf "$builddir"
mkdir -p "$builddir"

echo ">> Building ${name} ${deb_version}"

# --- stage the source tree debian/rules expects ----------------------------
# Only what debian/rules actually reads. Notably NOT backend/ or dashboard/
# sources: dh must never be able to find a Cargo.toml to build.
echo ">> Staging build tree $builddir"
mkdir -p "$builddir/packaging/deb"
cp -a packaging/rpm "$builddir/packaging/rpm"
# debian/sauron-server.docs ships packaging/deb/INSTALL.md, so it has to be in
# the build tree for dh_installdocs to find it.
install -m0644 packaging/deb/INSTALL.md "$builddir/packaging/deb/INSTALL.md"
cp -a packaging/deb/debian "$builddir/debian"
rm -f "$builddir/debian/changelog.in"
install -m0644 LICENSE README.md "$builddir/"

# --- artifacts: prebuilt, or compile now -----------------------------------
mkdir -p "$builddir/prebuilt/bin" "$builddir/prebuilt/dist"

manifest=packaging/rpm/binaries.txt
[ -f "$manifest" ] || { echo "missing binary manifest: $manifest" >&2; exit 1; }
mapfile -t bins < <(grep -vE '^[[:space:]]*(#|$)' "$manifest")
[ "${#bins[@]}" -gt 0 ] || { echo "no binaries listed in $manifest" >&2; exit 1; }

if [ -n "$prebuilt_dir" ]; then
    echo ">> Using prebuilt artifacts from $prebuilt_dir (no compile)"
    # Preflight every path before copying anything, so a missing binary fails here
    # with its own name rather than half-way through dh_install.
    for b in "${bins[@]}"; do
        [ -f "$prebuilt_dir/bin/$b" ] || { echo "missing prebuilt binary: $prebuilt_dir/bin/$b" >&2; exit 1; }
    done
    [ -d "$prebuilt_dir/dist" ]        || { echo "missing prebuilt dashboard: $prebuilt_dir/dist/" >&2; exit 1; }
    [ -f "$prebuilt_dir/libduckdb.so" ] || { echo "missing $prebuilt_dir/libduckdb.so" >&2; exit 1; }

    for b in "${bins[@]}"; do install -m0755 "$prebuilt_dir/bin/$b" "$builddir/prebuilt/bin/$b"; done
    cp -a "$prebuilt_dir/dist/." "$builddir/prebuilt/dist/"
    install -m0755 "$prebuilt_dir/libduckdb.so" "$builddir/prebuilt/libduckdb.so"
else
    echo ">> Compiling the workspace + dashboard (slow)"
    duckdb_dir="$("$repo_root/packaging/rpm/fetch-libduckdb.sh")"
    export DUCKDB_LIB_DIR="$duckdb_dir"

    # Same overlap as the RPM's %build and release.yml: the npm build hides under
    # the (longer) cargo compile.
    (cd dashboard && npm ci && npm run build) &
    dashboard_build=$!
    (cd backend && cargo build --release --workspace)
    wait "$dashboard_build"

    for b in "${bins[@]}"; do
        install -m0755 "backend/target/release/$b" "$builddir/prebuilt/bin/$b"
    done
    cp -a dashboard/dist/. "$builddir/prebuilt/dist/"
    install -m0755 "$duckdb_dir/libduckdb.so" "$builddir/prebuilt/libduckdb.so"
fi

# --- generate debian/changelog ---------------------------------------------
maintainer="$(awk -F': ' '/^Maintainer:/{print $2; exit}' packaging/deb/debian/control)"
[ -n "$maintainer" ] || { echo "no Maintainer: in packaging/deb/debian/control" >&2; exit 1; }
codename="unstable"
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    codename="${VERSION_CODENAME:-unstable}"
fi
sed -e "s|@VERSION@|${deb_version}|g" \
    -e "s|@VERSION_UPSTREAM@|${version}|g" \
    -e "s|@DISTRO@|${codename}|g" \
    -e "s|@MAINTAINER@|${maintainer}|g" \
    -e "s|@DATE@|$(date -R)|g" \
    packaging/deb/debian/changelog.in > "$builddir/debian/changelog"

# --- build ------------------------------------------------------------------
# -b        binary packages only (no source package: .dsc/.orig.tar are a stated
#           non-goal, and there is no upstream tarball to point one at)
# -uc -us   unsigned; the release is signed by GitHub, not by a Debian key
# -d        skip the Build-Depends check. In --prebuilt mode the package job
#           installs debhelper and nothing else, which is all debian/rules
#           actually uses; the check would still pass, but skipping it keeps the
#           two modes byte-identical in behaviour.
echo ">> dpkg-buildpackage"
(cd "$builddir" && dpkg-buildpackage -b -uc -us -d)

echo ">> Done. Artifacts:"
find "$topdir" -maxdepth 1 -name "*_${deb_version}_*.deb" | sort
