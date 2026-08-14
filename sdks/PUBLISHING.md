# Publishing the Sauron SDKs

Covers the three public registries with automated publish steps below: **npm**,
**PyPI**, **pub.dev**. `sdks/csharp` targets NuGet; its manual publish flow is
not covered here, though its version is tracked in the table below like the
other four.

---

## Versioning policy

Each SDK versions **independently** — there is no shared "SDK release" number
and no requirement that a release touch all five packages. A given SDK's
version bumps only when *that* SDK's own code, behavior, or dependencies
change; the other four sit untouched at whatever version they last shipped.

This replaces an earlier lockstep policy, where all five bumped together on
every release whether or not each had actually changed. Bumping an SDK that
has nothing new publishes a release with an empty changelog, which is noise
for consumers — and it undermined the version number as a signal: under
lockstep, a consumer watching `@edraj/sauron-node` move from one version to
the next couldn't tell whether anything in the Node package had actually
changed, or whether it moved only because some other SDK forced a
synchronized release.

Under independent versioning the version number is trustworthy again: if
`sauron_flutter` moves from 1.2.0 to 1.3.0, something in the Flutter SDK
changed — check its own `CHANGELOG.md` for what. A consumer who only depends
on `sauron-sdk` (Python) never sees a version bump unless the Python package
itself changed.

Current versions:

| SDK | Directory | Registry | Package | Version | Wire `sdk.name` |
| --- | --- | --- | --- | --- | --- |
| Browser | `sdks/js` | npm | `@edraj/sauron-browser` | 1.4.1 | `sauron.javascript` |
| Node | `sdks/node` | npm | `@edraj/sauron-node` | 1.4.0 | `sauron-node` |
| Python | `sdks/python` | PyPI | `sauron-sdk` | 1.4.0 | `sauron-python` |
| C# | `sdks/csharp` | NuGet | `Sauron` | 1.4.0 | `sauron-dotnet` |
| Flutter | `sdks/flutter` | pub.dev | `sauron_flutter` | 1.7.0 | `sauron.flutter` |

The registry package name and the **wire** SDK name are independent — the
envelope header reports the wire name above no matter what the package is
called on npm/PyPI/NuGet/pub.dev; renaming a package does not touch the ingest
contract or the dashboard.

---

## 0. One-time account setup

**npm** — the `@edraj` scope must exist before the first publish.

1. Create the free org at <https://www.npmjs.com/org/create>, name it `edraj`.
2. `npm login`
3. Enable 2FA. If you use 2FA-for-publish you will be prompted for an OTP, or
   pass `--otp=123456`.

**PyPI**

1. Account at <https://pypi.org/account/register/>.
2. Either an API token (<https://pypi.org/manage/account/token/>) written to
   `~/.pypirc`, or — preferred — GitHub Actions Trusted Publishing.

**pub.dev**

1. A Google account. `flutter pub login` opens the browser flow.
2. Optional but recommended: claim a verified publisher (e.g. `edraj.com`) at
   <https://pub.dev/create-publisher> and add `publisher: edraj.com` to
   `sdks/flutter/pubspec.yaml`. Without it the package publishes under your
   personal account.

---

## 1. Pre-flight

Run the pre-flight for whichever SDK(s) you're actually releasing — under
independent versioning there's no reason to build or test an SDK whose code
hasn't changed just because you're publishing a different one.

```bash
(cd sdks/flutter && flutter analyze && flutter test)
(cd sdks/python  && python -m pytest -q)
(cd sdks/js      && npm ci && npm run typecheck && npm test && npm run build)
(cd sdks/node    && npm ci && npm run typecheck && npm test && npm run build)
```

Then **commit and tag that SDK only**. `flutter pub publish` warns on a dirty
git tree, and a tag is the only way to reconstruct what a published artifact
contained. Tag as `<sdk-dir>-v<version>`:

```bash
git commit -am "release(js): v1.2.0"
git tag js-v1.2.0
git push origin main --tags
```

Do not fold unrelated SDKs' in-flight changes into the same commit/tag just
because they happen to be dirty at the same time — each SDK's release history
should trace only its own changes.

---

## 2. PyPI — `sauron-sdk`

`build` and `twine` are **not** stdlib and are not installed by default on
Fedora — `python -m build` fails with `No module named build` until you install
them. Either put them on the user path once:

```bash
python -m pip install --user build twine
```

or keep them in a project-local venv (`.venv` is gitignored), which is what CI
should do:

```bash
cd sdks/python
python -m venv .venv
.venv/bin/pip install build twine
```

Then build. Prefix with `.venv/bin/` if you took the venv route.

```bash
cd sdks/python
rm -rf dist build *.egg-info
python -m build
twine check dist/*
```

Rehearse against TestPyPI first (optional, recommended for a first release):

```bash
twine upload --repository testpypi dist/*
pip install --index-url https://test.pypi.org/simple/ sauron-sdk==1.2.0
```

Then publish:

```bash
twine upload dist/*
```

Verify:

```bash
pip install sauron-sdk==1.2.0
python -c "import sauron; print(sauron.SDK_VERSION)"   # -> 1.2.0
```

---

## 3. npm — `@edraj/sauron-browser`, `@edraj/sauron-node`

`prepublishOnly` runs typecheck + tests + build in both packages, so a stale
`dist/` cannot be published. Publish only the package(s) that actually changed
— they are independent releases even though the commands sit next to each
other here.

```bash
cd sdks/js   && npm publish --access public
cd ../node   && npm publish --access public
```

`--access public` is required for a scoped package's first publish (also set
via `publishConfig` in both manifests, so the flag is belt-and-braces).

