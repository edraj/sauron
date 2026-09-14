<script lang="ts">
  /**
   * "Charts and totals include all releases" caption for env-scoped telemetry
   * pages while a release is selected.
   *
   * Rendered once, in `AppShell`, directly above `{@render children()}` — not
   * per-page — so a route gets the caption for free the moment
   * `models/shell.ts`'s `showsReleaseNote` says so: any env-scoped telemetry
   * page (`PAGE_ACCESS[key].envAware === true`). That includes the
   * `RELEASE_AWARE` list pages, whose side widgets are aggregate even though
   * their list narrows; it keeps the note off pages with no telemetry at all
   * (admin, account, docs) with no per-page wiring to forget.
   *
   * Reads the route the same way `AppShell` itself does (`location` from
   * `svelte-spa-router`, aliased to avoid shadowing `window.location`) rather
   * than through the non-reactive `stores/current-route` bridge, which exists
   * for a cache tag and is documented as read-once, not for driving markup.
   */
  import { location as routePath } from 'svelte-spa-router';
  import { sessionStore } from '../../stores/session.svelte';
  import { showsReleaseNote } from '../../models/shell';
  import { t } from '../../i18n';

  const show = $derived(sessionStore.currentRelease !== null && showsReleaseNote($routePath));
</script>

{#if show}
  <p class="release-note muted" role="status">{t('ui.release.showingAll')}</p>
{/if}

<style>
  .release-note {
    margin: 0 0 8px;
    font-size: 12px;
  }
</style>
