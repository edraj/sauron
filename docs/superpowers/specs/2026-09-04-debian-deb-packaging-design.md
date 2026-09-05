# Sauron Debian Packaging (`.deb`) — Design

**Date:** 2026-09-04
**Status:** Approved (design)
**Scope:** Native `.deb` packaging of the Sauron backend + dashboard for Debian-family
distributions, wired into the existing `Release` workflow alongside the RPMs.

Supersedes the "`.deb` packaging" non-goal in
[2026-07-16-sauron-rpm-packaging-design.md](2026-07-16-sauron-rpm-packaging-design.md).

---

## 1. Goal

`Release` currently emits RPMs for two targets (`fedora`, `el9`) and attaches them to a GitHub
release. Emit `.deb` artifacts for two Debian-family targets from the same run, from the same
compiled binaries, with the same operator-facing filesystem layout — so `SETUP.md` and `INSTALL.md`
describe one product, not two.

**Non-goals (explicitly out of scope for this cut):**

- APT repository publishing (`reprepro`, `aptly`), repository signing.
- Debian source packages (`.dsc` / `debian/source` uploads), mentors.debian.net, Debian archive
  submission, lintian-clean status.
- `arm64` / any architecture other than `amd64`.
- Backporting to distributions older than the two targets below.

## 2. Decisions (locked)

| Question | Decision | Rationale |
|---|---|---|
| Targets | **`debian:12` + `ubuntu:22.04`**, two native legs | glibc 2.36 and 2.35 respectively — neither covers the other, verified with `ldd --version` |
| Build method | **`debhelper` + `dpkg-buildpackage`** | `dh_shlibdeps` derives `Depends:` from the actual linked `.so` set; hand-written control can only guess |
| Compilation | **Never inside `debian/rules`** | `build-deb.sh` owns it, exactly as `build-rpm.sh` does; all four `dh_auto_*` are no-ops |
| Package split | **Identical to the RPM**: `sauron`, `sauron-server`, `sauron-dashboard`, `sauron-cli` | one mental model per format |
| Unit policy on install | **Debian policy — enable *and* start** | diverges from the RPM deliberately; see §6 |
| Shared assets | **Read from `packaging/rpm/`**, not copied | units/sysusers/tmpfiles/env/nginx have one copy, so they cannot drift |

## 3. Verified facts this design rests on

Each was checked in the actual target containers, not assumed. Re-check them before changing the
corresponding decision.

| Fact | `debian:12` | `ubuntu:22.04` |
|---|---|---|
| `debhelper` version | 13.11.4 | 13.6ubuntu1 |
| `dh_installsysusers` in the `dh` sequence | **compat ≥ 14 only** | **compat ≥ 14 only** |
| `Sequence/installsysusers.pm` add-on (`dh --with installsysusers`) | present | **absent** |
| `dh_installtmpfiles` in the `dh` sequence | compat ≥ 13 ✓ | compat ≥ 13 ✓ |
| `postinst-systemd-start` autoscript | `deb-systemd-invoke start … \|\| true` | same |
| `deb-systemd-invoke` skips non-enabled units | yes — `is-enabled` gate | same |
| glibc | 2.36 | 2.35 |
| `valkey` package | **absent** | **absent** |
| `nginx`, `postgresql`, `redis-server`, `adduser`, `passwd` | available | available |

Two of these directly shape the implementation:

1. **`dh_installsysusers` is not in the compat-13 sequence, and jammy has no add-on for it.**
   Compat 14 is not available on either target, so `--with installsysusers` is not portable.
   The *program* exists on both, so `debian/rules` calls it explicitly from
   `override_dh_install`.

2. **`deb-systemd-invoke start` is `|| true` on both.** A daemon that fails to start on an
   unconfigured host therefore leaves the unit in `failed` but lets `dpkg` configure the package
   cleanly. This is what makes the "enable and start" decision in §6 safe; the earlier concern
   that it would leave a half-configured `dpkg` state was wrong.

Also relevant: `backend/Cargo.toml`'s release profile sets `strip = true`, so binaries arrive
already stripped and `dh_strip` produces no `-dbgsym` package. `--no-automatic-dbgsym` is passed
anyway so this stays true if the profile changes.

## 4. Layout

