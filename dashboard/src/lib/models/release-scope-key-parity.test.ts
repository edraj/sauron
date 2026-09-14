import { describe, expect, it } from 'vitest';
import { findPageAccessKey } from './page-access';
import { RELEASE_AWARE } from './shell';
// `?raw` rather than `node:fs` — same reason `refresh-parity.test.ts` and
// `wiki-sdk-versions.test.ts` do it: Vite resolves the specifier at build time,
// so a renamed or moved file is a hard resolution error here rather than a
// runtime read that a `try` could quietly swallow.
import routesSource from '../../routes.ts?raw';

/**
 * `scopeKey` vs `scopeKeyWithRelease`.
 *
 * `sessionStore.scopeKey` is `app:env` — the two dimensions EVERY telemetry
 * page's data is narrowed by. `scopeKeyWithRelease` appends the selected
 * release, and only the pages whose list requests actually carry `?release=`
 * (`api/scope.ts`'s `RELEASE_SCOPED_URL`) may key on it.
 *
 * The split exists because the merged key re-fetched all 24 telemetry pages on
 * every release switch, including the ~20 that read rollups with no release
 * dimension at all and would come back byte-identical. The failure mode this
 * test guards is the reverse of the usual one: not a page that forgets to
 * refetch, but a page that refetches for nothing — invisible in every
 * screenshot and every unit test, and only ever noticed as a slow dashboard.
 *
 * Derived, never hand-listed: the page files come out of `routes.ts`, the
 * release-awareness decision out of `RELEASE_AWARE`. A page added tomorrow
 * inherits whichever answer its `PAGE_ACCESS` key already carries (and
 * `shell.test.ts` is what forces that key to exist), so neither half of this
 * can be satisfied by editing a list in this file.
 */
const KEY = 'scopeKeyWithRelease';

const pages = import.meta.glob('../../pages/*.svelte', {
  query: '?raw',
  import: 'default',
  eager: true,
});

const components = import.meta.glob('../../lib/components/**/*.svelte', {
  query: '?raw',
  import: 'default',
  eager: true,
});

function fileName(path: string): string {
  return path.split('/').pop()!;
}

/**
 * The page source with every comment removed, so a substring search sees only
 * CODE.
 *
 * Without this the checks below are satisfiable by prose: several of these
 * pages carry a comment explaining *why* a particular fetch stays on the plain
 * `scopeKey` — and naming `scopeKeyWithRelease` to do it. That mention alone
 * made the "keys its list fetch on scopeKeyWithRelease" assertion pass on a
 * page whose list had been reverted to `scopeKey`, which is precisely the
 * regression this file exists to catch. Comments are also where a stale claim
 * survives longest, so they are the last thing a parity test should trust.
 *
 * Three flavours, in order: Svelte/HTML `<!-- -->`, block `/* *\/` (JSDoc
 * included), and `//` to end of line. The line-comment pattern deliberately
 * refuses a `//` preceded by `:` so `https://…` inside a string survives.
 *
 * Over-stripping cannot pass silently: the positive assertions require the
 * release-aware pages to still CONTAIN the key, so an expression eaten by a
 * bad strip fails this file loudly rather than weakening it.
 */
function stripComments(src: string): string {
  return src
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/[^\n]*/g, '$1');
}

function sourceOf(files: Record<string, unknown>, name: string): string {
  for (const [path, src] of Object.entries(files)) {
    if (fileName(path) === name) return stripComments(src as string);
  }
  throw new Error(
    `release-scope-key-parity.test.ts could not find "${name}" in the glob it scans. ` +
      `This test must fail rather than silently skip a component that was renamed or moved.`,
  );
}

/**
 * Every `'<route path>': … import('./pages/<Page>.svelte')` pair in
 * `routes.ts`, as `[routePath, PageFile.svelte]`.
 *
 * Entry-by-entry rather than one line-anchored regex: several routes spell
 * their loader on a continuation line (`/change-password`'s `wrap({ … })`),
 * and a single-line pattern would drop exactly those without saying so.
 */
function parseRoutePages(): [string, string][] {
  const start = routesSource.indexOf('export const routes = {');
  if (start === -1) {
    throw new Error('could not find "export const routes = {" in routes.ts — the parser is stale');
  }
  const body = routesSource.slice(start);
  const keys = Array.from(body.matchAll(/^ {2}'([^']+)':/gm));
  const out: [string, string][] = [];
  for (let i = 0; i < keys.length; i++) {
    const from = keys[i].index!;
    const to = i + 1 < keys.length ? keys[i + 1].index! : body.length;
    const entry = body.slice(from, to);
    const page = entry.match(/import\('\.\/pages\/([A-Za-z0-9]+)\.svelte'\)/);
    // Bare `Redirect` entries and '/'/'*' load no page of their own.
    if (page) out.push([keys[i][1], `${page[1]}.svelte`]);
  }
  return out;
}

