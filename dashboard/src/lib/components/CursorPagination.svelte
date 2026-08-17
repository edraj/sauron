<script lang="ts">
  import PageStrip from './PageStrip.svelte';

  /**
   * The keyset adapter over {@link PageStrip}.
   *
   * Emits a page NUMBER and leaves the caller to decide how to reach it — a
   * keyset step for ±1, an offset jump for anything else. That decision needs
   * the caller's `next_cursor` and its `CursorPage`, neither of which belongs
   * in a pager.
   */
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
    /** Rows per page — turns a total into a page count. */
    limit: number;
    canNext: boolean;
    /** A load is in flight. See `PageStrip`'s `busy`. */
    busy?: boolean;
    /** Singular noun for the count; pluralised by appending `s`. */
    noun?: string;
    /** Called with the 1-based page to move to. */
    onjump: (page: number) => void;
  }

  let {
    total,
    totalIsCapped,
    page,
    limit,
    canNext,
    busy = false,
    noun = 'result',
    onjump,
  }: Props = $props();

  const plural = $derived(`${noun}s`);

  /**
   * The last page count the server actually stated.
   *
   * `total` is null for the whole duration of every page move, and a strip
   * derived straight from it would collapse to one or two slots and spring
   * back — the exact width jitter the constant slot count exists to prevent,
   * reintroduced one layer up. The count caption still goes blank while the
   * move is in flight, because that is a number this control cannot stand
   * behind; the page COUNT is a property of the query, not of the page being
   * fetched, so holding it across the move states nothing false.
   */
  let lastKnownPages = $state(1);
  $effect(() => {
    if (total !== null) lastKnownPages = Math.max(Math.ceil(total / limit), 1);
  });

  const totalPages = $derived(
    Math.max(total !== null ? Math.ceil(total / limit) : lastKnownPages, page),
  );

  // A capped total can never be 0 or 1 (the cap is 10,000), but the guard is
  // written out anyway: "1+ result" would be wrong in a way that reads as a
  // rounding bug rather than as a deliberate lower bound.
  //
  // `null` in, `null` out: no count is known, so none is stated. Note this is
  // NOT the same as `total === 0`, which is a count the server did give.
  const label = $derived(
    total === null
      ? null
      : total === 0 && !totalIsCapped
        ? `No ${plural}`
        : `${total.toLocaleString()}${totalIsCapped ? '+' : ''} ${
            total === 1 && !totalIsCapped ? noun : plural
          }`,
  );
</script>

<PageStrip {page} {totalPages} {canNext} {label} {busy} {onjump} />
