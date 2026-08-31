<script lang="ts">
  /**
   * The REAL Performance page behind the REAL router.
   *
   * The route is unguarded — `routes.ts` wraps it in `guarded()`, which would
   * bounce to /login without an auth boot this harness has no reason to stub.
   */
  import Router from 'svelte-spa-router';
  import Performance from '../src/pages/Performance.svelte';

  const routes = { '/performance': Performance, '*': Performance };
</script>

<Router {routes} />
