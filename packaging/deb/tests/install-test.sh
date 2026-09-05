#!/usr/bin/env bash
# Install / upgrade / purge gate for the Sauron .deb packages.
#
#   packaging/deb/tests/install-test.sh [topdir]
#
# Runs as root inside a throwaway container of a target distribution. Builds the
# packages from stub artifacts (see make-stub-prebuilt.sh), then exercises the
# things that are only decidable at dpkg time and are invisible to every other
# gate in this repo: maintainer-script ordering, conffile ownership across an
# upgrade, secret generation and backfill, unit enablement, and purge cleanup.
#
# systemd does NOT need to be running. deb-systemd-invoke is guarded by
# [ -d /run/systemd/system ], so nothing is started here, while
# deb-systemd-helper (which creates the enablement symlinks) is NOT guarded and
# runs normally -- so enablement is still fully assertable.
set -uo pipefail

repo_root="$(git rev-parse --show-toplevel)"
topdir="${1:-${DEB_BUILD_TOPDIR:-$repo_root/build/deb}}"

fail=0
pass() { printf '  ok   %s\n' "$*"; }
die()  { printf '  FAIL %s\n' "$*" >&2; fail=1; }
sec()  { printf '\n== %s\n' "$*"; }

# assert_mode <path> <user:group> <mode>
assert_mode() {
    local p="$1" want_own="$2" want_mode="$3" got
    if [ ! -e "$p" ]; then die "$p does not exist"; return; fi
    got="$(stat -c '%U:%G %a' "$p")"
    if [ "$got" = "$want_own $want_mode" ]; then
        pass "$p is $got"
    else
        die "$p is $got, expected $want_own $want_mode"
    fi
}

assert_exists()  { [ -e "$1" ] && pass "$1 exists"      || die "$1 is missing"; }
assert_absent()  { [ ! -e "$1" ] && pass "$1 absent"    || die "$1 should not exist"; }

# ---------------------------------------------------------------------------
sec "build"
export DEB_BUILD_TOPDIR="$topdir"
stub="$(mktemp -d)"
"$repo_root/packaging/deb/tests/make-stub-prebuilt.sh" "$stub" >/dev/null || exit 1
"$repo_root/packaging/deb/build-deb.sh" --prebuilt "$stub" >/dev/null || {
    echo "build-deb.sh failed" >&2; exit 1; }
