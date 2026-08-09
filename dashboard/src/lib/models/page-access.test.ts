import { describe, expect, it } from 'vitest';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import fs from 'node:fs';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import path from 'node:path';
import { ALL_PERMISSIONS } from './permissions';
import { PAGE_ACCESS, resolvePageAccess, findPageAccessKey, lockTitle } from './page-access';

// routes.ts is read as TEXT, not imported. Importing it pulls in every page
// component, and this project has no Svelte compilation in its test
// environment — the suite dies in the preprocessor before a single assertion
// runs. Parsing the source has the same virtue it has in permissions.test.ts
// (which reads rbac.rs the same way): the drift guard validates the real file
// rather than a hand-copied echo of it.
const ROUTES_TS_PATH = path.resolve(
  path.dirname(new URL(import.meta.url).pathname),
  '../../routes.ts',
);

/** Every quoted key in the `export const routes = { … }` object literal. */
function parseRoutePaths(): string[] {
  let source: string;
  try {
    source = fs.readFileSync(ROUTES_TS_PATH, 'utf-8');
  } catch (err) {
    throw new Error(
      `page-access.test.ts could not read the route table it validates against at ` +
        `"${ROUTES_TS_PATH}" (${err instanceof Error ? err.message : String(err)}). ` +
        `This test must fail rather than silently skip when that file is missing or moved.`,
    );
  }
  const start = source.indexOf('export const routes = {');
  if (start === -1) {
    throw new Error(`could not find "export const routes = {" in ${ROUTES_TS_PATH}`);
  }
  const body = source.slice(start);
  const paths = Array.from(body.matchAll(/^\s{2}'([^']+)':/gm), (m) => m[1]);
  if (paths.length === 0) {
    throw new Error(`parsed zero route keys out of ${ROUTES_TS_PATH} — the regex has gone stale`);
  }
  return paths;
}

/**
 * Every param-free prefix of each route path.
 *
 * Was `'/' + p.split('/')[1]` — first segment only, which was sufficient while
 * every route was one segment deep and made nested keys impossible. A `:param`
 * segment terminates the walk, so `/issues/:id` still contributes only
 * `/issues` and today's keys all stay legal; `/admin/members` now additionally
 * contributes itself, so each admin child can carry its own permission.
 */
function routePrefixes(paths: string[]): Set<string> {
  const out = new Set<string>();
  for (const p of paths) {
    if (!p.startsWith('/') || p.length <= 1) continue;
    const segments = p.split('/').filter(Boolean);
    const acc: string[] = [];
    for (const seg of segments) {
      if (seg.startsWith(':')) break;
      acc.push(seg);
      out.add('/' + acc.join('/'));
    }
  }
  return out;
}

/**
 * The full source line for a top-level route key ('  '/path': …,'), or `null`
 * if that key isn't one. `parseRoutePaths` above deliberately keeps only the
 * KEY half of each match — it never learns what a route maps to. That is
 * exactly what the `LEGACY_REDIRECTS` test below needs to check: not that the
 * path still exists (the "not a real route" test already covers that), but
 * that it is still bare `Redirect`, not a real page.
 */
function findRouteLine(path: string): string | null {
  const source = fs.readFileSync(ROUTES_TS_PATH, 'utf-8');
  const start = source.indexOf('export const routes = {');
  const body = source.slice(start);
  const escaped = path.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const match = body.match(new RegExp(`^\\s{2}'${escaped}':.*$`, 'm'));
  return match ? match[0] : null;
}

const ROUTE_PATHS = parseRoutePaths();

// Routes that never mount AppShell, so they have no page-level permission:
// the unauthenticated pages plus the two Redirect entries.
const UNAUTHENTICATED = [
  '/login',
  '/register',
  '/forgot-password',
  '/reset-password',
  '/change-password',
  '/unsubscribe',
  '/',
  '*',
];

// Legacy paths kept as bare Redirect entries after the /admin/* move. They
// mount no AppShell, so like UNAUTHENTICATED they have no page permission —
// but they ARE authenticated app paths, so they get their own list rather
// than making that array's name a lie.
const LEGACY_REDIRECTS = [
  '/members',
  '/projects',
  '/settings',
  '/source-maps',
  '/alerts',
  '/storage',
  '/inspector',
];