Verify:

```bash
npm view @edraj/sauron-browser version
npm view @edraj/sauron-node version
```

---

## 4. pub.dev — `sauron_flutter`

```bash
cd sdks/flutter
flutter pub publish --dry-run   # must report 0 warnings on a clean tree
flutter pub publish
```

Verify at <https://pub.dev/packages/sauron_flutter>. The pub.dev score takes a
few minutes to appear.

---

## 5. Post-publish

- [ ] Update the wiki install snippets if the version moved.
- [ ] `git push origin <sdk-dir>-v<version>` if not already pushed.
- [ ] Smoke-test the published package against a real ingest endpoint.
- [ ] Announce in the release notes.

---

## Rollback

Publishing is close to irreversible on all three registries. Prefer shipping a
patch release over unpublishing.

| Registry | Window | Mechanism |
| --- | --- | --- |
| npm | 72 hours | `npm unpublish <pkg>@<version>`. After that, `npm deprecate` only. |
| PyPI | none | A version can never be re-uploaded. You can only *yank* it. |
| pub.dev | 7 days | `dart pub retract <version>`. After that the version is permanent. |

---

## Cutting the next release

Pick the **one** SDK you're releasing. Nothing here touches the other four —
that's the whole point of independent versioning.

1. Bump the version in **both** places for that SDK — the manifest and the
   in-code constant. They are asserted against in that SDK's own test suite,
   so a mismatch fails its CI.

   | SDK | Manifest | Constant |
   | --- | --- | --- |
   | Browser | `sdks/js/package.json` | `sdks/js/src/utils.ts` (`SDK_VERSION`) |
   | Node | `sdks/node/package.json` | `sdks/node/src/transport.ts` (`SDK_VERSION`) |
   | Python | `sdks/python/pyproject.toml` | `sdks/python/sauron/_client.py` (`SDK_VERSION`) |
   | C# | `sdks/csharp/Sauron/Sauron.csproj` (`<Version>`) | `sdks/csharp/Sauron/Envelope.cs` (`SauronSdkMeta.Version`) |
   | Flutter | `sdks/flutter/pubspec.yaml` | `sdks/flutter/lib/src/envelope.dart` (`kSauronSdkVersion`) |

2. Update the version assertions in that SDK's own tests — and only that
   SDK's:

   | SDK | Test files |
   | --- | --- |
   | Browser | `sdks/js/test/envelope.test.ts` |
   | Node | `sdks/node/test/transport.test.ts`, `sdks/node/test/envelope.test.ts` |
   | Python | `sdks/python/tests/test_golden.py`, `sdks/python/tests/test_envelope.py` |
   | C# | `sdks/csharp/Sauron.Tests/EnvelopeGoldenTests.cs`, `sdks/csharp/Sauron.Tests/TransportTests.cs` |
   | Flutter | `sdks/flutter/test/envelope_test.dart` |

3. Promote that SDK's `## Unreleased` (or undated top) CHANGELOG section to
   the new version + date.
4. Re-run that SDK's pre-flight, commit, tag, publish. Leave the other four
   SDKs' manifests, constants, and CHANGELOG files untouched — they didn't
   change, so their versions don't move.
