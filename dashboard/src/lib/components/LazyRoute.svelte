<!--
  The boundary every lazy route is mounted through.

  ## Why this exists rather than `wrap({ asyncComponent })`

  svelte-spa-router does `const loaded = await obj()` (Router.svelte:539) with no
  `catch`. When the dynamic import rejects, `component` is never reassigned, the
  `loadingComponent` stays mounted, and the app sits on a spinner INDEFINITELY —
  no error, no retry, no recovery short of a manual reload. A chunk 404 is
  routine (a deploy lands mid-session and this tab still holds an `index.html`
  naming hashes the server no longer has; a CDN edge misses; one response is
  dropped), so the split traded a visible failure for a silent hang.

  Nor can the router be made to show the failure: it caches the resolved
  component per route object (`if (componentObj != obj)`), and it has nowhere to
  put an error even if it noticed one. Owning the load here is what turns a
  rejected import into something renderable.

  Note there is no retry — `models/route-chunk.ts` records the measurement that
  removed it (a failed specifier is cached as failed, so a second `import()`
  never reaches the network). Recovery is the document reload `RouteError`
  offers.

  ## Contract with the router

  Mounted via `wrap({ component: LazyRoute, props: { loader } })`, so the router
  resolves instantly and never awaits the chunk itself. `params` is the router's
  match object, forwarded to the page unchanged.

  Note the router reuses ONE LazyRoute instance across routes (`component` is
  the same reference for every entry in the table), so a route change arrives as
  a new `loader` prop, not a remount. The `$effect` below is what makes that
  work, and `token` is what stops a slow chunk from landing after the user has
  navigated on.
-->
<script lang="ts">
  import type { Component } from 'svelte';
  import RouteLoading from './RouteLoading.svelte';
  import RouteError from './RouteError.svelte';
  import { loadRouteChunk } from '../models/route-chunk';

  /**
   * This boundary is generic over all 38 pages, each with its own props, so it
   * renders through a permissive component type — the same cast `routes.ts` used
   * to apply at its own `asyncComponent` boundary.
   */
  type PageComponent = Component<Record<string, unknown>>;

  interface Props {
    loader: () => Promise<{ default: unknown }>;
    /** The router's match object; `null` on routes with no params. */
    params?: Record<string, string> | null;
  }

  let { loader, params = null }: Props = $props();

  // Capitalized so it can be rendered directly as `<Page />` (Svelte 5's
  // replacement for the deprecated `<svelte:component>`), and `$state.raw`
  // because a Svelte component is a function: `$state` would deep-proxy it and
  // break identity.
  let Page = $state.raw<PageComponent | null>(null);
  let failure = $state<string | null>(null);

  // Plain `let`s, not `$state`: read only for control flow, never rendered, so
  // making them reactive would just re-run the effect that writes them.
  let token = 0;
  // The loader the current `Page` was loaded from. See the guard in the $effect.
  let activeLoader: Props['loader'] | null = null;

  async function run(wanted: Props['loader'], mine: number) {
    const outcome = await loadRouteChunk(wanted);
    // The user navigated away while the chunk was in flight and the router has
    // already handed us a different loader. Dropping the stale result here is
    // what keeps a slow page from replacing the one asked for second.
    if (mine !== token) return;
    if (outcome.status === 'loaded') {
      Page = outcome.module.default as PageComponent;
      failure = null;
    } else {
      Page = null;
      failure = outcome.message;
    }
  }

  $effect(() => {
    // Reading `loader` is what subscribes this effect to route changes.
    const wanted = loader;

    // MUST come before anything below it. The router hands props down as
    // `{...props}` on a legacy (non-runes) `<svelte:component>`, and it
    // REASSIGNS that object on every location change — `props = routesList[i]
    // .props` (Router.svelte:562). Legacy invalidation uses `safe_not_equal`,
    // which reports any object as changed even when it is the identical
    // reference, so the spread republishes `loader` on every location change
    // including a pure querystring edit.
    //
    // Without this guard that is an infinite remount loop, and pages that write
    // their filter state into the querystring drive it: Issues mounts → pushes
    // `?filter=…` → props republish → effect re-runs → `Page = null` → Issues
    // remounts → pushes again. Observed live at `#/issues`: a permanent
    // "Loading page…" and an unbounded flood of `/issues` + `/issues/stats`
    // requests. Comparing the loader itself — the only thing that actually
    // identifies a route here — is what makes a republish a no-op.
    if (wanted === activeLoader) return;
    activeLoader = wanted;

    const mine = ++token;
    Page = null;
    failure = null;
    void run(wanted, mine);
  });
</script>

{#if failure !== null}
  <RouteError message={failure} />
{:else if Page}
  <!-- `params` is forwarded only when the route actually matched some, so a
       page that declares no `params` prop is never handed one. -->
  <Page {...params ? { params } : {}} />
{:else}
  <RouteLoading />
{/if}
