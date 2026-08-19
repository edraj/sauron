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
  /**
   * Set when this page's list endpoint takes `sauron-auth`'s ENV-AWARE read
   * path (`authorized_read_scope` → `authorize_env_read`) rather than the
   * env-blind `authorize_app`/`_project`/`_org`.
   *
   * Only on such a page can an environment-scoped grant satisfy the gate — see
   * `canAccessPage`. Setting it on an env-blind page produces a page that
   * renders and then 403s, so `page-access.test.ts` derives the correct set by
   * reading the backend route files rather than trusting this flag.
   */
  envAware?: true;
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
  '/overview': { perm: 'event:read', level: 'app', title: 'Overview', envAware: true },
  '/issues': { perm: 'issue:read', level: 'app', title: 'Exceptions', envAware: true },
  '/performance': { perm: 'event:read', level: 'app', title: 'Performance', envAware: true },

  // --- Explore -------------------------------------------------------------
  '/events': { perm: 'event:read', level: 'app', title: 'Events', envAware: true },
  // `routes/transactions.rs:list` authorizes on `event:read` at the app scope,
  // and the same permission gates the `tags`/`extra` body via
  // `symbolicate::gate_transaction_body`.
  '/transactions': { perm: 'event:read', level: 'app', title: 'Transactions', envAware: true },
  '/sessions': { perm: 'event:read', level: 'app', title: 'Sessions', envAware: true },
  '/users': { perm: 'event:read', level: 'app', title: 'Users', envAware: true },
  '/persons': { perm: 'event:read', level: 'app', title: 'Users', envAware: true },
  '/devices': { perm: 'event:read', level: 'app', title: 'Devices', envAware: true },
  '/screens': { perm: 'event:read', level: 'app', title: 'Screens', envAware: true },
  '/workflows': { perm: 'event:read', level: 'app', title: 'Workflows', envAware: true },

  // --- Analyze -------------------------------------------------------------
  // Project-level: active_users.rs:525 resolves reach across every app in the
  // project rather than authorizing one app.
  '/active-users': { perm: 'event:read', level: 'project', title: 'Active users', envAware: true },
  // NOT envAware, unlike its Explore/Analyze neighbours. `funnels::compute`
  // (the live counts) is environment-aware, but the page LOADS through
  // `funnels::list_saved`, and `/v1/apps/{id}/funnels` sits in
  // `api/scope.ts`'s `BACKEND_REJECTS_ENVIRONMENT_ID`: saved funnel
  // definitions are app-wide config, so that endpoint rejects
  // `environment_id` outright and authorizes at the app. Marking this page
  // envAware would admit an environment-scoped member to a page whose first
  // request 403s.
  '/funnels': { perm: 'event:read', level: 'app', title: 'Funnels' },
  '/journeys': { perm: 'event:read', level: 'app', title: 'Journeys', envAware: true },

  // --- Uptime --------------------------------------------------------------
  // monitors.rs:67 authorizes at the project.
  '/monitors': { perm: 'monitor:read', level: 'project', title: 'Monitors' },

  // --- Admin ---------------------------------------------------------------
  // '/admin' itself is ungated: no single permission expresses "can reach at
  // least one admin child", and inventing one would drift from the nine child
  // gates below. AdminIndex redirects to the first reachable child, or renders
  // PermissionDenied when there is none.
  '/admin': null,
  // orgs.rs:160 uses authorize_org — a project- or app-scoped member:read
  // grant genuinely cannot list members.
  '/admin/members': { perm: 'member:read', level: 'org', title: 'Members' },
  // orgs.rs:1380 gates list_roles on member:read, not role:manage — reading
  // the catalogue is not the same as editing it.
  '/admin/roles': { perm: 'member:read', level: 'org', title: 'Roles' },
  // projects.rs:49 is reach-based: an app-scoped member receives a filtered
  // list rather than a 403, so this is the widest level rather than 'project'.
  '/admin/projects': { perm: 'project:read', level: 'app', title: 'Projects' },
  // Widest level rather than 'project' — same reasoning as '/admin/projects'
  // above. list_project_environments (environments.rs:196) authorizes the
  // CATALOGUE at the project via authorize_project (not reach-based), but
  // list_app_environments (:400-425) and the per-app mutation endpoints
  // (update_app_environment :444, rotate_app_environment_key :525) are
  // reach-based and authorize at the APP. Gating the page at 'project' would
  // fail an app-scoped env:read grant outright — before this plan that member
  // reached the same per-app controls via /settings. 'app' is the widest
  // `can()` level (keeps org, project AND app scope ids), so it admits that
  // member; Environments.svelte then degrades the catalogue-only parts of the
  // page when the project-wide read 403s.
  '/admin/environments': { perm: 'env:read', level: 'app', title: 'Environments' },
  '/admin/settings': { perm: 'app:read', level: 'app', title: 'App settings' },
  // artifacts.rs:396 lists on issue:read; only upload (:181) and delete (:429)
  // need artifact:write. Gating the PAGE on the write permission replaced a
  // readable page with a wall for everyone who could read it — SourceMaps.svelte
  // already computes a `writeLock` and applies it to all three write controls,
  // so the page was built to degrade and only this row disagreed.
  '/admin/source-maps': { perm: 'issue:read', level: 'app', title: 'Source Maps' },
  // notifications.rs:66,313 use authorize_org.
  '/admin/alerts': { perm: 'alert:read', level: 'org', title: 'Alerts' },
  // admin.rs:30 uses authorize_org.
  '/admin/storage': { perm: 'org:manage', level: 'org', title: 'Storage' },
  '/admin/privacy': { perm: 'pii:read', level: 'app', title: 'Privacy' },
  // audit.rs's `list` calls authorize_org with ORG_MANAGE. Org-level, and
  // deliberately the same gate as '/admin/storage': the people who may read
  // who did what are the people who administer the org. No new permission
  // was introduced for this page.
  '/admin/wall-of-shame': { perm: 'org:manage', level: 'org', title: 'Wall of Shame' },
  // Same gate as Storage and the tier routes. The backend requires org:manage
  // in EVERY org (require_deployment_admin), which this table cannot express —
  // org-level org:manage is the closest it has, so the nav is slightly more
  // permissive than the API. That direction is the safe one: the page loads
  // and reports a clean 403 rather than hiding a capability from someone who
  // holds it. See failures.rs for why the endpoint is deployment-wide.
  '/admin/ingest-failures': { perm: 'org:manage', level: 'org', title: 'Ingest failures' },
  // Same gate as Storage / Wall of Shame / Ingest failures, and the same caveat:
  // the backend requires org:manage in EVERY org (require_deployment_admin),
  // which this table cannot express. Org-level org:manage is the closest it
  // has, so the nav is slightly MORE permissive than the API. That direction is
  // the safe one — the page loads and reports a clean 403 rather than hiding a
  // capability from someone who holds it.
  '/admin/purge': { perm: 'org:manage', level: 'org', title: 'Purge data' },

  // --- Self-service --------------------------------------------------------
  // Self-scoped (/v1/me/*). Always reachable — see the fallback note on
  // `PermissionDenied`, which relies on at least one ungated page existing.
  '/account': null,

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

