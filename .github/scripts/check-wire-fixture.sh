#!/usr/bin/env bash
#
# SDK-side drift gate for the wire fixtures.
#
# Each SDK's suite REGENERATES `sdks/wire-fixtures/<sdk>.json` from the envelope
# its own transport posts. Without this script that regeneration is thrown away
# at the end of the job, so CI only ever caught drift in ONE direction:
#
#   * the `backend` job parses the COMMITTED fixtures on a clean checkout, which
#     catches the backend's deserializer changing under a fixed SDK output;
#   * nothing compared the freshly generated envelope against the committed one,
#     so an SDK-side wire regression left `sdk-js` green (it regenerates) AND
#     `backend` green (it parses the stale committed file) — the identical
#     both-sides-agree failure mode the conformance test exists to close.
#
# So: after the suite runs, the working tree must be clean for that fixture.
#
# A diff here is not automatically a bug. It means "the bytes this SDK puts on
# the wire changed". Either that was intended — commit the regenerated fixture
# and let `sdk_wire_conformance` have an opinion about it — or it was not, in
# which case the diff is the regression, caught before it shipped.
#
# It should NOT fire for toolchain reasons. `sdks/js/test/wire-fixture-io.ts` and
# its four siblings pin everything the runner/host supplies (frame identity
# strings, `context.os` / `.runtime` / `.device` values) precisely so that a
# vitest, Node, CPython, .NET or Dart upgrade is not a fixture diff. If this gate
# ever fires for something that is not a wire change, fix the normalizer rather
# than committing the noise — a gate reviewers learn to wave through is worse
# than no gate. See `sdks/wire-fixtures/README.md`.
set -euo pipefail

sdk="${1:?usage: check-wire-fixture.sh <js|node|python|csharp|flutter>}"
fixture="sdks/wire-fixtures/${sdk}.json"

if [ ! -f "$fixture" ]; then
  echo "::error::${fixture} does not exist after running the ${sdk} suite."
  echo "The suite is supposed to write it (see sdks/wire-fixtures/README.md for"
  echo "which test does). A missing fixture is a conformance hole, not a skip."
  exit 1
fi

# An untracked fixture is its own failure: it means the file is not in the repo,
# so the backend job would be parsing nothing.
if ! git ls-files --error-unmatch "$fixture" >/dev/null 2>&1; then
  echo "::error::${fixture} is not tracked by git. The suite wrote it, but the"
  echo "repository has no committed copy for the backend job to parse. Commit it."
  exit 1
fi

# Worktree vs INDEX, not vs HEAD. After `actions/checkout` the index matches HEAD
# exactly, so in CI this is precisely "did the suite change the committed
# fixture". Comparing against the index rather than HEAD also keeps the script
# usable locally in a tree with staged-but-uncommitted work.
if git diff --quiet -- "$fixture"; then
  echo "${sdk}: wire fixture unchanged — the envelope this SDK posts still matches ${fixture}."
  exit 0
fi

echo "--- diff ---"
git --no-pager diff -- "$fixture" || true

cat <<EOF
::error file=${fixture}::the ${sdk} SDK now posts a DIFFERENT envelope than ${fixture} records
The ${sdk} suite regenerated its wire fixture and the result differs from the
committed file, so the bytes this SDK puts on the wire have changed.

  * If the change is intended, run that SDK's suite locally and COMMIT the
    regenerated fixture. The backend's \`cargo test -p sauron-core --test
    sdk_wire_conformance\` will then check the new shape actually deserializes.
  * If it is not intended, this diff IS the regression — one wire-invalid item
    is a 400 \`invalid_envelope\` for the whole batch (default 30 items), which
    every SDK drops without retrying.
  * If the diff is toolchain noise (a frame name, a runtime version), the
    normalizer in that SDK's wire-fixture IO helper is what needs fixing.
EOF
exit 1
