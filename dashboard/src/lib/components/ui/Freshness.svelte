<script lang="ts">
  /**
   * "as of 14:32 · Updating…" for any stale-while-revalidate view.
   *
   * Wiring only — every decision lives in `models/freshness.ts` with a
   * co-located test, per the house rule that components carry no logic worth
   * testing. Pass `computedAt` whenever the endpoint discloses one; it is the
   * honest clock, and the model prefers it over the local fetch time.
   *
   * The timestamp and the "Updating…" marker are two INDEPENDENT facts and are
   * rendered as such. A refresh being in flight says nothing about how old the
   * figure currently on screen is, and running them together ("refreshing, so
   * this must be current") is the inference this component exists to prevent —
   * a cached endpoint can be serving an answer from hours ago while a
   * revalidate runs.
   */
  import { t } from '../../i18n';
  import Badge from './Badge.svelte';
  import { viewFreshness } from '../../models/freshness';

  interface Props {
    /** The server's own stamp, where the endpoint returns one. */
    computedAt?: string | null;
    /** `CachedView.fetchedAt`. */
    fetchedAt?: number | null;
    /** `CachedView.revalidating`. */
    revalidating?: boolean;
    staleAfterMs?: number;
  }

  let { computedAt = null, fetchedAt = null, revalidating = false, staleAfterMs }: Props = $props();

  const view = $derived(
    viewFreshness({ computedAt, fetchedAt, revalidating, staleAfterMs }),
  );
</script>

{#if view}
  <span class="fresh" title={view.title}>
    <Badge tone={view.tone} size="sm">{view.label}</Badge>
    {#if view.updating}
      <span class="updating">{t('time.updating')}</span>
    {/if}
  </span>
{/if}

<style>
  .fresh {
    display: inline-flex;
    align-items: center;
    gap: 7px;
  }
  .updating {
    font-size: 11.5px;
    color: var(--text-muted);
    /* No animation: this sits beside a Badge that does not move, and a pulsing
       label next to a static one reads as an error state. The spinner on
       RefreshButton already carries motion for an explicit refresh. */
  }
</style>