describe('PAGE_ACCESS', () => {
  it('only references permissions the backend recognises', () => {
    for (const [path, entry] of Object.entries(PAGE_ACCESS)) {
      if (!entry) continue;
      expect(ALL_PERMISSIONS, `${path} uses unknown permission ${entry.perm}`).toContain(
        entry.perm,
      );
    }
  });

  // Asserts on findPageAccessKey, NOT resolvePageAccess. The latter returns
  // `PageAccess | null` and is never `undefined`, so a coverage check written
  // against it would pass for every input and test nothing at all.
  it('covers every guarded route', () => {
    for (const routePath of ROUTE_PATHS) {
      if (UNAUTHENTICATED.includes(routePath) || LEGACY_REDIRECTS.includes(routePath)) continue;
      const concrete = routePath.replace(/\/:[^/]+/g, '/x');
      expect(
        findPageAccessKey(concrete),
        `route "${routePath}" has no PAGE_ACCESS entry — add one (use null if it needs no permission)`,
      ).not.toBe(null);
    }
  });

  it('has no entry for a path that is not a real route', () => {
    const routeBases = routePrefixes(ROUTE_PATHS);
    for (const path of Object.keys(PAGE_ACCESS)) {
      expect(routeBases.has(path), `PAGE_ACCESS key "${path}" matches no route in routes.ts`).toBe(
        true,
      );
    }
  });

  // LEGACY_REDIRECTS is an exemption from the coverage check above, keyed
  // only on the route's PATH — and `parseRoutePaths`/`ROUTE_PATHS` never look
  // at what a path maps to, so nothing before this test told a genuine bare
  // redirect apart from a path that was quietly turned back into a real page.
  // That is precisely the fail-open hole Task 6 caught mid-plan
  // (resolvePageAccess(unknown path) returns null, and canAccessPage(null) is
  // unconditionally true): if `/settings` were ever reinstated as a real
  // component, this array would keep exempting it from needing a
  // PAGE_ACCESS entry, and it would ship visible to every authenticated user
  // with nothing here to catch it. Asserting the route's VALUE — not just its
  // presence as a key — closes that: the exemption expires automatically the
  // day the route stops being `Redirect`.
  it('LEGACY_REDIRECTS entries are still bare Redirect routes', () => {
    for (const path of LEGACY_REDIRECTS) {
      const line = findRouteLine(path);
      expect(
        line,
        `LEGACY_REDIRECTS names "${path}", but no top-level route by that key exists in routes.ts`,
      ).not.toBe(null);
      expect(
        line,
        `LEGACY_REDIRECTS exempts "${path}" from the coverage check on the assumption it is a bare ` +
          `Redirect — it no longer is. Give it a real PAGE_ACCESS entry and drop it from this array.`,
      ).toMatch(/\bRedirect\b/);
    }
  });

  // Nested admin routes each need their OWN entry: '/admin/members' and
  // '/admin/storage' have different permissions at different levels, so a
  // single '/admin' key cannot express them. This asserts the prefix
  // derivation below accepts multi-segment keys.
  it('accepts a multi-segment key for a nested route', () => {
    const prefixes = routePrefixes(['/admin/members', '/issues/:id', '/issues']);
    expect(prefixes.has('/admin')).toBe(true);
    expect(prefixes.has('/admin/members')).toBe(true);
    expect(prefixes.has('/issues')).toBe(true);
    // A param segment terminates the walk — '/issues/:id' must NOT contribute
    // '/issues/:id' as a legal key.
    expect(prefixes.has('/issues/:id')).toBe(false);
  });
});

describe('resolvePageAccess', () => {
  it('matches a base path exactly', () => {
    expect(resolvePageAccess('/issues')?.perm).toBe('issue:read');
  });

  it('strips trailing segments until a key matches', () => {
    expect(resolvePageAccess('/issues/abc-123')?.perm).toBe('issue:read');
    expect(resolvePageAccess('/persons/user-9')?.perm).toBe('event:read');
    expect(resolvePageAccess('/monitors/m-1')?.perm).toBe('monitor:read');
    expect(resolvePageAccess('/screens/Checkout')?.perm).toBe('event:read');
  });

  it('ignores a query string and a trailing slash', () => {
    expect(resolvePageAccess('/issues/')?.perm).toBe('issue:read');
    expect(resolvePageAccess('/issues?status=unresolved')?.perm).toBe('issue:read');
  });

  it('returns null for a deliberately ungated page', () => {
    expect(resolvePageAccess('/account')).toBe(null);
    expect(resolvePageAccess('/docs')).toBe(null);
  });

  it('returns null for an unknown path rather than failing closed', () => {
    expect(resolvePageAccess('/nope')).toBe(null);
    expect(findPageAccessKey('/nope')).toBe(null);
  });

  it('gates org-only pages at org level', () => {
    // These three call authorize_org server-side (orgs.rs:160, admin.rs:30,
    // notifications.rs:66), which no project- or app-scoped grant can satisfy.
    expect(resolvePageAccess('/admin/members')?.level).toBe('org');
    expect(resolvePageAccess('/admin/storage')?.level).toBe('org');
    expect(resolvePageAccess('/admin/alerts')?.level).toBe('org');
  });

  it('gates project-scoped pages at project level', () => {
    expect(resolvePageAccess('/monitors')?.level).toBe('project');
    expect(resolvePageAccess('/active-users')?.level).toBe('project');
  });
});

describe('lockTitle', () => {
  it('names both the human label and the raw permission', () => {
    const title = lockTitle('issue:write');
    expect(title).toContain('Resolve, assign, and comment on issues');
    expect(title).toContain('issue:write');
  });

  it('falls back to the bare permission when no label exists', () => {
    expect(lockTitle('future:permission' as never)).toBe('Requires: future:permission');
  });
});
