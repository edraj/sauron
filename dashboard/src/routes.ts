import { wrap } from 'svelte-spa-router/wrap';
import type { Component } from 'svelte';
import { authStore } from './lib/stores/auth.svelte';

// Redirect is the ONLY statically-imported route component. It is a handful of
// lines, and '/' and '*' resolve through it on the very first tick — lazily
// importing it would add a round trip to the one navigation that cannot afford
// one. Every real page below is loaded on demand instead: statically importing
// all 39 meant an unauthenticated visitor downloaded Docs, the nine admin
// pages, FunnelBuilder and the member dialogs in order to render a login form.
import Redirect from './lib/components/Redirect.svelte';
// The load boundary for every lazy page: it owns the loading state and the
// error state. Static, necessarily — the component that reports a failed
// download cannot itself be a download.
import LazyRoute from './lib/components/LazyRoute.svelte';

const authed = () => authStore.isAuthenticated;

// A pending password change blocks every authenticated page except the one
// that can resolve it (the server enforces this for real via Task 7's
// extractor gate — this is convenience only). Deliberately does NOT navigate
// itself: App.svelte's `conditionsFailed` handler does that, so there is a
// single place deciding between /login and /change-password instead of two
// navigations racing each other.
function passwordCurrent(): boolean {
  return !authStore.mustChangePassword;
}

/**
 * A dynamic `import()` of a page component.
 *
 * Vite turns each of these call sites into its own chunk, which is the whole
 * point — the literal must stay inline at the call site. Hoisting the specifier
 * into a variable, or building it by concatenation, defeats the static analysis
 * and silently collapses the split back into one bundle.
 */
type PageLoader = () => Promise<{ default: Component<never> }>;

/**
 * A lazy page, mounted through `LazyRoute` rather than the router's own
 * `asyncComponent`.
 *
 * `asyncComponent` was the obvious form and it is the reason C1 existed: the
 * router `await`s the loader with no `catch` (Router.svelte:539), so a rejected
 * import leaves the loading component mounted forever — a spinner with no error
 * and no way out. `LazyRoute` owns the load instead, so a rejected import
 * becomes a rendered failure with a working reload; see the header comment
 * there, and `models/route-chunk.ts` for why that reload is the only recovery.
 *
 * The `import()` literal still has to stay inline at each call site below —
 * that is what Vite's static analysis splits on — so this only moves WHO calls
 * the loader, not where it is written.
 */
function guarded(loader: PageLoader) {
  return wrap({
    component: LazyRoute as never,
    props: { loader },
    conditions: [authed, passwordCurrent],
  });
}

/** Lazy, but with no route conditions — see the per-route comments below. */
function open(loader: PageLoader) {
  return wrap({ component: LazyRoute as never, props: { loader } });
}

/**
 * Warm the chunk for the route authenticated users actually land on.
 *
 * That route is **`/overview`**: `Redirect` sends an authenticated visitor
 * there from `/` and from any unmatched path, `Login.svelte` pushes there after
 * a successful sign-in, and `ChangePassword.svelte` replaces to it. (This used
 * to warm `Issues.svelte` on the strength of App.svelte's `push('/issues')`,
 * which only fires for an authenticated user sitting on a PUBLIC_ROUTE — a path
 * almost nobody takes, because Login navigates to /overview itself. So it
 * fetched a chunk the dominant path never renders while the chunk it does
 * render still paid the round trip this function exists to remove.)
 *
 * Idempotent twice over: the `started` flag plus the module system's own import
 * cache.
 */
let started = false;
export function prefetchLandingRoute(): void {
  if (started) return;
  // Known-offline is the one transient cause worth declining, because a failed
  // `import()` is NOT free: the module map records the specifier as failed, and
  // later imports of it replay that rejection instead of re-fetching. Warming
  // while offline would therefore poison the landing route for the rest of the
  // page's life. Skipping without setting `started` lets the caller's $effect
  // warm it later. (`navigator.onLine === false` is reliable in the negative
  // direction; `true` says nothing, which is fine — that is the normal path.)
  if (typeof navigator !== 'undefined' && navigator.onLine === false) return;
  started = true;
  // A failure here is not silently absorbed the way this `catch` suggests on
  // its own: it poisons the module map, so the subsequent navigation to
  // /overview fails too — and lands on LazyRoute's error state, which names the
  // failure and offers the reload that actually fixes it. The `catch` only
  // stops an unhandled rejection from being logged twice.
  void import('./pages/Overview.svelte').catch(() => {});
}

