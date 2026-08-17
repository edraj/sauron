/**
 * Which page numbers a pager renders, and where the gaps fall.
 *
 * Split out of the component and kept pure so the arithmetic can be tested
 * without a DOM. A windowing bug is an off-by-one that renders perfectly — the
 * strip still paints seven buttons and every one of them still navigates — so
 * a rendering test waves it through. The invariants worth defending (constant
 * width, no gap standing in for a single page, the current page always present)
 * are properties of the array, and that is where they are asserted.
 */

/** A gap in the strip: at least two pages the window does not list. */
export type PageSlot = number | 'gap';

/**
 * How many slots a long list always produces.
 *
 * `2 * siblings` neighbours, the current page, the first and last page, and the
 * two gaps between them.
 */
export function slotCount(siblings = 1): number {
  return 2 * siblings + 5;
}

/**
 * The slots for a pager showing `totalPages` pages, centred on `page`.
 *
 * Always returns exactly `slotCount(siblings)` entries once `totalPages`
 * exceeds that, and every page otherwise. A strip that gained and lost slots as
 * you paged would move the controls under the cursor at the two boundaries
 * where a gap appears or collapses, which reads as a control that breaks at
 * random rather than one that resizes.
 *
 * **A constant slot count is not by itself a constant width**, and the
 * difference was measured rather than reasoned about: 275.7px on page 1 of a
 * 200-page list against 332.95px on page 200, because a 1-digit button sits on
 * its width floor while a 3-digit one grows past it. Seven slots either way, so
 * nothing here can see it. `PageStrip` closes that half by sizing every slot to
 * the widest page number; this function's job is only the slot count.
 *
 * A gap is only ever emitted for two or more pages. The obvious boundary test
 * (`leftSibling > 2`) lets a gap stand in for exactly page 2 — the same width
 * as the button it replaced, with nowhere to go — so the thresholds are one
 * wider on each side and the block form is used instead.
 */
export function pageWindow(page: number, totalPages: number, siblings = 1): PageSlot[] {
  if (totalPages <= 0) return [];

  const slots = slotCount(siblings);
  // Callers derive `totalPages` as `max(ceil(total / limit), page)`, so an
  // out-of-range page cannot reach here through them. Clamped anyway: a strip
  // that renders a page number past its own last slot is a control that states
  // a destination it does not have.
  const current = Math.min(Math.max(Math.round(page) || 1, 1), totalPages);

  if (totalPages <= slots) return range(1, totalPages);

  const leftSibling = Math.max(current - siblings, 1);
  const rightSibling = Math.min(current + siblings, totalPages);

  // `> 3` and `< totalPages - 2`, not `> 2` / `< totalPages - 1`: at the looser
  // threshold the gap hides a single page. See the doc comment.
  const leftGap = leftSibling > 3;
  const rightGap = rightSibling < totalPages - 2;

  // The block that replaces a suppressed gap absorbs its slot, so all three
  // shapes below come out the same width.
  const blockLen = slots - 2;

  if (!leftGap && rightGap) return [...range(1, blockLen), 'gap', totalPages];
  if (leftGap && !rightGap) return [1, 'gap', ...range(totalPages - blockLen + 1, totalPages)];
  if (leftGap && rightGap) {
    return [1, 'gap', ...range(leftSibling, rightSibling), 'gap', totalPages];
  }

  // Both gaps suppressed means the window reaches both ends, which implies
  // `totalPages <= slots` and was returned above. Unreachable; listing every
  // page is the answer that stays correct if the thresholds are ever widened.
  return range(1, totalPages);
}

function range(from: number, to: number): number[] {
  return Array.from({ length: Math.max(0, to - from + 1) }, (_, i) => from + i);
}
