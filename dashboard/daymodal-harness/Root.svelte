<script lang="ts">
  /**
   * The REAL Performance page behind the REAL router.
   *
   * The route is unguarded — `routes.ts` wraps it in `guarded()`, which would
   * bounce to /login without an auth boot this harness has no reason to stub.
   */
  import Router from 'svelte-spa-router';
  import Performance from '../src/pages/Performance.svelte';
  import ActiveUsers from '../src/pages/ActiveUsers.svelte';
  import Transactions from '../src/pages/Transactions.svelte';
  import IssueDetail from '../src/pages/IssueDetail.svelte';
  import MonitorDetail from '../src/pages/MonitorDetail.svelte';
  import Environments from '../src/pages/Environments.svelte';
  import FunnelBuilder from '../src/pages/FunnelBuilder.svelte';
  import Inspector from '../src/pages/Inspector.svelte';
  import Purge from '../src/pages/Purge.svelte';
  import WallOfShame from '../src/pages/WallOfShame.svelte';
  import Storage from '../src/pages/Storage.svelte';

  const routes = { '/performance': Performance, '/active-users': ActiveUsers,
    '/transactions': Transactions,
    '/issues/:id': IssueDetail,
    '/monitors/:id': MonitorDetail,
    '/environments': Environments,
    '/funnels': FunnelBuilder,
    '/inspector': Inspector,
    '/purge': Purge,
    '/admin/wall-of-shame': WallOfShame,
    '/admin/storage': Storage, '*': Performance };
</script>

<Router {routes} />
