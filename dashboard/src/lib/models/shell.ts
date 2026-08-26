/**
 * Which shell a route renders in, and with which scope requirements.
 *
 * Until 2026-08, every page mounted its own `<AppShell>`, which meant the
 * sidebar and topbar were torn down and rebuilt on every navigation — and
 * while a lazy route chunk downloaded, `LazyRoute`'s loading state rendered
 * with no shell at all, so a section click read as a whole-page load.
 * `App.svelte` now mounts ONE `AppShell` around the router and consults this
 * table for the flags the page used to pass itself; pages render only their
 * content.
 *
 * Keyed by the SAME route keys as `PAGE_ACCESS` and resolved through the same
 * `findPageAccessKey` longest-prefix match, so '/issues/:id' inherits
 * '/issues''s row and the two tables cannot disagree about what a "page" is.
 * `shell.test.ts` enforces key parity in both directions.
 *
 * `null` means NO shell: the page owns its whole viewport (onboarding renders
 * its own first-run layout with no topbar — `AppShell`'s own docs explain why
 * that page must not be wrapped). Routes with no `PAGE_ACCESS` key at all
 * (login/register/password flows, unsubscribe, the legacy `Redirect` rows,
 * '/' and '*') resolve to `null` the same way, which preserves their current
 * bare rendering.
 *
 * The flag values are the EXACT ones each page passed before the hoist —
 * `requireProject` defaulted true on `AppShell` and false through
 * `AdminShell`, which is why the admin rows below mostly read
 * `requireProject: false`. Changing one is a behavior change to that page's
 * empty-scope steering, not a cleanup.
 */

import { findPageAccessKey } from './page-access';

export interface ShellFlags {
  /** Steer to onboarding/Projects when the org has no projects. */
  requireProject: boolean;
  /** The page cannot render without a current app (Issues, Events, …). */
  requireApp: boolean;
}

const APP: ShellFlags = { requireProject: true, requireApp: true };
const PROJECT: ShellFlags = { requireProject: true, requireApp: false };
const BARE: ShellFlags = { requireProject: false, requireApp: false };

export const SHELL_FLAGS: Record<string, ShellFlags | null> = {
  // --- Monitor ---
  '/overview': APP,
  '/issues': APP,
  '/performance': APP,
  // --- Explore ---
  '/events': APP,
  '/transactions': APP,
  '/sessions': APP,
  '/users': APP,
  '/persons': APP,
  '/devices': APP,
  '/screens': APP,
  '/workflows': APP,
  // --- Analyze ---
  '/active-users': PROJECT,
  '/funnels': APP,
  '/journeys': APP,
  // --- Uptime ---
  '/monitors': PROJECT,
  // --- Admin (the rail itself is AdminShell's, rendered by each page) ---
  '/admin': BARE,
  '/admin/members': BARE,
  '/admin/roles': BARE,
  '/admin/projects': BARE,
  '/admin/environments': PROJECT,
  '/admin/settings': BARE,
  '/admin/source-maps': PROJECT,
  '/admin/alerts': PROJECT,
  '/admin/storage': BARE,
  '/admin/privacy': { requireProject: false, requireApp: true },
  '/admin/wall-of-shame': BARE,
  '/admin/ingest-failures': BARE,
  '/admin/purge': { requireProject: false, requireApp: true },
  // --- Self-service / other ---
  '/account': BARE,
  // First-run flow: full-viewport page with its own layout and no topbar —
  // wrapping it would put an org switcher over a screen that exists precisely
  // because the user has nowhere to switch to.
  '/onboarding': null,
  '/docs': BARE,
};

/**
 * The shell for a concrete path, or `null` for a bare page.
 *
 * Fails CLOSED (no shell) for an unknown path: the parity test guarantees
 * every real route has a row, so an unknown path is a typo'd link about to
 * hit the '*' redirect — wrapping that flash in a shell would mount the whole
 * session bootstrap for a page that navigates away on its first tick.
 */
export function resolveShell(path: string): ShellFlags | null {
  const key = findPageAccessKey(path);
  return key === null ? null : (SHELL_FLAGS[key] ?? null);
}
