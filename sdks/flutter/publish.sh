#!/usr/bin/env bash
# Publish sauron_flutter to pub.dev.
#
# Dry run by default. Set APPLY=1 to actually publish.
#
#   ./publish.sh            # validate + dry-run, changes nothing
#   APPLY=1 ./publish.sh    # publish for real (irreversible)
#
# pub.dev versions are IMMUTABLE. Once a version is published it can never be
# replaced or re-licensed -- only retracted, which leaves it resolvable for
# anyone who already depends on it. That is why this script refuses to run
# against a version that already exists, and why the licence check below is
# not merely cosmetic: 1.8.0 went out on 2026-08-16 under AGPL, two weeks
# before the tree was relicensed, and no amount of republishing can change it.
set -euo pipefail

cd "$(dirname "$0")"
PKG=sauron_flutter
APPLY="${APPLY:-0}"

die() { printf '\n  FAIL  %s\n\n' "$*" >&2; exit 1; }
ok()  { printf '  ok    %s\n' "$*"; }

[ -f pubspec.yaml ] || die "run this from sdks/flutter (no pubspec.yaml here)"
grep -q "^name: $PKG\$" pubspec.yaml || die "pubspec.yaml is not $PKG"

# ---- version ---------------------------------------------------------------
VERSION=$(sed -n 's/^version: *//p' pubspec.yaml | head -1 | tr -d '\r')
[ -n "$VERSION" ] || die "no version in pubspec.yaml"
ok "pubspec version: $VERSION"

# Refuse to attempt a version pub.dev already has. `pub publish` would reject
# it anyway, but it does so *after* uploading the archive and only once you
# have confirmed -- this fails in a second instead, and says why.
PUBLISHED=$(curl -fsS "https://pub.dev/api/packages/$PKG" 2>/dev/null \
  | python3 -c 'import json,sys; print(" ".join(v["version"] for v in json.load(sys.stdin).get("versions",[])))' \
  2>/dev/null || echo "")
if [ -z "$PUBLISHED" ]; then
  ok "pub.dev has no releases yet (or is unreachable -- check manually)"
else
  for v in $PUBLISHED; do
    [ "$v" = "$VERSION" ] && die "$PKG $VERSION is ALREADY on pub.dev and cannot be
        replaced. Bump the version in pubspec.yaml and add a CHANGELOG entry.
        Published: $PUBLISHED"
  done
  ok "$VERSION is unpublished (pub.dev has: $PUBLISHED)"
fi

# ---- changelog -------------------------------------------------------------
grep -qE "^## +\[?${VERSION//./\\.}\]?( |$|-)" CHANGELOG.md \
  || die "CHANGELOG.md has no '## $VERSION' heading. pub.dev scores this, and
        a release with no entry is how a licence change reaches users unannounced."
ok "CHANGELOG.md documents $VERSION"

# ---- licence ---------------------------------------------------------------
# The SDKs are LGPL-3.0-only so that linking one does not pull an application
# into copyleft. Publishing an AGPL LICENSE here would silently undo that for
# every consumer, and could not be taken back.
head -3 LICENSE | grep -qi "LESSER GENERAL PUBLIC LICENSE" \
  || die "LICENSE is not the LGPL text -- refusing to publish.
        head -1: $(head -1 LICENSE | tr -s ' ')"
[ -f COPYING ] || die "COPYING (the GPLv3 base LGPLv3 extends) is missing"
head -3 COPYING | grep -qi "GNU GENERAL PUBLIC LICENSE" || die "COPYING is not the GPLv3 text"
ok "LICENSE is LGPLv3, COPYING is GPLv3"

# ---- gates -----------------------------------------------------------------
command -v flutter >/dev/null || die "flutter not on PATH"
echo; echo "  -- flutter pub get"      ; flutter pub get >/dev/null
echo "  -- flutter analyze"            ; flutter analyze
echo "  -- flutter test"               ; flutter test
echo "  -- pub publish --dry-run"      ; flutter pub publish --dry-run
echo

# ---- publish ---------------------------------------------------------------
if [ "$APPLY" != "1" ]; then
  echo "  DRY RUN. Nothing was published."
  echo "  Re-run with APPLY=1 to publish $PKG $VERSION to pub.dev."
  exit 0
fi

echo "  PUBLISHING $PKG $VERSION to pub.dev -- this cannot be undone."
flutter pub publish --force

echo
echo "  Published. Tag the release so the pub.dev version is traceable to a commit:"
echo "    git tag -a flutter-v$VERSION -m '$PKG $VERSION' && git push origin flutter-v$VERSION"
