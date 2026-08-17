import { describe, expect, it } from 'vitest';
import { pageWindow, slotCount } from './page-window';

describe('pageWindow', () => {
  it('renders every page when they all fit', () => {
    expect(pageWindow(3, 6)).toEqual([1, 2, 3, 4, 5, 6]);
    expect(pageWindow(1, 7)).toEqual([1, 2, 3, 4, 5, 6, 7]);
  });

  // The three shapes a long list takes. Each keeps the first and last page
  // reachable in one click, which is the whole reason for the gaps.
  it('anchors to the start', () => {
    expect(pageWindow(1, 200)).toEqual([1, 2, 3, 4, 5, 'gap', 200]);
    expect(pageWindow(3, 200)).toEqual([1, 2, 3, 4, 5, 'gap', 200]);
  });

  it('centres on the current page in the middle', () => {
    expect(pageWindow(5, 200)).toEqual([1, 'gap', 4, 5, 6, 'gap', 200]);
    expect(pageWindow(100, 200)).toEqual([1, 'gap', 99, 100, 101, 'gap', 200]);
  });

  it('anchors to the end', () => {
    expect(pageWindow(198, 200)).toEqual([1, 'gap', 196, 197, 198, 199, 200]);
    expect(pageWindow(200, 200)).toEqual([1, 'gap', 196, 197, 198, 199, 200]);
  });

  /**
   * The invariant the layout depends on.
   *
   * A strip that changes width as you page moves the Next button out from
   * under the cursor, and it only does it at the two boundaries where a gap
   * appears or collapses — so it reads as a control that breaks at random
   * rather than as a control that resizes. Every page of a long list must
   * therefore produce the same number of slots.
   */
  it('emits a constant slot count for a fixed total', () => {
    for (const total of [8, 9, 20, 200, 5000]) {
      const widths = new Set(
        Array.from({ length: total }, (_, i) => pageWindow(i + 1, total).length),
      );
      expect(widths, `total=${total}`).toEqual(new Set([slotCount()]));
    }
  });

  // The boundary between "all pages fit" and "gaps appear". At exactly
  // `slotCount()` every page is listed; one more and the strip switches shape
  // without changing width.
  it('switches to gaps exactly one page past the slot count', () => {
    expect(pageWindow(1, slotCount())).toHaveLength(slotCount());
    expect(pageWindow(1, slotCount())).not.toContain('gap');
    expect(pageWindow(1, slotCount() + 1)).toHaveLength(slotCount());
    expect(pageWindow(1, slotCount() + 1)).toContain('gap');
  });

  it('never emits two gaps in a row, or a gap hiding a single page', () => {
    // A gap standing in for exactly one page is worse than the page: same
    // width, no destination. Check every position of a long list.
    for (let page = 1; page <= 200; page++) {
      const w = pageWindow(page, 200);
      for (let i = 1; i < w.length; i++) {
        expect(w[i] === 'gap' && w[i - 1] === 'gap').toBe(false);
        if (w[i] === 'gap') {
          const before = w[i - 1] as number;
          const after = w[i + 1] as number;
          expect(after - before, `page=${page} slot=${i}`).toBeGreaterThan(2);
        }
      }
    }
  });

  it('is strictly ascending', () => {
    for (let page = 1; page <= 200; page++) {
      const nums = pageWindow(page, 200).filter((s): s is number => s !== 'gap');
      expect([...nums].sort((a, b) => a - b)).toEqual(nums);
      expect(new Set(nums).size).toBe(nums.length);
    }
  });

  it('always contains the current page', () => {
    for (let page = 1; page <= 200; page++) {
      expect(pageWindow(page, 200), `page=${page}`).toContain(page);
    }
  });

  // Degenerate inputs reach here from `Math.ceil(total / limit)` on an empty
  // or not-yet-loaded list. None of them should throw, and none should render
  // a page number that cannot be clicked.
  it('handles empty and single-page lists', () => {
    expect(pageWindow(1, 0)).toEqual([]);
    expect(pageWindow(1, -3)).toEqual([]);
    expect(pageWindow(1, 1)).toEqual([1]);
  });

  it('clamps a current page outside the range instead of emitting it', () => {
    // Callers derive `totalPages` as `max(ceil(total / limit), page)`, so this
    // is unreachable through them — but a strip that renders a page number
    // past its own last slot is a confidently-wrong control, so it is refused
    // here rather than trusted upstream.
    expect(pageWindow(50, 3)).toEqual([1, 2, 3]);
    expect(pageWindow(0, 10)).toContain(1);
  });

  it('honours a wider sibling count', () => {
    expect(pageWindow(100, 200, 2)).toEqual([1, 'gap', 98, 99, 100, 101, 102, 'gap', 200]);
    expect(pageWindow(100, 200, 2)).toHaveLength(slotCount(2));
  });
});
