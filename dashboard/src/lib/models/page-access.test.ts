import { beforeEach, describe, expect, it } from 'vitest';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import fs from 'node:fs';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import path from 'node:path';
import { ALL_PERMISSIONS } from './permissions';
import { PAGE_ACCESS, resolvePageAccess, findPageAccessKey, lockTitle, canAccessPage } from './page-access';
import { sessionStore } from '../stores/session.svelte';
import type { Permission } from './index';

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

// ---------------------------------------------------------------------------
// Environment-scoped members.
//
// `sauron-auth` has two authorization paths, and which one a route takes
// decides whether an environment-scoped grant can ever satisfy it:
//
//   env-aware  `authorized_read_scope` → `authorize_env_read`
//   env-blind  `authorize_app` / `authorize_project` / `authorize_org`
//
// `authorize_env_read_with_perms`'s own doc comment records why this matters:
// the env-blind helper "returned 403 to an env-scoped caller **even for their
// own environment** — they could list issues but not open one". That was fixed
// server-side. The gate below is the client half of the same fix.
//
// The backend files are read as TEXT, exactly as permissions.test.ts reads
// rbac.rs: a hand-copied echo of which routes are env-aware would drift the
// first time a handler switched paths, and drift in the PERMISSIVE direction
// is a page that renders and then 403s.
// ---------------------------------------------------------------------------
const ROUTES_RS_DIR = path.resolve(
  path.dirname(new URL(import.meta.url).pathname),
  '../../../../backend/bins/sauron-api/src/routes',
);

/**
 * The backend handler that serves each gated page's INITIAL load, as
 * `file.rs::function`.
 *
 * Per HANDLER, not per file. The first version of this test asked only whether
 * the route file mentioned `authorized_read_scope` anywhere, which is an
 * over-approximation: `funnels.rs` contains both the environment-aware
 * `compute` and the environment-blind `list_saved`, so a file-level check
 * happily marked `/funnels` environment-aware while the page's very first
 * request authorizes at the app and 403s for the member it just admitted.
 */
const PAGE_LOAD_HANDLER: Record<string, string> = {
  '/overview': 'analytics.rs::overview',
  '/issues': 'issues.rs::list',
  '/performance': 'performance.rs::summary',
  '/events': 'analytics.rs::events_list',
  '/transactions': 'transactions.rs::list',
  '/sessions': 'sessions.rs::list',
  '/users': 'analytics.rs::users_summary',
  '/persons': 'analytics.rs::persons_list',
  '/devices': 'devices.rs::list',
  '/screens': 'screens.rs::list',
  '/workflows': 'workflows.rs::list',
  '/journeys': 'journeys.rs::explore',
  // Environment-BLIND loads. Each is here to be asserted NEGATIVE — the set of
  // pages that must never carry `envAware` is as load-bearing as the set that
  // must.
  '/funnels': 'funnels.rs::list_saved',
  '/monitors': 'monitors.rs::list',
  '/admin/members': 'orgs.rs::list_members',
  '/admin/projects': 'projects.rs::list_projects',
  '/admin/source-maps': 'artifacts.rs::list',
  '/admin/alerts': 'notifications.rs::list_rules',
  '/admin/wall-of-shame': 'audit.rs::list',
};

/** The body of `pub async fn <name>(` up to the next top-level `pub async fn`. */
function handlerBody(file: string, fn: string): string {
  const full = path.join(ROUTES_RS_DIR, file);
  let source: string;
  try {
    source = fs.readFileSync(full, 'utf-8');
  } catch (err) {
    throw new Error(
      `page-access.test.ts could not read the backend route file it validates against at ` +
        `"${full}" (${err instanceof Error ? err.message : String(err)}). This test must fail ` +
        `rather than silently skip when the backend moves.`,
    );
  }
  const start = source.indexOf(`pub async fn ${fn}(`);
  if (start === -1) {
    throw new Error(
      `page-access.test.ts expects a handler "pub async fn ${fn}(" in ${file}, and there is ` +
        `none. Either it was renamed — update PAGE_LOAD_HANDLER — or the page now loads ` +
        `through something else, which changes whether it may be envAware.`,
    );
  }
  const next = source.indexOf('\npub async fn ', start + 1);
  return source.slice(start, next === -1 ? source.length : next);
}

/** Whether that handler authorizes through the environment-aware read path. */
function handlerIsEnvAware(spec: string): boolean {
  const [file, fn] = spec.split('::');
  return handlerBody(file, fn).includes('authorized_read_scope');
}