ls "$topdir"/*.deb >/dev/null 2>&1 || { echo "no .debs produced" >&2; exit 1; }
pass "four packages built"

# ---------------------------------------------------------------------------
sec "maintainer scripts are well-formed"
# The debhelper token is substituted by LITERAL STRING MATCH, comments included.
# A mention of it in prose is replaced too, which emits the autoscripts a second
# time and leaves the rest of that comment line dangling as a command. That
# produced a package that ran systemd-sysusers and systemd-tmpfiles twice and
# still built without one warning, so it is asserted here rather than reviewed.
#
# Checked at the SOURCE, where the rule is exact and unambiguous: the token must
# appear once per script, alone on its line. Counting expanded commands in the
# generated script instead does not work -- dh_installsystemd is deliberately
# invoked twice (once for the inspector, once for the rest), so a "no autoscript
# appears more than once" rule has legitimate exceptions, and a substring search
# also matches any comment that merely names the command.
src="$repo_root/packaging/deb/debian"
tok='#DEBHELPER#'
for f in "$src"/*.postinst "$src"/*.postrm "$src"/*.preinst "$src"/*.prerm; do
    [ -f "$f" ] || continue
    total=$(grep -Fc "$tok" "$f" || true)
    alone=$(grep -cxF "$tok" "$f" || true)
    if [ "$total" -ne 1 ] || [ "$alone" -ne 1 ]; then
        die "$(basename "$f"): the debhelper token appears $total time(s), $alone of them standalone; expected exactly 1 standalone and no other mention"
    fi
done

ctl="$(mktemp -d)"
for p in sauron sauron-server sauron-dashboard sauron-cli; do
    rm -rf "${ctl:?}/$p"; mkdir -p "$ctl/$p"
    dpkg-deb -e "$topdir/${p}"_*.deb "$ctl/$p" 2>/dev/null || continue
    for s in postinst prerm postrm preinst; do
        [ -f "$ctl/$p/$s" ] || continue
        sh -n "$ctl/$p/$s" || die "$p/$s is not valid shell"
        # debhelper stamps one header per inserted block, so this counts
        # insertions rather than mentions.
        n=$(grep -c '^# Automatically added by dh_installsysusers' "$ctl/$p/$s" || true)
        [ "$n" -le 1 ] || die "$p/$s expands the sysusers autoscript $n times"
    done
done
n=$(grep -c '^# Automatically added by dh_installsysusers' "$ctl/sauron/postinst" || true)
[ "$n" -eq 1 ] || die "sauron/postinst has $n sysusers autoscript(s), expected exactly 1"
[ "$fail" -eq 0 ] && pass "debhelper token used exactly once per script; no duplicated autoscripts"

# ---------------------------------------------------------------------------
sec "install"
apt-get install -y --no-install-recommends "$topdir"/*.deb >/dev/null 2>&1 \
    || { echo "apt-get install failed" >&2; apt-get install -y --no-install-recommends "$topdir"/*.deb; exit 1; }
pass "installed"

sec "layout"
for b in sauron-api sauron-ingest sauron-monitor sauron-alerts sauron-tier \
         sauron-inspector sauron-storesync sauron-migrate crebain sauron-symcli; do
    assert_exists "/usr/bin/$b"
done
for u in api ingest monitor tier migrate alerts inspector storesync; do
    assert_exists "/lib/systemd/system/sauron-$u.service"
done
assert_exists /usr/lib/sysusers.d/sauron.conf
assert_exists /usr/lib/tmpfiles.d/sauron.conf
assert_exists /usr/lib/sauron/libduckdb.so
assert_exists /etc/ld.so.conf.d/sauron.conf
assert_exists /etc/nginx/conf.d/sauron-dashboard.conf
assert_exists /usr/libexec/sauron/sauron-dashboard-config
assert_exists /usr/share/sauron/dashboard/index.html
assert_exists /usr/share/sauron/dashboard/config.template.js
# The RPM's %install deletes the built config.js and ships only the template, so
# the postinst-generated one is never shadowed by a stale build artifact. Assert
# the .deb does the same: the stub tree deliberately contains a config.js.
assert_exists /usr/share/sauron/dashboard/config.js
grep -q 'API_BASE_URL' /usr/share/sauron/dashboard/config.template.js \
    && pass "config.template.js kept its placeholders" \
    || die "config.template.js lost its placeholders"
grep -q '\${API_BASE_URL}' /usr/share/sauron/dashboard/config.js \
    && die "config.js still holds an unsubstituted placeholder (postinst did not run?)" \
    || pass "config.js was generated from the template"

sec "unit Documentation= URIs are shipped, uncompressed"
# dh_compress gzips anything over 4k in /usr/share/doc by default, which turns
# SETUP.md into SETUP.md.gz and leaves every unit's
# Documentation=file:///usr/share/doc/sauron-server/SETUP.md pointing at nothing.
# `systemctl status` shows the dead link and nothing else complains.
#
# Asserted against the PACKAGE CONTENTS, not the installed filesystem. The
# ubuntu:22.04 base image configures dpkg with path-exclude=/usr/share/doc/*, so
# a correct package legitimately unpacks no documentation there at all and a
# filesystem check fails on Ubuntu while passing on Debian -- reporting an image
# policy as a packaging bug. What this package controls, and what dh_compress
# would break, is whether the file is in the .deb under the name the unit names.
docs="$(grep -ho '^Documentation=file://[^ ]*' /lib/systemd/system/sauron-*.service \
        | sed 's|^Documentation=file://||' | sort -u)"
if [ -z "$docs" ]; then
    die "no Documentation=file:// URIs found in the installed units"
else
    shipped="$(dpkg-deb -c "$topdir"/sauron-server_*.deb | awk '{print $6}')"
    for f in $docs; do
        if printf '%s\n' "$shipped" | grep -qxF ".$f"; then
            pass "sauron-server ships $f"
        else
            die "units reference $f but sauron-server does not ship it (compressed by dh_compress?)"
        fi
    done
fi

sec "service account"
id sauron >/dev/null 2>&1 && pass "sauron account exists" || die "sauron account was not created"

sec "conffile ownership after install"
# The RPM declares %attr(0640,root,sauron); dpkg cannot, because it unpacks before
# any maintainer script runs. The postinst applies it instead.
for f in api ingest monitor tier alerts inspector storesync; do
    assert_mode "/etc/sauron/$f.env" "root:sauron" 640
done
assert_mode /etc/sauron/sauron.env root:sauron 640

sec "secrets generated on first install"
assert_mode /etc/sauron/secret.env root:sauron 640
grep -q '^JWT_SECRET=.\+'        /etc/sauron/secret.env && pass "JWT_SECRET present"        || die "JWT_SECRET missing"
grep -q '^NOTIFY_SECRET_KEY=.\+' /etc/sauron/secret.env && pass "NOTIFY_SECRET_KEY present" || die "NOTIFY_SECRET_KEY missing"

sec "unit enablement"
# Six daemons enabled; the inspector deliberately not (its extra connection pool
# pushes a stock Postgres past max_connections -- see 50-sauron.preset).
# sauron-migrate is static, so it gets no symlink and must not be treated as a
# failure to enable.
for u in api ingest monitor alerts tier storesync; do
    assert_exists "/etc/systemd/system/multi-user.target.wants/sauron-$u.service"
done
assert_absent /etc/systemd/system/multi-user.target.wants/sauron-inspector.service
assert_absent /etc/systemd/system/multi-user.target.wants/sauron-migrate.service

# ---------------------------------------------------------------------------
sec "upgrade preserves secrets and REPAIRS broken conffile ownership"
# A synthetic v2 of sauron-server whose api.env content DIFFERS, so dpkg actually
# replaces the (unmodified) conffile.
#
# The ownership is BROKEN on purpose first, and that is the whole point of this
# test. dpkg does NOT reset a conffile's owner and mode when it replaces the
# content -- measured on both targets: it keeps whatever is on disk. So simply
# asserting "still root:sauron 640 after an upgrade" is vacuous; it passes even
# if the postinst only ever fixes ownership on first install, because dpkg's
# preservation supplies the right answer. Verified by mutation: restricting the
# fixup to first install left that weaker assertion green.
#
# Breaking it first reproduces the real failure the unconditional re-run exists
# for: a first configure where the `sauron` group did not exist yet (base package
# not configured), chgrp failed, and the file was left root:root 0644 -- which
# dpkg then preserves forever. The upgrade must repair it.
jwt_before="$(sed -n 's/^JWT_SECRET=//p' /etc/sauron/secret.env)"
chown root:root /etc/sauron/api.env
chmod 0644 /etc/sauron/api.env
pass "deliberately broke /etc/sauron/api.env to root:root 644"
v2="$(mktemp -d)"
dpkg-deb -R "$topdir"/sauron-server_*.deb "$v2/pkg"
oldver="$(awk '/^Version:/{print $2}' "$v2/pkg/DEBIAN/control")"
sed -i "s/^Version: .*/Version: ${oldver}+test2/" "$v2/pkg/DEBIAN/control"
echo "# upgrade marker" >> "$v2/pkg/etc/sauron/api.env"
# The conffile's recorded md5 must match the new content or dpkg treats the file
# as locally modified and prompts instead of replacing it.
newmd5="$(md5sum "$v2/pkg/etc/sauron/api.env" | cut -d' ' -f1)"
sed -i "s|^[0-9a-f]* etc/sauron/api.env$|$newmd5 etc/sauron/api.env|" "$v2/pkg/DEBIAN/md5sums"
dpkg-deb -b "$v2/pkg" "$v2/sauron-server-v2.deb" >/dev/null
DEBIAN_FRONTEND=noninteractive dpkg -i --force-confnew "$v2/sauron-server-v2.deb" >/dev/null 2>&1 \
    || die "upgrade install failed"

