<script lang="ts">
  import { t } from './lib/i18n';
  import { onMount } from 'svelte';
  import Router, { location, push } from 'svelte-spa-router';
  import { routes, prefetchLandingRoute } from './routes';
  import { authStore } from './lib/stores/auth.svelte';
  import AppShell from './lib/components/layout/AppShell.svelte';
  import { resolveShell } from './lib/models/shell';
  import { setCurrentRoute } from './lib/stores/current-route';
  import Toast from './lib/components/ui/Toast.svelte';
  import Spinner from './lib/components/ui/Spinner.svelte';
  import EyeMark from './lib/components/EyeMark.svelte';
  import { sessionStore } from './lib/stores/session.svelte';
  import { toastStore } from './lib/stores/toast.svelte';
  import { parseScopeSearch, scopeSearch } from './lib/models/scope-url';

  let booted = $state(false);

  // The topbar scope travels in the URL (`models/scope-url.ts`): a link copied
  // from the address bar opens on the same org / project / app / release /
  // environment for whoever it is pasted to.
  //
  // Seeded HERE, at the top of the root component's script, because that runs
  // before any child mounts — `AppShell`'s `onMount` is what starts the first
  // `sessionStore.load()`, and Login's `load(true)` comes later still, so the
  // seed survives a signed-out visitor's detour through /login.
  sessionStore.seedFromUrl(parseScopeSearch(window.location.search));

  /**
   * Rewrite `location.search` to the store's scope, in place.
   *
   * `history.replaceState`, never a navigation: a scope switch is not a page
   * change, and the search part is outside the hash router entirely, so no
   * `hashchange` fires and no page re-reads its own query. The current
   * `history.state` is passed through because svelte-spa-router keeps its
   * scroll positions there.
   *
   * Not before the first load has resolved the scope — writing the
   * half-resolved store over the link would erase the link before it is read.
   *
   * And when a loaded session ENDS (logout, a password reset — every path that
   * calls `sessionStore.reset()`), the scope is taken back off the URL: it
   * belonged to that session. Left in place, a reload of the login page would
   * seed the next person's first load from it, as if they had followed a link
   * — landing them in the previous user's org, or toasting about a link they
   * never opened. A visitor who arrives signed out with a shared link is the
   * opposite case and is left alone: nothing was mirrored yet, so the link
   * survives the detour through /login.
   */
  let mirrored = false; // plain, not $state: bookkeeping no render reads
  const NO_SCOPE = { org: null, project: null, app: null, release: null, env: null };
  function mirrorScopeToUrl(): void {
    let scope;
    if (sessionStore.loaded) scope = sessionStore.scopeSelection;
    else if (mirrored) scope = NO_SCOPE;
    else return;
    mirrored = sessionStore.loaded;
    const next = scopeSearch(window.location.search, scope);
    if (next === window.location.search) return;
    window.history.replaceState(
      window.history.state,
      '',
      window.location.pathname + next + window.location.hash,
    );
  }

  // Reads `loaded` and all five scope fields, so it re-runs on every switch.
  $effect(() => {
    mirrorScopeToUrl();
  });

  // Back / Forward restores an older history entry, whose search still names
  // the scope that was current when it was pushed. Within a session the store
  // is the truth and the URL its mirror, so re-assert the store's scope rather
  // than leave the address bar showing (and sharing) a stale one.
  $effect(() => {
    window.addEventListener('popstate', mirrorScopeToUrl);
    return () => window.removeEventListener('popstate', mirrorScopeToUrl);
  });

  // A link that named something this member cannot open lands on the nearest
  // scope they can — say so once, or a teammate reads "no errors" off the
  // wrong app with nothing on screen to suggest it.
  $effect(() => {
    if (sessionStore.linkScopeUnmet.length === 0) return;
    toastStore.push(t('nav.linkScopeUnmet'), 'info', 8000);
    sessionStore.linkScopeUnmet = [];
  });

  // '/reset-password' is DELIBERATELY absent. This array feeds the $effect
  // below that pushes authenticated users to /issues, and a logged-in user
  // clicking their own reset link would be bounced off it before they could use
  // it. It is neither listed here nor guarded in routes.ts, so it simply
  // renders for everyone. '/forgot-password' IS listed: an authenticated user
  // who lands there wants Change password instead.
  //
  // Note `$location` from svelte-spa-router excludes the query string, so the
  // comparison is '/reset-password' even with ?token=… — which is exactly why
  // omitting it here is load-bearing rather than accidental.
  const PUBLIC_ROUTES = ['/login', '/register', '/forgot-password'];

  onMount(async () => {
    await authStore.boot();
    booted = true;
  });

  // Routes are lazily imported, so the landing route would otherwise cost a
  // chunk round trip that the old all-static bundle had already prepaid. That
  // route is '/overview' — where Redirect sends an authenticated visitor from
  // '/' or any unmatched path, and where Login pushes after sign-in — NOT the
  // '/issues' of the effect below, which only fires for an authenticated user
  // who happens to be sitting on a PUBLIC_ROUTE. `prefetchLandingRoute` names
  // the chunk; see its comment in routes.ts.
  //
  // Keyed on `isAuthenticated` rather than fired unconditionally so an
  // anonymous visitor still downloads only the login page, which is the entire
  // point of splitting the routes; that also covers the sign-in transition,
  // where the flag flips after boot.
  $effect(() => {
    if (booted && authStore.isAuthenticated) prefetchLandingRoute();
  });

  // Feeds `refreshRegistry`'s route tags. One writer, so nothing else in the
  // app has to import the router — see `current-route.ts` for why the caching
  // primitive must not depend on svelte-spa-router directly.
  //
  // Deliberately NOT gated on `booted`: a `CachedView` can load before boot
  // settles, and a registration tagged '' would never match a real route and
  // so would never refresh.
  $effect(() => {
    setCurrentRoute($location);
  });

  // Once booted, keep authenticated users out of the login/register pages.
  $effect(() => {
    if (!booted) return;
    if (authStore.isAuthenticated && PUBLIC_ROUTES.includes($location)) {
      push('/issues');
    }
  });

  function onConditionsFailed() {
    // A guarded route rejected the visitor. Tell apart "not logged in" (send
    // to /login) from "logged in but owes a password change" (send to
    // /change-password) — routes.ts's passwordCurrent() reports the latter
    // without navigating itself, so this is the one place that decides,
    // rather than two navigations racing each other.
    if (authStore.isAuthenticated && authStore.mustChangePassword) {
      push('/change-password');
    } else {
      push('/login');
    }
  }

  // The ONE AppShell for the whole session — see `models/shell.ts`. Pages no
  // longer wrap themselves, so between two shelled routes only the routed
  // component swaps and the sidebar/topbar DOM persists; a lazy chunk's
  // loading state now renders inside the shell's content pane instead of
  // replacing the whole viewport. Gated on `isAuthenticated`: an anonymous
  // visitor parked on a guarded path is mid-redirect to /login, and mounting
  // the shell for that one tick would fire the whole session bootstrap.
  const shell = $derived(
    booted && authStore.isAuthenticated ? resolveShell($location) : null,
  );
</script>

{#if !booted}
  <div class="boot">
    <EyeMark size={44} />
    <Spinner size={22} />
    <span class="boot-text">{t('prose.boot.loading')}</span>
  </div>
{:else if shell}
  <AppShell requireProject={shell.requireProject} requireApp={shell.requireApp}>
    <Router {routes} on:conditionsFailed={onConditionsFailed} />
  </AppShell>
{:else}
  <Router {routes} on:conditionsFailed={onConditionsFailed} />
{/if}

<Toast />

<style>
  .boot {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 16px;
  }
  .boot-text {
    color: var(--text-faint);
    font-size: var(--text-sm);
  }
</style>