```
packaging/deb/
  build-deb.sh              # mirrors build-rpm.sh: --prebuilt DIR, --distro SUFFIX
  debian/
    control                 # source + 4 binary packages
    rules                   # dh $@ with the overrides in §5
    changelog.in            # version + distro suffix substituted by build-deb.sh
    compat                  # 13
    source/format           # 3.0 (native)
    sauron.install
    sauron.postinst  sauron.postrm
    sauron-server.install
    sauron-server.postinst  sauron-server.prerm  sauron-server.postrm
    sauron-dashboard.install  sauron-dashboard.postinst
    sauron-cli.install
  INSTALL.md
```

Systemd units, sysusers, tmpfiles, the nine `*.env` files, the nginx vhost and
`sauron-dashboard-config` are **read out of `packaging/rpm/`**. There is exactly one copy of each.
(Hoisting them to `packaging/common/` would be tidier but churns every `SourceN` line in
`sauron.spec` for no functional gain; deliberately not done here.)

`packaging/rpm/binaries.txt` stays the single source of truth for what ships, gaining a fourth
reader (`debian/*.install` generation) alongside `release.yml`, `build-rpm.sh` and `sauron.spec`.

## 5. Filesystem layout and `debian/rules`

**Operator-visible paths are byte-identical to the RPM**, so `SETUP.md` needs no Debian fork:

| Path | Owner package |
|---|---|
| `/etc/sauron/{sauron,api,ingest,monitor,tier,alerts,inspector,storesync,dashboard}.env` | base + server + dashboard |
| `/etc/sauron/secret.env` | created by `postinst`, not shipped |
| `/usr/bin/sauron-*` | server |
| `/usr/bin/{crebain,sauron-symcli}` | cli |
| `/usr/share/sauron/dashboard/` | dashboard |
| `/usr/libexec/sauron/sauron-dashboard-config` | dashboard |
| `/etc/nginx/conf.d/sauron-dashboard.conf` | dashboard |
| `/var/lib/sauron{,/cold}` | base |
| `/lib/systemd/system/sauron-*.service` | server |

Two paths differ from the RPM, both loader-internal and never named in operator docs:

- **`/usr/lib/sauron/libduckdb.so`** (RPM: `/usr/lib64/sauron/`). Not multiarch — the path is
  resolved by the `ld.so.conf.d` drop-in, so `$(DEB_HOST_MULTIARCH)` templating buys nothing.
- `/usr/libexec` is blessed by Debian Policy 4.6.2 (bookworm). On jammy (policy 4.6.0) it is a
  lintian nit only, and lintian-clean is a stated non-goal — keeping the path identical to the
  RPM is worth more than the warning.

`debian/rules` overrides, and why each exists:

| Override | Reason |
|---|---|
| `dh_auto_configure`, `dh_auto_build`, `dh_auto_test` → no-op | `build-deb.sh` owns compilation; `dh` must never invoke cargo |
| `dh_auto_install` → stage from the prebuilt tree | same tree shape in `--prebuilt` and local-compile mode |
| `dh_install` → also run `dh_installsysusers` | not in the compat-13 sequence (§3) |
| `dh_shlibdeps` → `-l …/usr/lib/sauron -- --ignore-missing-info` | the vendored `libduckdb.so` is private and carries no dependency info; without this the `sauron-server` build fails |
| `dh_installsystemd` → default for 7 units, `--no-enable --no-start` for `sauron-inspector` | §6 |
| `dh_strip` → `--no-automatic-dbgsym` | binaries are pre-stripped; keeps that true if the profile changes |
| `dh_dwz` → no-op | no debuginfo to compress |

`Depends:` differences forced by the target distributions:

- `Recommends: valkey` → **`Recommends: redis-server`** (no `valkey` package on either target).
- `Requires: shadow-utils` → satisfied by `${misc:Depends}` from `dh_installsysusers`.
- `Requires: nginx` → `Depends: nginx`.

## 6. Unit enablement — the one deliberate divergence

The RPM's `%systemd_post` runs `systemctl preset` on first install: it reads
`packaging/rpm/systemd/50-sauron.preset`, **enables** six daemons, explicitly **disables**
`sauron-inspector`, and **starts nothing**.

