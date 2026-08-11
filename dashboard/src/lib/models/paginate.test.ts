import { describe, expect, it } from 'vitest';
import { pageSlice } from './paginate';

const rows = Array.from({ length: 25 }, (_, i) => i);

describe('pageSlice', () => {
  it('returns the requested window', () => {
    expect(pageSlice(rows, 10, 10).rows).toEqual([10, 11, 12, 13, 14, 15, 16, 17, 18, 19]);
  });

  it('reports a next page when rows remain', () => {
    expect(pageSlice(rows, 0, 10).hasNext).toBe(true);
  });

  it('reports no next page on a short final page', () => {
    expect(pageSlice(rows, 20, 10).rows).toEqual([20, 21, 22, 23, 24]);
    expect(pageSlice(rows, 20, 10).hasNext).toBe(false);
  });

  it('reports no next page when the final page is exactly full', () => {
    // The bug the server-side pager had: a last page of exactly `limit` rows
    // offered a Next that led nowhere. Here the total is known, so there is no
    // excuse for guessing.
    const exact = Array.from({ length: 20 }, (_, i) => i);
    expect(pageSlice(exact, 10, 10).hasNext).toBe(false);
  });

  it('returns an empty page past the end rather than throwing', () => {
    expect(pageSlice(rows, 100, 10)).toEqual({ rows: [], hasNext: false });
  });

  it('handles an empty source', () => {
    expect(pageSlice([], 0, 10)).toEqual({ rows: [], hasNext: false });
  });
});