describe('PAGE_ACCESS.envAware', () => {
  it('is set on exactly the pages whose LOAD handler takes the env-aware path', () => {
    for (const [pagePath, spec] of Object.entries(PAGE_LOAD_HANDLER)) {
      const entry = PAGE_ACCESS[pagePath];
      expect(entry, `${pagePath} has no PAGE_ACCESS entry`).toBeTruthy();
      const aware = handlerIsEnvAware(spec);
      expect(
        entry!.envAware === true,
        `${pagePath} is marked envAware=${entry!.envAware === true} but ${spec} ` +
          `${aware ? 'DOES' : 'does NOT'} use authorized_read_scope`,
      ).toBe(aware);
    }
  });

  // Named separately from the derived check above because these are the two
  // rows most likely to be marked by eye. Both sit with the data pages in the
  // nav and read like them:
  //   /monitors  — monitors.rs authorizes at the project throughout.
  //   /funnels   — funnels.rs::compute IS env-aware, but the page loads through
  //                list_saved, and /v1/apps/{id}/funnels is in api/scope.ts's
  //                BACKEND_REJECTS_ENVIRONMENT_ID because saved definitions are
  //                app-wide config.
  it('is not set on /monitors or /funnels', () => {
    expect(PAGE_ACCESS['/monitors']?.envAware).toBeUndefined();
    expect(PAGE_ACCESS['/funnels']?.envAware).toBeUndefined();
  });

  it('is not set on any admin child', () => {
    for (const [pagePath, entry] of Object.entries(PAGE_ACCESS)) {
      if (!pagePath.startsWith('/admin/') || !entry) continue;
      expect(entry.envAware, `${pagePath} must not be envAware`).toBeUndefined();
    }
  });
});

describe('canAccessPage — environment-scoped grants', () => {
  const devPerms: Permission[] = ['issue:read', 'event:read', 'monitor:read', 'member:read'];

  beforeEach(() => {
    sessionStore.currentOrgId = 'org-1';
    sessionStore.currentProjectId = 'proj-1';
    sessionStore.currentAppId = 'app-1';
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'env', scope_id: 'env-1', permissions: devPerms }],
    };
  });

  it('admits an env-aware page while the picker is on "all environments"', () => {
    // `resolve_env_filter` answers All with Ok(Subset(readable)) for an
    // env-scoped caller — it narrows the read, it does not deny it.
    sessionStore.currentEnvId = null;
    expect(canAccessPage(PAGE_ACCESS['/overview'])).toBe(true);
    expect(canAccessPage(PAGE_ACCESS['/issues'])).toBe(true);
    // Same permission, same picker state — refused, because the page's first
    // request rejects environment_id outright.
    expect(canAccessPage(PAGE_ACCESS['/funnels'])).toBe(false);
  });

  it('admits an env-aware page for the environment actually granted', () => {
    sessionStore.currentEnvId = 'env-1';
    expect(canAccessPage(PAGE_ACCESS['/overview'])).toBe(true);
  });

  it('refuses an env-aware page for an environment not granted', () => {
    sessionStore.currentEnvId = 'env-2';
    expect(canAccessPage(PAGE_ACCESS['/overview'])).toBe(false);
  });

  // `resolve_env_filter` returns Err(UnattributedNeedsAppReach) for this arm:
  // reading unattributed rows is an app-level question, so an env grant alone
  // cannot answer it.
  it('refuses an env-aware page when the picker is on "unattributed"', () => {
    sessionStore.currentEnvId = 'none';
    expect(canAccessPage(PAGE_ACCESS['/overview'])).toBe(false);
  });

  // The direction that actually matters. An env grant satisfying an
  // app/project/org-level check is the security-relevant failure — a UI that
  // shows a control the server then 403s.
  it('never admits an env-blind page, in any picker state', () => {
    for (const env of [null, 'env-1', 'env-2', 'none']) {
      sessionStore.currentEnvId = env;
      expect(canAccessPage(PAGE_ACCESS['/monitors']), `env=${env}`).toBe(false);
      expect(canAccessPage(PAGE_ACCESS['/admin/members']), `env=${env}`).toBe(false);
      expect(canAccessPage(PAGE_ACCESS['/admin/projects']), `env=${env}`).toBe(false);
    }
  });

  it('still admits an app-level grant with no environment selected', () => {
    sessionStore.access = {
      permissions: [],
      grants: [{ scope_type: 'app', scope_id: 'app-1', permissions: devPerms }],
    };
    sessionStore.currentEnvId = null;
    expect(canAccessPage(PAGE_ACCESS['/overview'])).toBe(true);
    expect(canAccessPage(PAGE_ACCESS['/monitors'])).toBe(false); // project-level
  });
});

describe('source maps page gate', () => {
  // artifacts.rs:396 lists on issue:read; only upload (:181) and delete (:429)
  // need artifact:write, and SourceMaps.svelte already locks both. Gating the
  // whole page on the write permission replaced a readable page with a wall.
  it('requires only the permission its list endpoint requires', () => {
    expect(PAGE_ACCESS['/admin/source-maps']?.perm).toBe('issue:read');
  });
});