The `.deb` follows Debian policy instead: `dh_installsystemd`'s default `postinst` enables **and
starts**. On an unconfigured host every daemon will fail to start and land in `failed`; because
`deb-systemd-invoke` is `|| true` (§3), `dpkg` still configures the package cleanly. This is
documented in `packaging/deb/INSTALL.md` so the failed units read as expected, not broken.

`sauron-inspector` is the exception and gets **`--no-enable` only** (not `--no-start`). Its
exclusion in the RPM preset is a *technical* constraint, not a policy preference: its extra
4-connection pool pushes a stock Postgres past `max_connections`, and the resulting failures
surface as `sauron-api` 500s rather than as inspector errors. That reasoning is
distribution-independent, so it carries over.

`--no-enable` is sufficient and `--no-start` would be actively wrong, because
`deb-systemd-invoke` gates on `systemctl is-enabled` and prints *"$unit is a disabled or a static
unit, not starting it"* (verified, §3). So a `--no-enable` unit is never started on install, and
on upgrade is restarted **only if the operator enabled and started it** — which is exactly RPM's
preset-disable plus `%systemd_postun_with_restart` try-restart semantics. Adding `--no-start`
would drop the upgrade restart and leave an operator-enabled inspector running old code.

`sauron-migrate.service` is static (no `[Install]`) and is pulled in by every daemon's
`Requires=`. The same `is-enabled` gate makes it a no-op for both start and restart, and
`deb-systemd-helper enable` creates no symlinks for a unit with no `[Install]`. It is therefore
listed in the normal group for symmetry with the RPM's `%post`/`%preun`, costing nothing —
the same reasoning the spec's own comment gives for listing it there.

## 7. Conffiles and ownership — the part that does not translate

RPM `%config(noreplace)` ≡ dpkg's default conffile behaviour for anything under `/etc`. Free.

**`%attr(0640,root,sauron)` has no dpkg equivalent.** dpkg unpacks files *before* `postinst`
runs, so the `sauron` group does not exist at unpack time (and a non-root build host cannot set
arbitrary owners anyway). Therefore:

- the `/etc/sauron/*.env` conffiles ship `0644 root:root`;
- `sauron-server.postinst` applies `chgrp sauron` + `chmod 0640` **on every `configure`**, not
  just first install.

**Measured, contradicting the obvious assumption:** dpkg does *not* restore package permissions
when it replaces an unmodified conffile on upgrade. It rewrites the content and **keeps the
existing owner and mode** — verified on `debian:12` with a synthetic package
(`root:testgrp 640` before upgrade, `root:testgrp 640` after, content replaced).

So the unconditional re-run is a **repair**, not a defence against dpkg. What it repairs is the
case where it previously ran and *failed*: if the base package had not been configured yet the
`sauron` group did not exist, `chgrp` took the warning branch, and the file is left
`root:root 0644` — which dpkg then faithfully preserves forever. Without the re-run there is no
second chance and the daemons (`User=sauron`) never regain read access to their own config.

This distinction has teeth for the test suite: an assertion that merely checks ownership *after*
an upgrade passes even when the fixup is restricted to first install, because dpkg's preservation
supplies the right answer. §10 test 6 therefore breaks the ownership deliberately before
upgrading.

`%ghost` files have no equivalent either:

- `/etc/sauron/secret.env` — created by `sauron-server.postinst`, removed in `postrm purge`.
- `/usr/share/sauron/dashboard/config.js` — generated by `sauron-dashboard.postinst` from
  `config.template.js`, removed in `postrm purge`.

The RPM's `%post server` secret logic ports across verbatim, and must sit **before** the
`#DEBHELPER#` token so secrets exist before `deb-systemd-invoke start` runs:

1. First install only: generate `JWT_SECRET` and `NOTIFY_SECRET_KEY`.
2. Upgrade backfill: if `secret.env` exists with no `NOTIFY_SECRET_KEY=` line, copy the value of
   the existing `JWT_SECRET`. **Never generate a fresh key here** — older builds derived the
   channel cipher from `JWT_SECRET`, so a new key boots cleanly and then fails every delivery with
   "secret decrypt failed": silent, total loss of every configured channel.
3. If `JWT_SECRET=` is absent (an operator moved it into `api.env`), write **nothing** and warn.
   Writing an empty `NOTIFY_SECRET_KEY=` is self-masking: `sauron-core`'s `var()` filters empty
   strings, so the daemons still refuse to start, and the `grep -q '^NOTIFY_SECRET_KEY='` guard
   then matches the empty assignment forever.

