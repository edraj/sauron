#!/usr/bin/env bash
# Fetch a prebuilt libduckdb matching the libduckdb-sys version pinned in
# backend/Cargo.lock, so the workspace links DuckDB dynamically instead of
# compiling the DuckDB C++ amalgamation from source (the `bundled` feature) —
# the single slowest item in the whole workspace build.
#
#   dir=$(packaging/rpm/fetch-libduckdb.sh)
#   export DUCKDB_LIB_DIR="$dir" DUCKDB_INCLUDE_DIR="$dir"
#   cargo build --release --workspace          # links dylib=duckdb, no C++ compile
#
# The DuckDB version is DERIVED from Cargo.lock (libduckdb-sys uses the
# 1.MAJOR_MINOR_PATCH.x scheme, e.g. 1.10504.0 => DuckDB 1.5.4), so a future
# crate bump automatically fetches the matching prebuilt library.
#
# Flags / env:
#   --target <triple>   default: x86_64-unknown-linux-gnu (aarch64-… also mapped)
#   --dir <cache>       default: $DUCKDB_VENDOR_DIR, else <repo>/.cache/duckdb
#
# Progress is written to stderr; the extraction directory (containing
# libduckdb.so + duckdb.h) is printed as the only stdout line.
set -euo pipefail

target="x86_64-unknown-linux-gnu"
cache="${DUCKDB_VENDOR_DIR:-}"
while [ $# -gt 0 ]; do
    case "$1" in
        --target) target="$2"; shift 2 ;;
        --dir)    cache="$2"; shift 2 ;;
        -h|--help) sed -n '2,17p' "$0" | sed 's/^#\s\?//'; exit 0 ;;
        *) echo "unknown argument: $1 (accepts --target and --dir)" >&2; exit 2 ;;
    esac
done

repo_root="$(git rev-parse --show-toplevel)"
: "${cache:=$repo_root/.cache/duckdb}"

lock="$repo_root/backend/Cargo.lock"
[ -f "$lock" ] || { echo "missing $lock (run 'cargo generate-lockfile' in backend/)" >&2; exit 1; }

# libduckdb-sys encodes the DuckDB C-library version as the integer in the second
# dotted component: 1.<MMmmpp>.x  =>  DuckDB <M>.<mm>.<pp>.
encoded="$(awk '
    $1=="name" && $3=="\"libduckdb-sys\""{f=1; next}
    f && $1=="version"{gsub(/"/,"",$3); split($3,a,"."); print a[2]; exit}
' "$lock")"
[ -n "${encoded:-}" ] || { echo "could not find libduckdb-sys version in $lock" >&2; exit 1; }
ver="$(( encoded / 10000 )).$(( (encoded / 100) % 100 )).$(( encoded % 100 ))"

case "$target" in
    x86_64-unknown-linux-gnu)  asset="libduckdb-linux-amd64.zip" ;;
    aarch64-unknown-linux-gnu) asset="libduckdb-linux-arm64.zip" ;;
    *) echo "no prebuilt libduckdb asset mapped for target '$target'" >&2; exit 1 ;;
esac

dir="$cache/$ver/$target"
lib="$dir/libduckdb.so"

if [ -f "$lib" ]; then
    echo ">> libduckdb $ver already vendored ($lib)" >&2
    printf '%s\n' "$dir"
    exit 0
fi

url="https://github.com/duckdb/duckdb/releases/download/v${ver}/${asset}"
echo ">> Fetching libduckdb $ver for $target" >&2
echo ">>   $url" >&2

mkdir -p "$dir"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
curl --proto '=https' --tlsv1.2 -fSL "$url" -o "$tmp/$asset"
unzip -o -q "$tmp/$asset" -d "$dir"

[ -f "$lib" ] || { echo "archive did not contain libduckdb.so:" >&2; ls -la "$dir" >&2; exit 1; }

# The official build ships with symbols we don't need. --strip-unneeded keeps the
# dynamic symbol table (linking + loading still work) while shrinking the shipped
# library (~68M -> ~57M). Best-effort — skipped if strip is unavailable.
if command -v strip >/dev/null 2>&1; then
    before=$(du -m "$lib" | cut -f1)
    strip --strip-unneeded "$lib" 2>/dev/null \
        && echo ">> Stripped libduckdb.so (${before}M -> $(du -m "$lib" | cut -f1)M)" >&2 \
        || echo ">> (strip failed — shipping unstripped)" >&2
fi
echo ">> Extracted libduckdb $ver -> $dir" >&2
printf '%s\n' "$dir"