grep -q '# upgrade marker' /etc/sauron/api.env \
    && pass "conffile was actually replaced (the case under test)" \
    || die "conffile was NOT replaced -- this assertion is not testing anything"
# The repair. Fails if the ownership fixup is ever restricted to first install.
assert_mode /etc/sauron/api.env root:sauron 640
jwt_after="$(sed -n 's/^JWT_SECRET=//p' /etc/sauron/secret.env)"
[ -n "$jwt_before" ] && [ "$jwt_before" = "$jwt_after" ] \
    && pass "JWT_SECRET survived the upgrade" \
    || die "JWT_SECRET changed across upgrade ($jwt_before -> $jwt_after)"

# ---------------------------------------------------------------------------
sec "NOTIFY_SECRET_KEY backfill (upgrade from a pre-fail-closed build)"
# Must copy the EXISTING JWT_SECRET, never generate a fresh key: older builds
# derived the channel cipher from JWT_SECRET, so a new key boots cleanly and then
# fails every delivery with "secret decrypt failed" -- silent, total loss of every
# configured channel.
printf 'JWT_SECRET=deadbeefcafe\n' > /etc/sauron/secret.env
chmod 0640 /etc/sauron/secret.env
dpkg-reconfigure sauron-server >/dev/null 2>&1 || dpkg --configure sauron-server >/dev/null 2>&1
got="$(sed -n 's/^NOTIFY_SECRET_KEY=//p' /etc/sauron/secret.env)"
[ "$got" = "deadbeefcafe" ] \
    && pass "backfilled NOTIFY_SECRET_KEY from the existing JWT_SECRET" \
    || die "backfill wrote '$got', expected 'deadbeefcafe'"

