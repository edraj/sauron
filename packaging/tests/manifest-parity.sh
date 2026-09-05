#!/usr/bin/env bash
# Assert the RPM and .deb ship exactly the same set of binaries.
#
#   packaging/tests/manifest-parity.sh
#
# Three lists have to agree, and nothing else in the repo compares them:
#
#   1. packaging/rpm/binaries.txt        the declared source of truth
#   2. packaging/rpm/sauron.spec         %files entries (%{_bindir}/<name>)
#   3. packaging/deb/debian/*.install    usr/bin/<name> entries
#
# Each format catches its OWN drift and neither catches the other's. rpmbuild
# fails on a binary installed but unpackaged, and dh_missing --fail-missing does
# the same for the .deb -- so a binary added to binaries.txt but to no %files
# section fails the RPM build, and one added to no *.install fails the deb build.
# What passes both is a binary added to binaries.txt AND to sauron.spec but not to
# any debian/*.install: the RPM is complete, dh_missing is satisfied because...
# it is not -- but only the deb job would say so, and only if it runs.
#
# The reverse is worse and completely silent: a binary dropped from binaries.txt
# but left in a %files section. binaries.txt drives the install loop in BOTH
# formats, so nothing installs it, nothing packages it, and both builds go green
# while the binary silently stops shipping.
#
# Shell-only by design: no toolchain, no containers, so this can run in the
# fastest job in the matrix and fail before anything expensive starts.
set -uo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

fail=0
note() { printf '%s\n' "$*" >&2; }
die()  { note "::error::$*"; fail=1; }

manifest=packaging/rpm/binaries.txt
spec=packaging/rpm/sauron.spec
debdir=packaging/deb/debian

for f in "$manifest" "$spec"; do
    [ -f "$f" ] || { die "missing $f"; exit 1; }
done
[ -d "$debdir" ] || { die "missing $debdir"; exit 1; }

# --- 1. declared ------------------------------------------------------------
declared="$(grep -vE '^[[:space:]]*(#|$)' "$manifest" | sort -u)"
[ -n "$declared" ] || { die "$manifest lists no binaries"; exit 1; }

# --- 2. the RPM's %files ----------------------------------------------------
# Only %{_bindir}/<name> lines; the spec references binaries nowhere else in
# %files. Comment lines are dropped first so a commented-out entry does not count
# as shipped.
rpm_files="$(grep -vE '^[[:space:]]*#' "$spec" \
             | grep -oE '^%\{_bindir\}/[A-Za-z0-9._-]+' \
             | sed 's|^%{_bindir}/||' | sort)"
rpm_uniq="$(printf '%s\n' "$rpm_files" | sort -u)"

# --- 3. the .deb's *.install ------------------------------------------------
deb_files="$(cat "$debdir"/*.install 2>/dev/null \
             | grep -oE '^usr/bin/[A-Za-z0-9._-]+' \
             | sed 's|^usr/bin/||' | sort)"
deb_uniq="$(printf '%s\n' "$deb_files" | sort -u)"

n_declared=$(printf '%s\n' "$declared" | grep -c .)
note "binaries.txt=$n_declared  sauron.spec=$(printf '%s\n' "$rpm_uniq" | grep -c .)  debian/*.install=$(printf '%s\n' "$deb_uniq" | grep -c .)"

# --- no binary claimed twice within one format ------------------------------
# rpmbuild tolerates a duplicate %files entry across subpackages by producing two
# packages that both own the path and cannot be co-installed; dh_install happily
# copies the file into both .debs with the same result. Neither errors.
dupes="$(printf '%s\n' "$rpm_files" | uniq -d)"
[ -z "$dupes" ] || die "sauron.spec packages these binaries in more than one %files section: $(echo $dupes)"
dupes="$(printf '%s\n' "$deb_files" | uniq -d)"
[ -z "$dupes" ] || die "debian/*.install claims these binaries in more than one package: $(echo $dupes)"

# --- parity -----------------------------------------------------------------
cmp_sets() { # cmp_sets <label-a> <a> <label-b> <b>
    local la="$1" a="$2" lb="$3" b="$4" only_a only_b
    only_a="$(comm -23 <(printf '%s\n' "$a") <(printf '%s\n' "$b"))"
    only_b="$(comm -13 <(printf '%s\n' "$a") <(printf '%s\n' "$b"))"
    [ -z "$only_a" ] || die "in $la but not $lb: $(echo $only_a)"
    [ -z "$only_b" ] || die "in $lb but not $la: $(echo $only_b)"
}

cmp_sets "binaries.txt" "$declared" "sauron.spec %files"      "$rpm_uniq"
cmp_sets "binaries.txt" "$declared" "debian/*.install"        "$deb_uniq"

if [ "$fail" -ne 0 ]; then
    note "PACKAGING MANIFEST PARITY FAILED"
    note "Every binary must appear in all three: packaging/rpm/binaries.txt, a %files"
    note "section of packaging/rpm/sauron.spec, and one packaging/deb/debian/*.install."
    exit 1
fi
note "PACKAGING MANIFEST PARITY OK ($n_declared binaries in all three lists)"
