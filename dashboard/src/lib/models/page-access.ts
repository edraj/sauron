import type { Permission } from './index';
import { PERMISSION_LABELS } from './permissions';
import { sessionStore, type CanScope } from '../stores/session.svelte';

/**
 * What a user needs in order to see a page at all.
 *
 * `level` is the scope the permission must be held at, and it must match the
 * `authorize_*` helper the page's endpoints actually call in
 * `backend/bins/sauron-api/src/routes/`. Getting it wrong in the permissive
 * direction shows a page that only ever 403s; getting it wrong in the strict
 * direction hides a page the user could have used. Each non-obvious row below
 * cites the backend line it was derived from.
 */
export interface PageAccess {
  perm: Permission;
  level: 'org' | 'project' | 'app';
  title: string;
}

/**
 * Route base path → requirement, or `null` for "no permission needed".
 *
 * Keyed by BASE path, not by the router's parameterised path: `/issues`, not
 * `/issues/:id`. `findPageAccessKey` strips trailing segments to find a key,
 * which mirrors how `Sidebar`'s `match: (p) => p.startsWith(…)` predicates
 * already work.
 *
 * Every route in `routes.ts` must appear here — `page-access.test.ts` enforces
 * it in both directions — so that adding a page forces a deliberate decision
 * about who can see it. `null` is that decision, written down.
 *
 * This table is the single source of truth for two consumers that used to
 * disagree: `Sidebar` (nav visibility) and `AppShell` (the in-page denied
 * state). They were previously two hand-maintained lists and had already
 * drifted.
 */
export const PAGE_ACCESS: Record<string, PageAccess | null> = {
  // --- Monitor -------------------------------------------------------------
  '/overview': { perm: 'event:read', level: 'app', title: 'Overview' },
  '/issues': { perm: 'issue:read', level: 'app', title: 'Exceptions' },
  '/performance': { perm: 'event:read', level: 'app', title: 'Performance' },

  // --- Explore -------------------------------------------------------------
  '/events': { perm: 'event:read', level: 'app', title: 'Events' },
  '/sessions': { perm: 'event:read', level: 'app', title: 'Sessions' },
  '/users': { perm: 'event:read', level: 'app', title: 'Users' },
  '/persons': { perm: 'event:read', level: 'app', title: 'Users' },
  '/devices': { perm: 'event:read', level: 'app', title: 'Devices' },
  '/screens': { perm: 'event:read', level: 'app', title: 'Screens' },
  '/workflows': { perm: 'event:read', level: 'app', title: 'Workflows' },

  // --- Analyze -------------------------------------------------------------
  // Project-level: active_users.rs:525 resolves reach across every app in the
  // project rather than authorizing one app.
  '/active-users': { perm: 'event:read', level: 'project', title: 'Active users' },
  '/funnels': { perm: 'event:read', level: 'app', title: 'Funnels' },
  '/journeys': { perm: 'event:read', level: 'app', title: 'Journeys' },

  // --- Uptime --------------------------------------------------------------
  // monitors.rs:67 authorizes at the project.
  '/monitors': { perm: 'monitor:read', level: 'project', title: 'Monitors' },

  // --- Alerting ------------------------------------------------------------
  // notifications.rs:66,313 use authorize_org.
  '/alerts': { perm: 'alert:read', level: 'org', title: 'Alerts' },

  // --- Manage --------------------------------------------------------------
  // Self-scoped (/v1/me/*). Always reachable — see the fallback note on
  // `PermissionDenied`, which relies on at least one ungated page existing.
  '/account': null,
  // projects.rs:49 is reach-based: an app-scoped member receives a filtered
  // list rather than a 403, so this is the widest level rather than 'project'.
  '/projects': { perm: 'project:read', level: 'app', title: 'Projects' },
  // NB: no '/apps' key. Sidebar's Projects item lists '/apps' as an alternate
  // `match` prefix, but no such route exists in routes.ts and lookups here go
  // through `item.href` ('#/projects'), never through `match`.
  // orgs.rs:160 uses authorize_org — a project- or app-scoped member:read
  // grant genuinely cannot list members.
  '/members': { perm: 'member:read', level: 'org', title: 'Members' },
  '/settings': { perm: 'app:read', level: 'app', title: 'App settings' },
  // Listing artifacts only needs issue:read (artifacts.rs:189), but this page
  // exists to upload, so a member who can only list has nothing to do here.
  '/source-maps': { perm: 'artifact:write', level: 'app', title: 'Source Maps' },
  // admin.rs:30 uses authorize_org.
  '/storage': { perm: 'org:manage', level: 'org', title: 'Storage' },
  '/inspector': { perm: 'pii:read', level: 'app', title: 'Privacy' },

  // --- Other ---------------------------------------------------------------
  // projects.rs:102 authorizes project creation at the org.
  '/onboarding': { perm: 'project:create', level: 'org', title: 'Onboarding' },
  // Integration guides — static content, no data. Always reachable.
  '/docs': null,
};

/**
 * The `PAGE_ACCESS` key a path resolves to, or `null` if it has no entry.
 *
 * Separate from [`resolvePageAccess`] because that function collapses "no
 * entry" and "an entry that is null" into the same `null` return — fine for
 * rendering, useless for the drift test, which exists precisely to tell an
 * undecided page from a deliberately ungated one.
 */
export function findPageAccessKey(path: string): string | null {
  const clean = path.split('?')[0].replace(/\/+$/, '') || '/';
  const segments = clean.split('/').filter(Boolean);
  for (let i = segments.length; i > 0; i--) {
    const key = '/' + segments.slice(0, i).join('/');
    if (key in PAGE_ACCESS) return key;
  }
  return null;
}

/**
 * The requirement for a concrete path, or `null` if the page needs none.
 *
 * Fails OPEN for an unknown path on purpose: the drift test guarantees every
 * real route has an entry, so an unknown path here is a typo in a link, and
 * rendering the page (which the server gates regardless) beats showing a
 * denial that has no permission to name.
 */
export function resolvePageAccess(path: string): PageAccess | null {
  const key = findPageAccessKey(path);
  return key === null ? null : PAGE_ACCESS[key];
}

/** Whether the current user satisfies a requirement. `null` is always allowed. */
export function canAccessPage(access: PageAccess | null): boolean {
  if (!access) return true;
  return sessionStore.can(access.perm, { level: access.level });
}

/**
 * `null` when the user may act, else the permission they are missing —
 * exactly the shape `Button`'s `lockedReason` prop wants, so a call site reads
 * `lockedReason={lockedBy('issue:write', { app })}` with no ternary and no way
 * to end up with a disabled control that cannot say why.
 */
export function lockedBy(perm: Permission, scope?: CanScope): Permission | null {
  return sessionStore.can(perm, scope) ? null : perm;
}

/** Tooltip text for a locked control that cannot take a `Button` prop. */
export function lockTitle(perm: Permission): string {
  const label = PERMISSION_LABELS[perm];
  return label ? `Requires: ${label} (${perm})` : `Requires: ${perm}`;
}
