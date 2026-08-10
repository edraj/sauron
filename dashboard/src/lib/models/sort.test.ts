import { describe, expect, it } from 'vitest';
import { sortParam, toggleSort, type SortState } from './sort';

describe('sortParam', () => {
  // The backend's `parse_sort` reads a BARE name as descending and a `-`
  // prefix as ascending. Getting this backwards produces a list ordered the
  // wrong way with no error anywhere, so it is pinned here.
  it('writes a descending sort as the bare column name', () => {
    expect(sortParam({ key: 'last_seen', dir: 'desc' })).toBe('last_seen');
  });

  it('writes an ascending sort with a leading dash', () => {
    expect(sortParam({ key: 'last_seen', dir: 'asc' })).toBe('-last_seen');
  });
});

describe('toggleSort', () => {
  const current: SortState = { key: 'last_seen', dir: 'desc' };

  it('selects a new column at that column default direction', () => {
    expect(toggleSort(current, 'times_seen', 'desc')).toEqual({
      key: 'times_seen',
      dir: 'desc',
    });
    expect(toggleSort(current, 'title', 'asc')).toEqual({ key: 'title', dir: 'asc' });
  });

  it('flips direction when the active column is clicked again', () => {
    expect(toggleSort(current, 'last_seen', 'desc')).toEqual({
      key: 'last_seen',
      dir: 'asc',
    });
  });

  it('flips back on a third click rather than clearing the sort', () => {
    const once = toggleSort(current, 'last_seen', 'desc');
    expect(toggleSort(once, 'last_seen', 'desc')).toEqual({
      key: 'last_seen',
      dir: 'desc',
    });
  });

  // The active column FLIPS; it does not re-apply the column default. These
  // coincide whenever the default matches the current direction, so the case
  // is chosen to make them disagree: default `asc` on a column already `asc`
  // must give `desc`. An implementation that returns the default for every
  // click passes every other test in this file and fails this one.
  it('flips the active column even when the default says otherwise', () => {
    expect(toggleSort({ key: 'title', dir: 'asc' }, 'title', 'asc')).toEqual({
      key: 'title',
      dir: 'desc',
    });
  });
});