## 8. Versioning

`build-deb.sh` generates `debian/changelog` from `changelog.in` using
`<Cargo.toml version>-1~<distro suffix>`:

- `1.8.1-1~deb12`
- `1.8.1-1~ubuntu22.04`

The suffix is **mandatory, not cosmetic**: without it both legs emit
`sauron-server_1.8.1-1_amd64.deb` and silently overwrite each other when `release.yml` merges both
artifacts into `dist/`. Within a single distribution the version still increases monotonically
across releases (`1.8.1-1~deb12` → `1.8.2-1~deb12`), so upgrades resolve correctly.

`build-deb.sh` asserts `backend/Cargo.toml` and `packaging/rpm/sauron.spec` agree on the version,
matching `build-rpm.sh`'s existing guard — the two formats must not ship different versions from
one commit.

## 9. `release.yml` changes

- **`build`** gains a `family: rpm | deb` matrix field and two new legs (`deb12`, `ubuntu2204`).
  Only *"Install build toolchain"* and *"Install Node 22"* branch on it; everything after
  (rustup, checkout, sccache, `rust-cache`, libduckdb cache, compile, assemble, upload) is shared,
  so **all four legs share one sccache** and the deb legs land largely warm.
- **`package-deb`** — new job mirroring `package`: installs `debhelper`/`dpkg-dev`/`fakeroot`
  only (no compiler), downloads `prebuilt-<target>`, runs `build-deb.sh --prebuilt`, uploads
  `debs-<target>`.
- **`release`** gains `debs-*` to its `download-artifact` pattern and `-o -name '*.deb'` to its
  `find`.
- `version` and `sbom` are unchanged.

## 10. Testing

**`packaging-deb` CI job** (new, in `ci.yml`, added to `ci-complete`'s `needs`) — a container gate
that builds the four `.deb`s from stub binaries and then, on both target images, runs
install → upgrade → purge, asserting:

1. every path in §5 exists with the expected mode;
2. `/etc/sauron/*.env` are `0640 root:sauron` **after** configure;
3. `secret.env` is generated on first install with both keys;
4. `secret.env` is **not** regenerated on upgrade (the values survive);
5. the `NOTIFY_SECRET_KEY` backfill copies `JWT_SECRET` when the key is missing, and writes
   nothing + warns when `JWT_SECRET` is absent;
6. conffile ownership is **repaired** by an upgrade after being deliberately broken — asserting
   only that it "survives" an upgrade is vacuous, because dpkg preserves it anyway (§7);
7. enablement statefiles: six daemons enabled, `sauron-inspector` not;
8. `purge` removes `secret.env`, `config.js` and `/etc/sauron`.

`deb-systemd-invoke` self-guards on `[ -d /run/systemd/system ]`, so a plain (non-systemd)
container install is clean and deterministic — no systemd-in-docker needed for this gate.

**Parity test** — asserts `packaging/rpm/binaries.txt` ≡ the binaries in `sauron.spec`'s `%files`
≡ the binaries in `debian/*.install`. The repo already relies on cross-source parity tests for
exactly this drift shape (`filter-registry-parity.test.ts`, `wiki_catalog.rs`), and a binary that
reaches one packaging format but not the other is invisible to every other gate.

**`migrations` job check 5** is extended to resolve `packaging/…` paths referenced by
`build-deb.sh` and `debian/rules`, not just `sauron.spec`. That check exists because
`50-sauron.preset` was added to `%install` untracked and killed the release build; the deb tree
has the same exposure.

## 11. Risks

| Risk | Mitigation |
|---|---|
| `dh_shlibdeps` fails on the vendored private `libduckdb.so` | proved in a throwaway container before the plan is written; `-l` + `--ignore-missing-info` |
| Conffile ownership reverts on upgrade | `postinst` re-applies on every `configure`; asserted by test 6 |
| Two legs double the release build cost | shared sccache across all four legs; deb legs compile the same workspace the rpm legs just cached |
| Debian and Ubuntu `Depends:` names diverge further over time | `dh_shlibdeps` derives the library deps; only the four hand-written ones (`nginx`, `redis-server`, `postgresql`, `adduser`) can rot, and the `packaging-deb` job installs on both |
