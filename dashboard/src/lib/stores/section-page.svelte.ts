import { errorMessage } from '../api/client';
import type { ListPage } from '../models/list-state';

/**
 * One fetch-on-demand, offset-paged section: the rows plus the flags a
 * collapsible card needs to render them.
 *
 * Extracted out of `CollapsibleFetchCard.svelte` so the parts that are easy to
 * get wrong — the out-of-order guard and which page a retry targets — can be
 * tested directly. The repo has no component-test harness (all 1000+ tests are
 * plain logic), and adding one to reach this logic would be the wrong trade;
 * `CachedView` sets the precedent for a `$state` class in a `.svelte.ts` with
 * its own spec file, so this follows it.
 *
 * The card keeps only what is genuinely presentational: collapsed or not.
 *
 * ## Two offsets, not one
 *
 * `offset` is the page ON SCREEN. `requestedOffset` is the page most recently
 * ASKED FOR, which differs precisely while a load is in flight or after one
 * failed. Retry has to use the latter: with a single offset, pressing Next and
 * having it fail leaves `offset` on the page you were already reading, so
 * "Try again" re-fetches that page, succeeds, and the card looks recovered
 * while never having gone anywhere. The user presses Next again and the same
 * thing happens.
 *
 * ## Out-of-order responses
 *
 * Prev/Next stay clickable while a request is in flight, so two can overlap and
 * nothing about HTTP makes them return in order. Each call claims a generation
 * and only the newest may write, so a slow response for a page the user has
 * already moved off cannot overwrite the current one — nor can its error, nor
 * can its `loading = false` clear the spinner belonging to a newer request.
 */
export class SectionPage<T> {
  /**
   * `$state.raw`: rows are replaced wholesale on every page and never edited in
   * place, so the deep proxy buys nothing and costs on a page of 25 events with
   * arbitrarily nested `properties` blobs. It also keeps `===` meaningful on a
   * row, which the proxy would quietly break.
   */
  rows = $state.raw<T[]>([]);
  loading = $state(false);
  error = $state<string | null>(null);
  hasNext = $state(false);
  /** Offset of the page currently rendered. */
  offset = $state(0);
  /** True once a fetch has SUCCEEDED — drives "Fetch" vs. the list. */
  loaded = $state(false);

  #gen = 0;
  #requested = 0;

  /**
   * Offset of the most recent attempt, successful or not. Equals {@link offset}
   * except while a load is in flight or after a failed one — see the class doc.
   */
  get requestedOffset(): number {
    return this.#requested;
  }

  /** Load the page at `next`. Only the newest call may write. */
  async load(next: number, fetcher: (offset: number) => Promise<ListPage<T>>): Promise<void> {
    const gen = ++this.#gen;
    this.#requested = next;
    this.loading = true;
    this.error = null;
    try {
      const page = await fetcher(next);
      if (gen !== this.#gen) return;
      this.rows = page.rows;
      this.hasNext = page.hasNext;
      this.offset = next;
      this.loaded = true;
    } catch (err) {
      if (gen !== this.#gen) return;
      this.error = errorMessage(err);
    } finally {
      // Guarded like the writes above: an overtaken request clearing this would
      // stop the spinner belonging to the request that overtook it.
      if (gen === this.#gen) this.loading = false;
    }
  }

  /**
   * Re-attempt the page the last call asked for — NOT the one on screen.
   *
   * This is the whole reason `requestedOffset` exists; see the class doc.
   */
  retry(fetcher: (offset: number) => Promise<ListPage<T>>): Promise<void> {
    return this.load(this.#requested, fetcher);
  }

  /** Refresh the page currently on screen. */
  refresh(fetcher: (offset: number) => Promise<ListPage<T>>): Promise<void> {
    return this.load(this.offset, fetcher);
  }

  /**
   * Rows walked through so far, for the header badge. `+` when more follow.
   *
   * Reads off the rendered page rather than a total, because these endpoints
   * have no count route — see `screen-sections.ts`.
   */
  get seen(): number {
    return this.offset + this.rows.length;
  }
}
