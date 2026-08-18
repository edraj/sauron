import { describe, it, expect } from 'vitest';
// `?raw` rather than `node:fs` — see the note in
// `components/filters/filter-registry-parity.test.ts`.
import jsPkg from '../../../../sdks/js/package.json?raw';
import nodePkg from '../../../../sdks/node/package.json?raw';
import pyToml from '../../../../sdks/python/pyproject.toml?raw';
import flutterPubspec from '../../../../sdks/flutter/pubspec.yaml?raw';
import csharpProj from '../../../../sdks/csharp/Sauron/Sauron.csproj?raw';
import browserMd from '../../../../wiki/Browser-SDK.md?raw';
import flutterMd from '../../../../wiki/Flutter-SDK.md?raw';
import nodeMd from '../../../../wiki/Node-SDK.md?raw';
import pythonMd from '../../../../wiki/Python-SDK.md?raw';
import csharpMd from '../../../../wiki/CSharp-SDK.md?raw';
import capabilitiesMd from '../../../../wiki/Capabilities.md?raw';
import wireMd from '../../../../wiki/Ingest-Wire-Contract.md?raw';
import homeMd from '../../../../wiki/Home.md?raw';

/**
 * The SDK versions the wiki states, against the manifests they describe.
 *
 * Exists because this drifted silently for five releases: every SDK page, the
 * capability matrix, the wire contract and the home page all said **v0.3.0**
 * while the manifests had moved to 1.5–1.8, and nothing anywhere failed. Prose
 * has no compiler, so the only way a version claim stays true is if something
 * reads it.
 *
 * Deliberately narrow. It checks the ONE number on each page that claims to be
 * that SDK's current version, and the table in `Capabilities.md` that collects
 * them. Historical statements ("since v0.3.0 every SDK gzips the body") are
 * about when a behaviour landed, are still true, and must NOT be swept up by a
 * blanket version match — which is why each case pins an exact sentence rather
 * than grepping for a version-shaped string.
 */
/** Manifest → the version it declares. */
const MANIFEST_VERSION: Record<string, () => string> = {
  browser: () => JSON.parse(jsPkg).version,
  node: () => JSON.parse(nodePkg).version,
  python: () => /^version\s*=\s*"([^"]+)"/m.exec(pyToml)![1],
  flutter: () => /^version:\s*(\S+)/m.exec(flutterPubspec)![1],
  csharp: () => /<Version>([^<]+)<\/Version>/.exec(csharpProj)![1],
};

/** The page, and the sentence on it that states a current version. */
const PAGE_CLAIM: { sdk: string; page: string; src: string; sentence: (v: string) => string }[] = [
  { sdk: 'browser', page: 'wiki/Browser-SDK.md', src: browserMd, sentence: (v) => `SDK (**v${v}**).` },
  { sdk: 'flutter', page: 'wiki/Flutter-SDK.md', src: flutterMd, sentence: (v) => `from one SDK (**v${v}**).` },
  { sdk: 'node', page: 'wiki/Node-SDK.md', src: nodeMd, sentence: (v) => `Server-side Node/TypeScript SDK (**v${v}**).` },
  { sdk: 'python', page: 'wiki/Python-SDK.md', src: pythonMd, sentence: (v) => `Server-side Python SDK (**v${v}**).` },
  { sdk: 'csharp', page: 'wiki/CSharp-SDK.md', src: csharpMd, sentence: (v) => `Server-side .NET SDK (**v${v}**,` },
];

describe('wiki SDK versions ↔ manifests', () => {
  for (const { sdk, page, src, sentence } of PAGE_CLAIM) {
    it(`${page} states ${sdk}'s real version`, () => {
      const version = MANIFEST_VERSION[sdk]();
      expect(src, `bump the version sentence on ${page} to ${version}`).toContain(
        sentence(version),
      );
    });
  }

  it('the Capabilities version table matches every manifest', () => {
    const table = capabilitiesMd;
    for (const [sdk, version] of Object.entries(MANIFEST_VERSION)) {
      const v = version();
      // The row's trailing cell, e.g. `| 1.6.0 |`. Matching the whole row would
      // pin the package/registry columns too, which change for different
      // reasons and have their own source of truth.
      expect(table, `Capabilities.md is missing a | ${v} | cell for ${sdk}`).toContain(`| ${v} |`);
    }
  });

  it('no page still claims the five share one version', () => {
    // The specific false statement this whole test was written for. It read
    // true for exactly one release and then quietly stopped.
    for (const src of [capabilitiesMd, wireMd, homeMd]) {
      expect(src).not.toMatch(/all five (SDKs )?ship as/i);
    }
  });
});
