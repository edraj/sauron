<!--
  The body of ONE independently-loaded section of a page: a skeleton while its
  own request is out, its own error (with a retry) if that request failed, and
  the content once there is something to show. A section that reads ANOTHER
  card's request passes `failed` instead, a quiet note in place of the error,
  since that card already shows the failure and its retry.

  Exists because detail pages read their sections separately — the record in one
  request, each heavy block in another — so a page is no longer "loading" or
  "loaded" as a whole. Without this every card re-derives the same three-way
  branch, and the copies drift: one forgets the retry, another shows the
  skeleton over data that is merely revalidating.

  The rules, which are `CachedView`'s own (`sectionState` in
  models/section-state.ts, where they are tested):

  - Skeleton only when there is NOTHING to show. A section that is revalidating
    over data it already has keeps showing that data.
  - An error only when the failure left nothing to show. The retry forces the
    network, since honouring the cache there makes the button look broken.

  The failure (without `failed`) is the shared `ErrorState` at section size,
  handed the view itself so the human sentence ("The server took too long to
  answer") is picked from its status rather than printing axios's raw message.
-->
<script lang="ts" generics="T">
  import type { Snippet } from 'svelte';
  import { t } from '../i18n';
  import Skeleton from './ui/Skeleton.svelte';
  import ErrorState from './ui/ErrorState.svelte';
  import type { CachedView } from '../stores/cached-view.svelte';
  import { sectionState } from '../models/section-state';

  interface Props {
    view: CachedView<T>;
    /** Placeholder lines — describe the shape of what is coming. */
    rows?: number;
    /** Height of each placeholder line; a chart is one tall row. */
    height?: string;
    /**
     * Pad the skeleton and the error. For a section whose content brings its
     * own inset (a full-bleed table) and so sits flush.
     */
    padded?: boolean;
    /**
     * Hold the content back although the view has loaded — for a section that
     * also needs something another request supplies.
     */
    waiting?: boolean;
    /** The failure's title. Defaults to "Couldn't load this section". */
    errorTitle?: string;
    /**
     * Drawn instead of the error and its retry. For a section that reads
     * another card's request: that card already shows the failure and the
     * retry, and a second copy here would only repeat it.
     */
    failed?: Snippet;
    children: Snippet;
  }

  let {
    view,
    rows = 4,
    height,
    padded = false,
    waiting = false,
    errorTitle,
    failed,
    children,
  }: Props = $props();

  const state = $derived(sectionState(view, waiting));
</script>

{#if state === 'content'}
  {@render children()}
{:else if state === 'failed' && failed}
  {@render failed()}
{:else if state === 'failed'}
  <div class="vs-state" class:padded>
    <ErrorState
      size="section"
      title={errorTitle ?? t('section.loadError')}
      error={view}
      onretry={() => view.reload()}
    />
  </div>
{:else}
  <div class="vs-state" class:padded>
    <Skeleton {rows} {height} />
  </div>
{/if}

<style>
  .vs-state.padded {
    padding: 16px;
  }
</style>