const ROUTE_PAGES = parseRoutePages();

/** `PAGE_ACCESS` key -> every page file whose route resolves to that key. */
function pagesByAccessKey(): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const [routePath, page] of ROUTE_PAGES) {
    // `/issues/:id` and `/issues` share the '/issues' key, exactly as
    // `findPageAccessKey` resolves them at runtime.
    const key = findPageAccessKey(routePath.replace(/\/:[^/]+/g, '/x'));
    if (key === null) continue; // pre-auth pages carry no PAGE_ACCESS row
    out.set(key, [...(out.get(key) ?? []), page]);
  }
  return out;
}

const PAGES_BY_KEY = pagesByAccessKey();

/**
 * Pages under a release-aware key that issue no release-scoped request of
 * their own, and so must stay on the two-segment `scopeKey`.
 *
 * `RELEASE_AWARE` is keyed per ROUTE FAMILY, and a family can hold both kinds
 * of page. `/sessions` is release-aware because `GET …/sessions` (the list)
 * carries `?release=`; `SessionDetail` reads `GET …/sessions/{id}`, which is
 * not in `RELEASE_SCOPED_URL` — one session is one session whatever the
 * switcher says. Keying it on the release would blank and refetch an open
 * session detail to redisplay the identical payload.
 *
 * `IssueDetail` is deliberately NOT here: its occurrences table loads through
 * `…/issues/{id}/events`, which IS release-scoped.
 */
const NO_RELEASE_SCOPED_READS = new Set(['SessionDetail.svelte']);

/** The one non-page component allowed to key on the release. */
const RELEASE_SCOPED_COMPONENT = 'OperationTransactionsModal.svelte';

describe('release scope key parity', () => {
  it('parses a realistic number of page routes out of routes.ts', () => {
    // Guards the parser against matching nothing (or almost nothing) and
    // turning every assertion below into a vacuous pass over an empty set.
    expect(ROUTE_PAGES.length).toBeGreaterThan(30);
  });

  it('maps every RELEASE_AWARE key to at least one page file', () => {
    const unmapped = Object.keys(RELEASE_AWARE).filter((k) => !PAGES_BY_KEY.has(k));
    expect(
      unmapped.sort(),
      `these RELEASE_AWARE keys match no route in routes.ts, so nothing below checks them: ` +
        unmapped.join(', '),
    ).toEqual([]);
  });

  it('every release-aware page keys its list fetch on scopeKeyWithRelease', () => {
    const missing: string[] = [];
    for (const [key, aware] of Object.entries(RELEASE_AWARE)) {
      if (!aware) continue;
      for (const page of PAGES_BY_KEY.get(key) ?? []) {
        if (NO_RELEASE_SCOPED_READS.has(page)) continue;
        if (!sourceOf(pages, page).includes(KEY)) missing.push(`${page} (${key})`);
      }
    }
    expect(
      missing.sort(),
      `these pages filter by release but never observe it, so a release switch leaves the ` +
        `previous release's rows on screen: ${missing.join(', ')}`,
    ).toEqual([]);
  });

  it('no page outside a release-aware key observes the release', () => {
    const rogue: string[] = [];
    for (const [key, aware] of Object.entries(RELEASE_AWARE)) {
      if (aware) continue;
      for (const page of PAGES_BY_KEY.get(key) ?? []) {
        if (sourceOf(pages, page).includes(KEY)) rogue.push(`${page} (${key})`);
      }
    }
    expect(
      rogue.sort(),
      `these pages read rollups with no release dimension, so keying on the release only ` +
        `refetches identical data on every switch: ${rogue.join(', ')}`,
    ).toEqual([]);
  });

  it('exempted pages under a release-aware key stay on the plain scopeKey', () => {
    // The exemption is an assertion in its own right, not a hole: a page
    // listed above that DID start observing the release would otherwise slip
    // past both checks.
    for (const page of NO_RELEASE_SCOPED_READS) {
      expect(sourceOf(pages, page), `${page} is exempt but now observes the release`).not.toContain(
        KEY,
      );
    }
  });

  it('OperationTransactionsModal is the only component that observes the release', () => {
    // Its transactions list is scoped by the axios interceptor (it loads
    // `…/transactions`), so a mid-view release switch must refetch — and the
    // modal opens over `/performance`, whose own key is `false`.
    expect(sourceOf(components, RELEASE_SCOPED_COMPONENT)).toContain(KEY);
    const rogue = Object.entries(components)
      .filter(
        ([path, src]) =>
          fileName(path) !== RELEASE_SCOPED_COMPONENT && stripComments(src as string).includes(KEY),
      )
      .map(([path]) => fileName(path));
    expect(
      rogue.sort(),
      `a shared component that keys on the release drags it onto every page that mounts it: ` +
        rogue.join(', '),
    ).toEqual([]);
  });
});
