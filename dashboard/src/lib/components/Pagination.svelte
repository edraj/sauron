<script lang="ts">
  import { t, formatNumber } from '../i18n';
  import PageStrip from './PageStrip.svelte';

  /**
   * The offset adapter over {@link PageStrip}.
   *
   * Turns a page number into the byte offset its caller pages with, and a row
   * count into the caption. All rendering lives in `PageStrip`.
   */
  interface Props {
    offset: number;
    limit: number;
    /** Number of rows on the current page. */
    count: number;
    /**
     * Whether a page exists after this one.
     *
     * Supplied by the caller rather than inferred from `count >= limit`, which
     * was wrong: a final page holding exactly `limit` rows offered an enabled
     * Next that led to an empty page. The caller knows the answer — from a
     * total, or by requesting `limit + 1` rows and rendering `limit`.
     */
    hasNext: boolean;
    /**
     * Rows matching the query across every page, when the caller knows it.
     *
     * `null` means the caller pages by a `limit + 1` over-fetch probe and has
     * no total to offer, so the strip can only show as far as it has walked.
     * Everything with a count endpoint or an in-memory list passes a number.
     */
    total?: number | null;
    /** The server stopped counting at its cap, so `total` means "at least this many". */
    totalIsCapped?: boolean;
    onchange: (offset: number) => void;
  }

  let {
    offset,
    limit,
    count,
    hasNext,
    total = null,
    totalIsCapped = false,
    onchange,
  }: Props = $props();

  const page = $derived(Math.floor(offset / limit) + 1);
  const from = $derived(count === 0 ? 0 : offset + 1);
  const to = $derived(offset + count);

  // Never below `page`: a strip whose last slot precedes the page it is
  // highlighting states a range it does not have. Without a total the best
  // available answer is "as far as we have walked, plus one if there is more".
  const totalPages = $derived(
    Math.max(total !== null ? Math.ceil(total / limit) : page + (hasNext ? 1 : 0), page),
  );

  const label = $derived.by(() => {
    if (count === 0) return offset === 0 ? t('common.noResults') : t('ui.pager.endOfResults');
    const range = `${formatNumber(from)}–${formatNumber(to)}`;
    if (total === null) return range;
    return t('ui.pager.range', {
      range,
      total: `${formatNumber(total)}${totalIsCapped ? '+' : ''}`,
    });
  });
</script>

<PageStrip
  {page}
  {totalPages}
  canNext={hasNext}
  {label}
  onjump={(p) => onchange((p - 1) * limit)}
/>
