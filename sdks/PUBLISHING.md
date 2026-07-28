# Publishing the Sauron SDKs

Covers the three public registries: **npm**, **PyPI**, **pub.dev**.

| SDK | Directory | Registry | Package | Version |
| --- | --- | --- | --- | --- |
| Browser | `sdks/js` | npm | `@edraj/sauron-browser` | 1.0.0 |
| Node | `sdks/node` | npm | `@edraj/sauron-node` | 1.0.0 |
| Python | `sdks/python` | PyPI | `sauron-sdk` | 1.0.0 |
| Flutter | `sdks/flutter` | pub.dev | `sauron_flutter` | 1.2.0 |

`sdks/csharp` targets NuGet and is **not** covered here.

The registry package name and the **wire** SDK name are independent. The
envelope header still reports `sauron.javascript`, `sauron-node`,
`sauron-python`, `sauron.flutter` — renaming a package does not touch the
ingest contract or the dashboard.

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

## 1. Pre-flight (all SDKs)

Run from the repo root. Everything below is currently green.

```bash
(cd sdks/flutter && flutter analyze && flutter test)
(cd sdks/python  && python -m pytest -q)
(cd sdks/js      && npm ci && npm run typecheck && npm test && npm run build)
(cd sdks/node    && npm ci && npm run typecheck && npm test && npm run build)
```

Then **commit and tag**. `flutter pub publish` warns on a dirty git tree, and a
tag is the only way to reconstruct what a published artifact contained.

```bash
git commit -am "release: SDKs v1.0.0"
git tag sdk-v1.0.0
git push origin main --tags
```

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
pip install --index-url https://test.pypi.org/simple/ sauron-sdk==1.0.0
```

Then publish:

```bash
twine upload dist/*
```

Verify:

```bash
pip install sauron-sdk==1.0.0
python -c "import sauron; print(sauron.SDK_VERSION)"   # -> 1.0.0
```

---

## 3. npm — `@edraj/sauron-browser`, `@edraj/sauron-node`

`prepublishOnly` runs typecheck + tests + build in both packages, so a stale
`dist/` cannot be published.

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

- [ ] Update the wiki install snippets if the versions moved.
- [ ] `git push origin sdk-v1.0.0` if not already pushed.
- [ ] Smoke-test each published package against a real ingest endpoint.
- [ ] Announce in the release notes.

---

## Rollback

Publishing is close to irreversible on all three registries. Prefer shipping a
patch release over unpublishing.

| Registry | Window | Mechanism |
| --- | --- | --- |
| npm | 72 hours | `npm unpublish <pkg>@1.0.0`. After that, `npm deprecate` only. |
| PyPI | none | A version can never be re-uploaded. You can only *yank* it. |
| pub.dev | 7 days | `dart pub retract 1.0.0`. After that the version is permanent. |

---

## Cutting the next release

1. Bump the version in **both** places for each SDK — the manifest and the
   in-code constant. They are asserted against in the test suites, so a
   mismatch fails CI.

   | SDK | Manifest | Constant |
   | --- | --- | --- |
   | Browser | `sdks/js/package.json` | `sdks/js/src/utils.ts` (`SDK_VERSION`) |
   | Node | `sdks/node/package.json` | `sdks/node/src/transport.ts` (`SDK_VERSION`) |
   | Python | `sdks/python/pyproject.toml` | `sdks/python/sauron/_client.py` (`SDK_VERSION`) |
   | Flutter | `sdks/flutter/pubspec.yaml` | `sdks/flutter/lib/src/envelope.dart` (`kSauronSdkVersion`) |

2. Update the version assertions in `sdks/python/tests/test_golden.py`,
   `sdks/python/tests/test_envelope.py`, `sdks/node/test/transport.test.ts`,
   `sdks/node/test/envelope.test.ts`, `sdks/flutter/test/envelope_test.dart`.
3. Promote each `## Unreleased` CHANGELOG section to the new version + date.
4. Re-run the pre-flight, commit, tag, publish.
