#!/usr/bin/env python3
"""Fill licences into a syft-generated CycloneDX SBOM from `cargo metadata`.

syft reads `backend/Cargo.lock` to enumerate the crate graph, and a lockfile
records names, versions and checksums -- no licence field exists in it. So
every component syft emits for a Rust workspace has no `licenses` entry at
all. Measured on sauron 1.8.1: 487 components, 487 without a licence.

Ingested into Dependency-Track that reads as "487 dependencies of unknown
licence", which is indistinguishable from 487 genuinely unlicensed
dependencies and buries any real finding.

The licence is not missing, only unread: every crate declares it in its own
Cargo.toml, and `cargo metadata` reports exactly that. This joins the two by
(name, version) and writes the result back in CycloneDX form.

Why cargo metadata rather than the crates.io API:

  * It is the same manifest the crate was published with, not a re-derivation.
  * No rate limit. crates.io throttles bulk anonymous lookups hard, and ~490
    serial requests takes minutes and fails intermittently in CI.
  * It covers the workspace's own crates. sauron's 25 first-party crates are
    not published to any registry, so an API-based enricher cannot resolve
    them at all -- they would stay unlicensed, and they are precisely the ones
    whose licence matters most here (AGPL-3.0-only, inherited from
    [workspace.package]).

Usage:
    cargo metadata --manifest-path backend/Cargo.toml \
        --format-version 1 --locked > cargo-meta.json
    ./fill-cargo-licences.py sauron-1.8.1.cdx.json cargo-meta.json
"""
from __future__ import annotations

import json
import re
import sys
import urllib.request

SPDX_URL = ("https://raw.githubusercontent.com/spdx/license-list-data/"
            "main/json/licenses.json")


def spdx_ids() -> set[str]:
    """The official SPDX identifier set.

    Needed to decide between CycloneDX's `id`, `expression` and `name` fields.
    Crates do emit things that are not SPDX ids -- the pre-2019 `MIT/Apache-2.0`
    form is still common, and a few use free text. Writing a non-identifier
    into `id` or `expression` produces a document Dependency-Track may reject
    or silently fail to match, which is worse than an honest free-text name.
    """
    try:
        with urllib.request.urlopen(SPDX_URL, timeout=30) as r:
            d = json.load(r)
    except Exception as e:                                    # noqa: BLE001
        print(f"  warning: could not fetch the SPDX list ({e});"
              " falling back to free-text names", file=sys.stderr)
        return set()
    return ({l["licenseId"] for l in d.get("licenses", [])} |
            {e["licenseExceptionId"] for e in d.get("exceptions", [])})


def classify(lic: str, ids: set[str]) -> tuple[str, str]:
    """-> ("id" | "expression" | "name", value). Only claims SPDX when it is."""
    s = " ".join(lic.split())
    if not ids:
        return ("name", s)
    # The pre-SPDX `MIT/Apache-2.0` form, still emitted by older crates.
    if "/" in s and " " not in s:
        parts = [p.strip() for p in s.split("/")]
        if len(parts) > 1 and all(p in ids for p in parts):
            s = " OR ".join(parts)
    terms = re.split(r"\s+(?:OR|AND)\s+", s)

    def base(t: str) -> str:
        return re.split(r"\s+WITH\s+", t.strip().strip("()"))[0].strip()

    if all(base(t) in ids for t in terms):
        return ("expression", s) if (len(terms) > 1 or " WITH " in s) else ("id", s)
    return ("name", s)


def cdx_entry(kind: str, value: str) -> dict:
    if kind == "expression":
        return {"expression": value}
    return {"license": {kind: value}}


def has_licence(comp: dict) -> bool:
    for l in comp.get("licenses") or []:
        if l.get("expression"):
            return True
        li = l.get("license") or {}
        if li.get("id") or li.get("name"):
            return True
    return False


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    sbom_path, meta_path = sys.argv[1], sys.argv[2]

    with open(sbom_path, encoding="utf-8") as f:
        sbom = json.load(f)
    with open(meta_path, encoding="utf-8") as f:
        meta = json.load(f)

    # (name, version) -> licence string. `license_file` is deliberately not
    # treated as a licence: it names a file, not an identifier, and guessing
    # the identifier from a filename is how a GPL crate gets labelled MIT.
    declared: dict[tuple[str, str], str] = {}
    file_only: set[tuple[str, str]] = set()
    for p in meta.get("packages", []):
        key = (p.get("name"), p.get("version"))
        lic = (p.get("license") or "").strip()
        if lic:
            declared[key] = lic
        elif p.get("license_file"):
            file_only.add(key)

    ids = spdx_ids()
    comps = sbom.get("components", [])
    filled = kinds = 0
    counts = {"id": 0, "expression": 0, "name": 0}
    unresolved: list[str] = []

    for c in comps:
        if has_licence(c):
            continue
        key = (c.get("name"), c.get("version"))
        lic = declared.get(key)
        if not lic:
            why = "license-file only" if key in file_only else "not in cargo metadata"
            unresolved.append(f"{key[0]} {key[1]} ({why})")
            continue
        kind, value = classify(lic, ids)
        c["licenses"] = [cdx_entry(kind, value)]
        counts[kind] += 1
        filled += 1
    kinds = len(comps)

    with open(sbom_path, "w", encoding="utf-8") as f:
        json.dump(sbom, f, indent=2, ensure_ascii=False)
        f.write("\n")

    still = sum(1 for c in comps if not has_licence(c))
    print(f"  components         : {kinds}")
    print(f"  licences filled    : {filled} "
          f"(id={counts['id']} expression={counts['expression']} name={counts['name']})")
    print(f"  still unlicensed   : {still}")
    for u in unresolved[:20]:
        print(f"      - {u}")
    if len(unresolved) > 20:
        print(f"      ... and {len(unresolved) - 20} more")
    return 0


if __name__ == "__main__":
    sys.exit(main())
