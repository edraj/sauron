<script lang="ts">
  import Icon from './ui/Icon.svelte';

  interface Props {
    /**
     * Rows matching the query, from `SearchEnvelope.total` — or `null` when
     * there is no envelope on screen to read it from. Read together with
     * `totalIsCapped`, never alone.
     *
     * `null` is the window during a page MOVE: the request for the new page is
     * in flight, and the count that arrived with the previous page describes
     * rows that are no longer rendered. Both a stale number and a zeroed one
     * would be a count this control cannot stand behind, so it states none and
     * the page number carries the label alone.
     */
    total: number | null;
    /**
     * The server stopped counting at its `COUNT_CAP`, so `total` means "at
     * least this many". Rendered as a `+`.
     *
     * This is why the API returns a number and a boolean instead of the string
     * `"1204+"`: the suffix is a display concern, and baking it into the
     * payload would force every other caller to parse the number back out.
     */
    totalIsCapped: boolean;
    /** 1-based page, from `pageNumber()`. */
    page: number;
    canPrev: boolean;
    canNext: boolean;
    /**
     * A load is in flight — keep the control in place, but refuse clicks.
     *
     * Pass the WHOLE in-flight window, page moves included, not just background
     * revalidates. A pager that unmounts while the next page loads and reappears
     * when it lands is a control that jumps out from under the cursor, and it
     * only does it in one direction (a cached Prev repaints instantly), so it
     * reads as a control that breaks at random. It stays mounted and goes dead
     * instead — with `total` null for the duration, so it cannot announce a
     * count it does not have.
     */
    busy?: boolean;
    /** Singular noun for the count; pluralised by appending `s`. */
    noun?: string;
    onprev: () => void;
    onnext: () => void;
  }

  let {
    total,
    totalIsCapped,
    page,
    canPrev,
    canNext,
    busy = false,
    noun = 'result',
    onprev,
    onnext,
  }: Props = $props();

  const plural = $derived(`${noun}s`);

  // A capped total can never be 0 or 1 (the cap is 10,000), but the guard is
  // written out anyway: "1+ result" would be wrong in a way that reads as a
  // rounding bug rather than as a deliberate lower bound.
  //
  // `null` in, `null` out: no count is known, so none is stated. Note this is
  // NOT the same as `total === 0`, which is a count the server did give.
  const countText = $derived(
    total === null
      ? null
      : total === 0 && !totalIsCapped
        ? `No ${plural}`
        : `${total.toLocaleString()}${totalIsCapped ? '+' : ''} ${
            total === 1 && !totalIsCapped ? noun : plural
          }`,
  );
</script>

<div class="pager">
  <span class="range muted">{countText ?? ''}</span>
  <div class="btns">
    <button class="pg" disabled={!canPrev || busy} onclick={onprev} type="button">
      <Icon name="chevron-left" size={14} /> Prev
    </button>
    
    {#if page > 1}
      <button class="pg num" disabled={busy} onclick={onprev} type="button">{page - 1}</button>
    {/if}

    <button class="pg num active" type="button">{page}</button>

    {#if canNext}
      <button class="pg num" disabled={busy} onclick={onnext} type="button">{page + 1}</button>
    {/if}

    <button class="pg" disabled={!canNext || busy} onclick={onnext} type="button">
      Next <Icon name="chevron-right" size={14} />
    </button>
  </div>
</div>

<style>
  .pager {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 2px 0;
  }
  .range {
    font-size: 12.5px;
    font-variant-numeric: tabular-nums;
  }
  .btns {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .pg {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 6px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 550;
    transition: color 0.12s ease, border-color 0.12s ease;
  }
  .pg.num {
    padding: 6px 10px;
    min-width: 32px;
    justify-content: center;
  }
  .pg:hover:not(:disabled, .active) {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .pg.active {
    background: var(--surface-3);
    color: var(--text);
    border-color: var(--border-strong);
    cursor: default;
  }
  .pg:disabled {
    opacity: 0.4;
    cursor: default;
  }
</style>
