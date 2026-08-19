<script lang="ts">
  /**
   * Several real pages behind the real router, in whichever locale `?lang=`
   * seeded.
   *
   * The question this harness answers is not "does `t()` return Arabic" — a
   * unit test settles that. It is whether the ASSEMBLED page survives
   * `dir="rtl"`: whether the sidebar moves to the right, whether IDs, DSNs and
   * stack frames stay left-to-right inside it, whether chevrons point the
   * right way, and whether any physical `left`/`right` declaration strands
   * something on the wrong edge. None of that is visible to `svelte-check` or
   * to vitest.
   *
   * The pages are chosen for layout variety rather than coverage: a form-heavy
   * settings page, a wide sortable table, a chart page, and a detail page with
   * key/value rows.
   *
   * Routes are UNGUARDED — `routes.ts` wraps each in `guarded()`, which would
   * bounce to /login without an auth boot this harness has no reason to stub.
   */
  import Router from 'svelte-spa-router';
  import Account from '../src/pages/Account.svelte';
  import Issues from '../src/pages/Issues.svelte';
  import Sessions from '../src/pages/SessionsList.svelte';
  import Monitors from '../src/pages/Monitors.svelte';
  import Docs from '../src/pages/Docs.svelte';

  const routes = {
    '/account': Account,
    '/issues': Issues,
    '/sessions': Sessions,
    '/monitors': Monitors,
    '/docs': Docs,
    '*': Account,
  };
</script>

<Router {routes} />