/**
 * Whether the current user satisfies a requirement. `null` is always allowed.
 *
 * The org/project/app answer comes first and is unchanged. An `envAware` page
 * then gets a second chance from an environment-scoped grant, mirroring
 * `resolve_env_filter` (rbac.rs) arm for arm:
 *
 * | picker            | rule                          | server arm                        |
 * |-------------------|-------------------------------|-----------------------------------|
 * | a real env id     | must hold it on THAT env      | `EnvFilter::One`                  |
 * | `null` ("all")    | holding it on any env is enough | `All` → `Ok(Subset(readable))`  |
 * | `'none'`          | app-level only                | `Err(UnattributedNeedsAppReach)`  |
 *
 * The "all" arm is the non-obvious one: the server NARROWS the read to the
 * environments the caller can see rather than refusing it, so refusing here
 * would hide a page the server would have served.
 *
 * Nothing here lets an environment grant satisfy an env-BLIND page. That is the
 * security-relevant direction — a control the UI shows and the server then
 * 403s — and it is why the second chance is opt-in per row rather than global.
 *
 * KNOWN IMPRECISION: `canAtAnyEnv` does not check that the granted environment
 * belongs to the current app, so a member granted on another app's environment
 * sees the page and gets a clean 403. Consulting `sessionStore.environments`
 * instead would make this gate flip as that list loads — the flashing-gate
 * failure `AdminIndex` documents — and this is the same trade-off the
 * `/admin/ingest-failures` row above already makes deliberately.
 */
export function canAccessPage(access: PageAccess | null): boolean {
  if (!access) return true;
  if (sessionStore.can(access.perm, { level: access.level })) return true;
  if (!access.envAware) return false;
  const env = sessionStore.currentEnvId;
  if (env === null) return sessionStore.canAtAnyEnv(access.perm);
  if (env === 'none') return false;
  return sessionStore.can(access.perm, { env });
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

/**
 * The permission a nav item is missing, or `null` if the page is reachable —
 * the same contract as [`lockedBy`], so a locked nav entry and a locked button
 * cannot describe the same missing grant two different ways.
 *
 * `null` for an ungated page and for an unknown path, matching
 * `resolvePageAccess`'s deliberate fail-open: a path with no entry is a typo in
 * a link, and there is no permission to name.
 */
export function pageLockedBy(path: string): Permission | null {
  const access = resolvePageAccess(path);
  if (!access) return null;
  return canAccessPage(access) ? null : access.perm;
}

/** Tooltip text for a locked control that cannot take a `Button` prop. */
export function lockTitle(perm: Permission): string {
  const label = PERMISSION_LABELS[perm];
  return label ? `Requires: ${label} (${perm})` : `Requires: ${perm}`;
}
