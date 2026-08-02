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
      if (UNAUTHENTICATED.includes(routePath)) continue;
      const concrete = routePath.replace(/\/:[^/]+/g, '/x');
      expect(
        findPageAccessKey(concrete),
        `route "${routePath}" has no PAGE_ACCESS entry — add one (use null if it needs no permission)`,
      ).not.toBe(null);
    }
  });

  it('has no entry for a path that is not a real route', () => {
    const routeBases = new Set(
      ROUTE_PATHS.filter((p) => p.startsWith('/') && p.length > 1).map(
        (p) => '/' + p.split('/')[1],
      ),
    );
    for (const path of Object.keys(PAGE_ACCESS)) {
      expect(routeBases.has(path), `PAGE_ACCESS key "${path}" matches no route in routes.ts`).toBe(
        true,
      );
    }
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
    expect(resolvePageAccess('/members')?.level).toBe('org');
    expect(resolvePageAccess('/storage')?.level).toBe('org');
    expect(resolvePageAccess('/alerts')?.level).toBe('org');
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
