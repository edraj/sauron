<script lang="ts">
  import { t } from '../lib/i18n';
  import { replace } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import { firstAccessibleAdminPath } from '../lib/models/admin-nav';
  import { sessionStore } from '../lib/stores/session.svelte';

  // Derived and gated on `loaded`, mirroring AppShell.svelte:61-62 — NOT
  // resolved once in onMount. `sessionStore.can()` fails CLOSED while the
  // session is still in flight (session.svelte.ts:176 returns false for every
  // permission until `access` arrives), so a direct landing on /admin — hard
  // refresh, deep link, bookmark, new tab — finds all of ADMIN_NAV
  // unreachable. Latched into a one-shot `$state`, that false denial would
  // never self-correct once the grants did arrive, showing a fully-privileged
  // owner a permanent denial page. Same reasoning AppShell.svelte:39-42 gives
  // for deriving rather than latching its own gates.
  const target = $derived(sessionStore.loaded ? firstAccessibleAdminPath() : null);
  const denied = $derived(sessionStore.loaded && target === null);

  // `replace`, not `push`: /admin is a resolver, not a destination. Pushing
  // would trap Back on a path that immediately forwards again.
  $effect(() => {
    if (target) replace(target);
  });
</script>

<AppShell requireProject={false}>
  {#if denied}
    <!--
      A plain EmptyState rather than PermissionDenied, for the same reason
      AppShell.svelte:105-110 uses one for its analogous `noAccess` state:
      landing here means none of ADMIN_NAV's independent requirements was met,
      and PermissionDenied's copy can only name ONE permission. Synthesizing
      that from whichever item happens to sit first in the array would let a
      cosmetic reorder silently change which permission gets blamed.
    -->
    <EmptyState
      title={t('admin.empty.title')}
      description={t('admin.empty.body')}
      icon="lock"
    />
  {/if}
</AppShell>
