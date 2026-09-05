#!/usr/bin/env bash
# Produce a --prebuilt directory of STUB artifacts for packaging tests.
#
#   packaging/deb/tests/make-stub-prebuilt.sh <outdir>
#
# The packaging gate has to answer "do these .debs build, install, upgrade and
# purge correctly", which needs nothing from the real binaries except that they
# are ELF and linked the way the real ones are. Compiling the actual workspace to
# find that out would cost ~40 minutes per PR; these stubs cost seconds.
#
# What is faithfully reproduced, because the packaging depends on it:
#   * every binary in packaging/rpm/binaries.txt exists and is ELF (dh_strip,
#     dh_shlibdeps and the ELF-only paths in dh all need real objects)
#   * sauron-tier links against a libduckdb.so with the real SONAME, so
#     dh_shlibdeps hits the genuine "private library with no dependency
#     information" case that override_dh_shlibdeps exists to solve
#   * dashboard/dist carries config.template.js and config.js, so the
#     ship-the-template-not-the-output behaviour is exercised
#
# What is NOT reproduced: any actual Sauron behaviour. Never ship these.
set -euo pipefail

out="${1:?usage: make-stub-prebuilt.sh <outdir>}"
repo_root="$(git rev-parse --show-toplevel)"

command -v cc >/dev/null || { echo "make-stub-prebuilt.sh needs a C compiler" >&2; exit 1; }

mkdir -p "$out/bin" "$out/dist"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

# --- stub libduckdb.so -----------------------------------------------------
# The SONAME matters: dh_shlibdeps resolves sauron-tier's DT_NEEDED entry against
# it, and that is the lookup that fails without the -l flag in debian/rules.
cat > "$tmp/duckdb.c" <<'EOF'
int duckdb_open(const char *path, void **db) { (void)path; (void)db; return 0; }
EOF
cc -shared -fPIC -Wl,-soname,libduckdb.so -o "$out/libduckdb.so" "$tmp/duckdb.c"

# --- stub binaries ---------------------------------------------------------
cat > "$tmp/stub.c" <<'EOF'
#include <stdio.h>
int main(int argc, char **argv) {
    (void)argc;
    fprintf(stderr, "%s: packaging stub, not a real Sauron binary\n", argv[0]);
    return 0;
}
EOF
# sauron-tier is the one that links DuckDB in the real workspace; keep that true
# here so the shlibdeps path under test is the real one.
cat > "$tmp/stub-tier.c" <<'EOF'
#include <stdio.h>
int duckdb_open(const char *path, void **db);
int main(int argc, char **argv) {
    (void)argc;
    if (0) duckdb_open("", 0);
    fprintf(stderr, "%s: packaging stub, not a real Sauron binary\n", argv[0]);
    return 0;
}
EOF

manifest="$repo_root/packaging/rpm/binaries.txt"
[ -f "$manifest" ] || { echo "missing $manifest" >&2; exit 1; }
while read -r b; do
    if [ "$b" = "sauron-tier" ]; then
        cc -o "$out/bin/$b" "$tmp/stub-tier.c" -L"$out" -lduckdb -Wl,-rpath-link,"$out"
    else
        cc -o "$out/bin/$b" "$tmp/stub.c"
    fi
done < <(grep -vE '^[[:space:]]*(#|$)' "$manifest")

# --- stub dashboard --------------------------------------------------------
cat > "$out/dist/index.html" <<'EOF'
<!doctype html><title>Sauron (packaging stub)</title><script src="/config.js"></script>
EOF
cat > "$out/dist/config.template.js" <<'EOF'
window.__SAURON_CONFIG__ = {
  apiBaseUrl: "${API_BASE_URL}",
  ingestBaseUrl: "${INGEST_BASE_URL}"
};
EOF
# Present on purpose: debian/rules must DELETE this and ship only the template,
# exactly as the RPM's %install does. If it ever stops deleting it, the shipped
# config.js would override the one the postinst generates.
cp "$out/dist/config.template.js" "$out/dist/config.js"
mkdir -p "$out/dist/assets"
echo "/* stub */" > "$out/dist/assets/index-stub.js"

echo "stub prebuilt tree ready: $out"
