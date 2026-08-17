import type { CountEnvelope } from '../api/counts';

/**
 * The total row count behind an offset-paged list, fetched independently of
 * the page.
 *
 * ## Keyed on the PREDICATE, never on the page
 *
 * A total is a property of the query, not of the slice being shown, so paging
 * must not refetch it. Keying on the predicate alone gives that for free: click
 * through twenty pages and the count endpoint is hit once. It is also what
 * keeps the page strip from flickering as you page — the number the strip's
 * width derives from simply does not change.
 *
 * Getting this wrong in the other direction is the CachedView moving-key trap:
 * fold the offset into the key and every page mints a fresh entry, so the
 * request fires on every click while looking perfectly cached.
 *
 * ## Failure is silence, not an error
 *
 * A count that does not arrive leaves `total` at `null`, and the pager falls
 * back to Prev/Next — which is exactly what these lists offered before this
 * existed. There is deliberately no error surface: the rows are on screen and
 * correct, and a red banner over a working table because a *caption* failed
 * would be worse than the missing number.
 */
export class RowCount {
  /** Rows matching the predicate, or `null` when no count has landed. */
  total = $state<number | null>(null);
  /** The server stopped at its cap, so `total` means "at least this many". */
  isCapped = $state(false);

  /**
   * The predicate this count describes.
   *
   * Doubles as the staleness guard: a response is only accepted while the key
   * that requested it is still current, so a fast count for an abandoned
   * predicate cannot overwrite a slow one for the live predicate.
   */
  #key: string | null = null;

  /**
   * Fetch the count for `key`, unless it is already the one on hand.
   *
   * `force` re-fetches the current key — for an explicit Refresh click, where
   * the point is to reach the network regardless of freshness.
   */
  async load(key: string, fetcher: () => Promise<CountEnvelope>, force = false): Promise<void> {
    if (key === this.#key && !force) return;
    this.#key = key;
    // `total` is deliberately NOT cleared first. The strip derives its width
    // from the page count, and blanking it mid-flight would collapse the strip
    // and spring it back — the width jitter the constant slot count exists to
    // prevent, reintroduced two layers up. A count from the previous predicate
    // is a better width for one beat than no count at all.
    try {
      const envelope = await fetcher();
      if (this.#key !== key) return;
      this.total = envelope.total;
      this.isCapped = envelope.total_is_capped;
    } catch {
      if (this.#key !== key) return;
      // No count is a supported state; see the class doc.
      this.total = null;
      this.isCapped = false;
    }
  }
}
