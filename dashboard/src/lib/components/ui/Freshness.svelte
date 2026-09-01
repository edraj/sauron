<script lang="ts">
  /**
   * "as of 14:32" for a stale-while-revalidate view.
   *
   * Wiring only — every decision lives in `models/freshness.ts` with a
   * co-located test, per the house rule that components carry no logic worth
   * testing. Pass `computedAt` whenever the endpoint discloses one; it is the
   * honest clock, and the model prefers it over the local fetch time.
   *
   * # Why this is plain text and not a Badge
   *
   * It was a bordered `Badge` and that was wrong. A stamp is passive metadata
   * about the data below, but the badge gave it a border, a background and a
   * tone — the same visual weight as the Export and Refresh buttons it sits
   * between, and in the warning tone it read as an alert on pages whose data
   * was merely a few minutes old. It has to be legible without competing:
   * quiet text that recedes, and darkens only when the age is genuinely
   * surprising.
   *
   * The timestamp and the "updating" marker stay two INDEPENDENT facts. A
   * refresh being in flight says nothing about how old the figure on screen
   * is, and running them together ("refreshing, so this must be current") is
   * the inference this exists to prevent — a cached endpoint can be serving an
   * answer from hours ago while a revalidate runs.
   */
  import { t } from '../../i18n';
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

  const view = $derived(viewFreshness({ computedAt, fetchedAt, revalidating, staleAfterMs }));
</script>

{#if view}
  <span class="fresh" class:stale={view.tone === 'warning'} title={view.title}>
    {view.label}{#if view.updating}<span class="updating"> · {t('time.updating')}</span>{/if}
  </span>
{/if}

<style>
  .fresh {
    font-size: 11.5px;
    color: var(--text-faint);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  /* Only once the age is genuinely surprising — six hours, see
     DEFAULT_STALE_AFTER_MS. Colour on the text, never a filled chip: the point
     is to be noticed when read, not to compete for attention when not. */
  .fresh.stale {
    color: var(--warning);
  }
  .updating {
    color: var(--text-faint);
  }
</style>