export const routes = {
  '/login': open(() => import('./pages/Login.svelte')),
  '/register': open(() => import('./pages/Register.svelte')),
  // Both are CONDITION-FREE — no `authed`, no `passwordCurrent`. Adding either
  // would fire conditionsFailed, which pushes to /login or /change-password and
  // makes a reset link unusable. (`wrap` with no `conditions` key stores
  // `conditions: undefined`, so this is identical to the bare form these used
  // to have; only the import became lazy.)
  '/forgot-password': open(() => import('./pages/ForgotPassword.svelte')),
  '/reset-password': open(() => import('./pages/ResetPassword.svelte')),
  // Ungated on passwordCurrent — otherwise a temp-password holder redirected
  // here would immediately redirect right back to itself.
  '/change-password': wrap({
    component: LazyRoute as never,
    props: { loader: () => import('./pages/ChangePassword.svelte') },
    conditions: [authed],
  }),
  // Condition-free, and deliberately NOT in App.svelte's PUBLIC_ROUTES either.
  // That array drives an $effect that pushes authenticated users OFF those
  // paths, which is exactly wrong here: a logged-in user clicking an
  // unsubscribe link must still see the confirmation.
  '/unsubscribe': open(() => import('./pages/Unsubscribe.svelte')),
  '/onboarding': guarded(() => import('./pages/Onboarding.svelte')),

  // Monitor
  '/overview': guarded(() => import('./pages/Overview.svelte')),
  '/issues': guarded(() => import('./pages/Issues.svelte')),
  '/issues/:id': guarded(() => import('./pages/IssueDetail.svelte')),
  '/performance': guarded(() => import('./pages/Performance.svelte')),

  // Explore
  '/events': guarded(() => import('./pages/Events.svelte')),
  '/sessions': guarded(() => import('./pages/SessionsList.svelte')),
  '/sessions/:id': guarded(() => import('./pages/SessionDetail.svelte')),
  '/users': guarded(() => import('./pages/UsersExplorer.svelte')),
  '/persons/:distinctId': guarded(() => import('./pages/PersonProfile.svelte')),
  '/devices': guarded(() => import('./pages/DevicesInventory.svelte')),
  '/devices/:key': guarded(() => import('./pages/DeviceDetail.svelte')),
  '/screens': guarded(() => import('./pages/ScreensList.svelte')),
  '/screens/:name': guarded(() => import('./pages/ScreenDetail.svelte')),
  '/workflows': guarded(() => import('./pages/WorkflowsList.svelte')),

  // Analyze
  '/active-users': guarded(() => import('./pages/ActiveUsers.svelte')),
  '/funnels': guarded(() => import('./pages/FunnelBuilder.svelte')),
  '/journeys': guarded(() => import('./pages/JourneyExplorer.svelte')),

  // Uptime
  '/monitors': guarded(() => import('./pages/Monitors.svelte')),
  '/monitors/:id': guarded(() => import('./pages/MonitorDetail.svelte')),

  // Admin. Nested routes, each with its own PAGE_ACCESS entry — the children
  // carry genuinely different permissions at different levels, so a single
  // '/admin' requirement could not express them. '/admin' itself is ungated
  // and resolves to the first child the caller can reach.
  //
  // Route conditions deliberately do NOT carry permissions: a failed condition
  // fires `conditionsFailed`, which navigates, and a deep link the user cannot
  // open should keep its URL and explain itself. AppShell resolves the path
  // through PAGE_ACCESS and renders PermissionDenied in place of the page.
  // Both layers stay cosmetic — every endpoint 403s on its own.
  '/admin': guarded(() => import('./pages/AdminIndex.svelte')),
  '/admin/members': guarded(() => import('./pages/Members.svelte')),
  '/admin/roles': guarded(() => import('./pages/Roles.svelte')),
  '/admin/projects': guarded(() => import('./pages/Projects.svelte')),
  '/admin/environments': guarded(() => import('./pages/Environments.svelte')),
  '/admin/settings': guarded(() => import('./pages/SettingsApp.svelte')),
  '/admin/source-maps': guarded(() => import('./pages/SourceMaps.svelte')),
  '/admin/alerts': guarded(() => import('./pages/Alerts.svelte')),
  '/admin/storage': guarded(() => import('./pages/Storage.svelte')),
  '/admin/privacy': guarded(() => import('./pages/Inspector.svelte')),
  '/admin/wall-of-shame': guarded(() => import('./pages/WallOfShame.svelte')),
  '/admin/ingest-failures': guarded(() => import('./pages/IngestFailures.svelte')),

  '/account': guarded(() => import('./pages/Account.svelte')),

  // Legacy paths. Bare Redirect entries, no guarded() — they mount no AppShell
  // and have no page permission, so they are listed in LEGACY_REDIRECTS in
  // page-access.test.ts rather than carrying PAGE_ACCESS rows. Kept so
  // bookmarks and any hardcoded links survive the move. `Redirect` must stay
  // named on each of these SOURCE LINES: that test asserts the value, not just
  // the key, so the exemption expires the day one becomes a real page.
  '/members': wrap({ component: Redirect as never, props: { to: '/admin/members' } }),
  '/projects': wrap({ component: Redirect as never, props: { to: '/admin/projects' } }),
  '/settings': wrap({ component: Redirect as never, props: { to: '/admin/settings' } }),
  '/source-maps': wrap({ component: Redirect as never, props: { to: '/admin/source-maps' } }),
  '/alerts': wrap({ component: Redirect as never, props: { to: '/admin/alerts' } }),
  '/storage': wrap({ component: Redirect as never, props: { to: '/admin/storage' } }),
  '/inspector': wrap({ component: Redirect as never, props: { to: '/admin/privacy' } }),

  // Docs / integration guides
  '/docs': guarded(() => import('./pages/Docs.svelte')),

  '/': Redirect,
  '*': Redirect,
};
