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

import { findPageAccessKey, PAGE_ACCESS } from './page-access';

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
  '/retention': APP,
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

/**
 * Whether a page's data honors the topbar release switcher.
 *
 * `false` pages read rollups keyed on (app, period, env) only — they have no
 * release dimension to filter on — so while a release is selected they show
 * ALL releases regardless, and `ReleaseScopeNote` says so. `true` is exactly
 * the pages whose requests `api/scope.ts`'s `RELEASE_SCOPED_URL` attaches
 * `?release=` to: the four searched list pages (Issues' detail view rides the
 * `/issues` key via `findPageAccessKey`, same as every other
 * shared-with-its-list-page route in this file) and `/users`.
 *
 * On the four list pages `true` is a claim about the LIST, not about
 * everything on the page. Their side widgets — Issues' stat tiles, Sessions'
 * duration timeseries and summary — read the same release-blind rollups the
 * `false` pages do and stay aggregate across every release. That is why
 * `ui.release.showingAll` is phrased "charts and totals … only lists follow
 * the release switcher", and why those widgets key on `sessionStore.scopeKey`
 * while the list beside them keys on `scopeKeyWithRelease`
 * (`models/release-scope-key-parity.test.ts` enforces the split).
 *
 * `/users` is the exception, and `FULLY_RELEASE_AWARE` below names it: its
 * summary tiles, activity chart, persons list and count all read
 * release-keyed rollups (migration 82), so every number on it follows the
 * switcher and the note's sentence would be false there.
 *
 * Keyed identically to `SHELL_FLAGS`: one entry per `PAGE_ACCESS` key, parity
 * tested against it in both directions by `shell.test.ts` so a new page must
 * make a deliberate release-awareness decision rather than defaulting into
 * one silently.
 */
export const RELEASE_AWARE: Record<string, boolean> = {
  // --- Monitor ---
  '/overview': false,
  '/issues': true,
  '/performance': false,
  // --- Explore ---
  '/events': true,
  '/transactions': true,
  '/sessions': true,
  '/users': true,
  // The person DETAIL page. Its reads (`…/persons/{id}/summary`, `…/timeline`)
  // are one person's history, not a list a release could narrow, and are not
  // in `RELEASE_SCOPED_URL`.
  '/persons': false,
  '/devices': false,
  '/screens': false,
  '/workflows': false,
  // --- Analyze ---
  '/active-users': false,
  '/funnels': false,
  '/journeys': false,
  '/retention': false,
  // --- Uptime ---
  '/monitors': false,
  // --- Admin ---
  '/admin': false,
  '/admin/members': false,
  '/admin/roles': false,
  '/admin/projects': false,
  '/admin/environments': false,
  '/admin/settings': false,
  '/admin/source-maps': false,
  '/admin/alerts': false,
  '/admin/storage': false,
  '/admin/privacy': false,
  '/admin/wall-of-shame': false,
  '/admin/ingest-failures': false,
  '/admin/purge': false,
  // --- Self-service / other ---
  '/account': false,
  '/onboarding': false,
  '/docs': false,
};

/**
 * The release-aware pages on which EVERY widget follows the switcher — the
 * charts and totals as well as the list.
 *
 * A subset of the `true` rows of `RELEASE_AWARE` (`shell.test.ts` enforces
 * that, and that each is a real envAware `PAGE_ACCESS` key). Its one job is to
 * keep `ReleaseScopeNote` off these pages: "charts and totals include all
 * releases" is a true sentence on Issues and Sessions and a false one here.
 *
 * Deliberately a separate, short set rather than a third value folded into
 * `RELEASE_AWARE`: that table answers "does this page key on the release", and
 * every reader of it (the parity tests included) only needs the boolean.
 */
export const FULLY_RELEASE_AWARE: ReadonlySet<string> = new Set(['/users']);

/**
 * Whether `ReleaseScopeNote` should render on the page at `path`.
 *
 * The note is a statement about the CHARTS AND TOTALS on an env-scoped
 * telemetry page: they include every release, and only lists follow the
 * switcher. That claim is true on most release-aware pages too — Issues' stat
 * tiles and Sessions' duration timeseries/summary read the same
 * release-blind rollups their neighbours on `false` pages do (see
 * `RELEASE_AWARE` above) — so the note shows on every env-scoped telemetry
 * page except the `FULLY_RELEASE_AWARE` ones, where the charts and totals
 * follow the switcher as well and the sentence would be false. Keying the
 * exclusion on `RELEASE_AWARE` itself would be wrong the other way: it would
 * leave the aggregate side widgets on `/issues` and `/sessions` silently
 * unexplained.
 *
 * Past that one exclusion, `PAGE_ACCESS[key].envAware` is the gate: it is exactly "this page
 * reads env-scoped telemetry", which is the read path a release (itself an
 * env-scoped concept) could plausibly narrow. It excludes pages with no
 * telemetry at all (admin, account, docs, onboarding) and app-wide-config
 * pages like `/funnels` that are deliberately NOT envAware (see
 * `page-access.ts`) — on those there are no per-release aggregates to
 * mis-read, so the note would be noise.
 *
 * Fails CLOSED (no note) for an unknown path or a page with no PAGE_ACCESS
 * entry — the opposite default from `resolveShell`, because here "unknown"
 * must not be misread as "this page ignores your release filter".
 *
 * `RELEASE_AWARE` is deliberately NOT read here; it is the source of truth
 * for which pages key their requests on `scopeKeyWithRelease`
 * (`models/release-scope-key-parity.test.ts`). An earlier round also exported
 * an `isReleaseAware(path)` predicate; nothing ever called it (the release
 * interceptor keys on the URL, not the route — see `api/scope.ts`'s
 * `RELEASE_SCOPED_URL`), so it was removed rather than left as a second,
 * drifting answer to "does this page honour the release filter".
 */
export function showsReleaseNote(path: string): boolean {
  const key = findPageAccessKey(path);
  if (key === null) return false;
  if (FULLY_RELEASE_AWARE.has(key)) return false;
  return PAGE_ACCESS[key]?.envAware === true;
}
