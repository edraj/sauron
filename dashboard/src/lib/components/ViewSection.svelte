<!--
  The body of ONE independently-loaded section of a page: a skeleton while its
  own request is out, its own error (with a retry) if that request failed, and
  the content once there is something to show.

  Exists because detail pages read their sections separately — the record in one
  request, each heavy block in another — so a page is no longer "loading" or
  "loaded" as a whole. Without this every card re-derives the same three-way
  branch, and the copies drift: one forgets the retry, another shows the
  skeleton over data that is merely revalidating.

  The rules, which are `CachedView`'s own:

  - Skeleton only when there is NOTHING to show. A section that is revalidating
    over data it already has keeps showing that data.
  - An error only when the failure left nothing to show. The retry forces the
    network, since honouring the cache there makes the button look broken.
-->
<script lang="ts" generics="T">
  import type { Snippet } from 'svelte';
  import { t } from '../i18n';
  import Skeleton from './ui/Skeleton.svelte';
  import Button from './ui/Button.svelte';
  import type { CachedView } from '../stores/cached-view.svelte';

  interface Props {
    view: CachedView<T>;
    /** Placeholder lines — describe the shape of what is coming. */
    rows?: number;
    /** Height of each placeholder line; a chart is one tall row. */
    height?: string;
    /**
     * Pad the skeleton and the error. For a card rendered with `padding="none"`
     * (one that holds a full-bleed table), whose content brings its own.
     */
    padded?: boolean;
    /**
     * Hold the content back although the view has loaded — for a section that
     * also needs something another request supplies.
     */
    waiting?: boolean;
    children: Snippet;
  }

  let { view, rows = 4, height, padded = false, waiting = false, children }: Props = $props();
</script>

{#if view.hasData && !waiting}
  {@render children()}
{:else if view.error && !view.loading}
  <div class="vs-state" class:padded>
    <p class="muted">{view.error}</p>
    <Button variant="secondary" size="sm" onclick={() => view.reload()}>
      {t('common.retry')}
    </Button>
  </div>
{:else}
  <div class="vs-state" class:padded>
    <Skeleton {rows} {height} />
  </div>
{/if}

<style>
  .vs-state {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
  }
  .vs-state.padded {
    padding: 16px;
  }
  .vs-state p {
    margin: 0;
  }
</style>