sec "backfill refuses to invent a key when JWT_SECRET is absent"
# An operator who moved JWT_SECRET into api.env leaves no JWT_SECRET= line here.
# Writing an empty NOTIFY_SECRET_KEY= would be self-masking: sauron-core's var()
# filters empty strings, so the daemons still refuse to start, and the
# `! grep -q '^NOTIFY_SECRET_KEY='` guard then matches the empty assignment
# forever. Nothing must be written.
printf 'SOMETHING_ELSE=1\n' > /etc/sauron/secret.env
chmod 0640 /etc/sauron/secret.env
dpkg --configure sauron-server >/dev/null 2>&1
if grep -q '^NOTIFY_SECRET_KEY=' /etc/sauron/secret.env; then
    die "wrote a NOTIFY_SECRET_KEY with no JWT_SECRET to derive it from"
else
    pass "wrote nothing, as intended"
fi

# ---------------------------------------------------------------------------
sec "purge"
apt-get purge -y sauron-server sauron-dashboard sauron-cli sauron >/dev/null 2>&1 \
    || die "purge failed"
assert_absent /etc/sauron/secret.env
assert_absent /usr/share/sauron/dashboard/config.js
assert_absent /etc/sauron/api.env
assert_absent /usr/bin/sauron-api
assert_absent /usr/lib/sauron/libduckdb.so

# ---------------------------------------------------------------------------
echo
if [ "$fail" -ne 0 ]; then
    echo "DEB PACKAGING GATE FAILED"
    exit 1
fi
echo "DEB PACKAGING GATE OK"
