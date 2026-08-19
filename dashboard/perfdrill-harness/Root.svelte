<script lang="ts">
  /**
   * Both real pages behind the REAL `svelte-spa-router`.
   *
   * Mounting the two components side by side would have been simpler and would
   * have verified nothing about the part most likely to be wrong: the modal's
   * "Open in Transactions" link is a hash URL carrying two encoded chips, and
   * whether the Transactions page can parse them back out of `$querystring` is
   * the actual question. That needs a router, a real navigation and the real
   * `parseFilters`.
   *
   * The routes are UNGUARDED here — `routes.ts` wraps each in `guarded()`,
   * which would bounce to /login without an auth boot this harness has no
   * reason to stub. Everything downstream of the route (AppShell, the session
   * store, both pages) is the real code.
   */
  import Router from 'svelte-spa-router';
  import Performance from '../src/pages/Performance.svelte';
  import Transactions from '../src/pages/Transactions.svelte';

  const routes = {
    '/performance': Performance,
    '/transactions': Transactions,
    '*': Performance,
  };
</script>

<Router {routes} />
