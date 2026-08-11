import { describe, expect, it, vi } from 'vitest';
import {
  STORAGE_APP_DEFAULT_SORT,
  STORAGE_TABLE_DEFAULT_SORT,
  storageAppAccessor,
  storageTableAccessor,
} from './storage-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { AppStorage, TableSize } from '../api/admin';

/**
 * Defaults are CONSTANTS, never derived from another field, and any field a
 * test does not distinguish ties across that test's rows — so an accessor that
 * reads a neighbour either collates differently or collapses to input order,
 * and input order is never the expected order. The four numeric app columns in
 * particular hold four distinct defaults, so no one of them can pass another's
 * test.
 */
function table(over: Partial<TableSize> & { name: string }): TableSize {
  return {
    total_bytes: 4096,
    hot_rows: 64,
    ...over,
  };
}

function app(over: Partial<AppStorage> & { app_id: string }): AppStorage {
  return {
    app_name: 'App',
    project_name: 'Project',
    org_name: 'Org',
    tables: [],
    hot_rows_total: 11,
    cold_rows_total: 22,
    cold_bytes_total: 33,
    estimated_hot_bytes_total: 44,
    cold_files: [],
    cold_files_total: 0,
    ...over,
  };
}

const tOrder = (rows: TableSize[], key: string, dir: SortDir): string[] =>
  sortRows(rows, storageTableAccessor(key), dir).map((t) => t.name);

const aOrder = (rows: AppStorage[], key: string, dir: SortDir): string[] =>
  sortRows(rows, storageAppAccessor(key), dir).map((a) => a.app_id);

describe('storageTableAccessor', () => {
  it('orders Size by bytes, not by the formatted string', () => {
    // The whole reason the accessors live outside the component. `fmtBytes`
    // renders these as "1.2 GB", "900.0 KB" and "512 B"; ordering that text
    // descending gives "900.0 KB" first — the kilobytes above the gigabyte,
    // on the one page whose job is saying what is big. `hot_rows` runs
    // OPPOSITE to size here, so the neighbouring numeric column cannot
    // produce this order either.
    const rows = [
      table({ name: 'kb', total_bytes: 921_600, hot_rows: 900 }),
      table({ name: 'gb', total_bytes: 1_288_490_188, hot_rows: 5 }),
      table({ name: 'b', total_bytes: 512, hot_rows: 90_000 }),
    ];
    expect(tOrder(rows, 'size', 'desc')).toEqual(['gb', 'kb', 'b']);
    expect(tOrder(rows, 'size', 'asc')).toEqual(['b', 'kb', 'gb']);
  });

  it('orders Hot rows by the count, not the thousands-separated text', () => {
    // `toLocaleString` renders 1_000_052 as "1,000,052" and 48 as "48"; as
    // text the million sorts first ascending.
    const rows = [
      table({ name: 'mid', hot_rows: 700, total_bytes: 1 }),
      table({ name: 'busy', hot_rows: 1_000_052, total_bytes: 2 }),
      table({ name: 'rare', hot_rows: 48, total_bytes: 3 }),
    ];
    expect(tOrder(rows, 'hot_rows', 'desc')).toEqual(['busy', 'mid', 'rare']);
    expect(tOrder(rows, 'hot_rows', 'asc')).toEqual(['rare', 'mid', 'busy']);
  });

  it('orders Table by name', () => {
    // Size runs opposite to name, so the default column cannot produce this.
    const rows = [
      table({ name: 'analytics_events', total_bytes: 900 }),
      table({ name: 'sessions', total_bytes: 1 }),
      table({ name: 'error_events', total_bytes: 400 }),
    ];
    expect(tOrder(rows, 'table', 'asc')).toEqual([
      'analytics_events',
      'error_events',
      'sessions',
    ]);
    expect(tOrder(rows, 'table', 'desc')).toEqual([
      'sessions',
      'error_events',
      'analytics_events',
    ]);
  });

  it('falls back to Size for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Both wrong fallbacks are excluded, and each needs its own property of
    // this fixture:
    //
    //  - a fallback to TABLE — the row label — because name order runs
    //    opposite to size order, `aaa` being the BIG one;
    //  - a fallback to HOT ROWS because the rows are supplied in the OPPOSITE
    //    order to the expected one. `hot_rows` holds its constant default
    //    across both rows, so it ties and collapses to input order — and
    //    while input order WAS the expected order, this assertion could not
    //    tell Size from Hot rows.
    const rows = [
      table({ name: 'zzz', total_bytes: 10 }),
      table({ name: 'aaa', total_bytes: 900 }),
    ];
    expect(tOrder(rows, 'no-such-column', 'desc')).toEqual(['aaa', 'zzz']);
    expect(STORAGE_TABLE_DEFAULT_SORT).toEqual({ key: 'size', dir: 'desc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });
});

describe('storageAppAccessor', () => {
  it('orders each byte column by ITS OWN bytes', () => {
    // Cold bytes and estimated hot bytes run in OPPOSITE directions, so an
    // accessor pointing at the neighbouring byte column — the likeliest
    // mistake in a map of four near-identical numeric lines — fails. As
    // `fmtBytes` text, descending cold bytes would read "900.0 KB", "512 B",
    // "1.2 GB".
    const rows = [
      app({ app_id: 'kb', cold_bytes_total: 921_600, estimated_hot_bytes_total: 5 }),
      app({ app_id: 'gb', cold_bytes_total: 1_288_490_188, estimated_hot_bytes_total: 1 }),
      app({ app_id: 'b', cold_bytes_total: 512, estimated_hot_bytes_total: 90_000 }),
    ];
    expect(aOrder(rows, 'cold_bytes', 'desc')).toEqual(['gb', 'kb', 'b']);
    expect(aOrder(rows, 'cold_bytes', 'asc')).toEqual(['b', 'kb', 'gb']);
    expect(aOrder(rows, 'hot_bytes', 'desc')).toEqual(['b', 'kb', 'gb']);
  });

  it('orders each row count by ITS OWN field', () => {
    // Hot and cold counts disagree row by row, so neither accessor satisfies
    // the other's expectation.
    const rows = [
      app({ app_id: 'a', hot_rows_total: 5, cold_rows_total: 900 }),
      app({ app_id: 'b', hot_rows_total: 900, cold_rows_total: 5 }),
      app({ app_id: 'c', hot_rows_total: 50, cold_rows_total: 50 }),
    ];
    expect(aOrder(rows, 'hot_rows', 'desc')).toEqual(['b', 'c', 'a']);
    expect(aOrder(rows, 'cold_rows', 'desc')).toEqual(['a', 'c', 'b']);
  });

  it('orders Org, Project and App by their own names', () => {
    // The three names run in three different directions, so no one of them
    // can stand in for another.
    const rows = [
      app({ app_id: 'x', org_name: 'Acme', project_name: 'Zeta', app_name: 'Mid' }),
      app({ app_id: 'y', org_name: 'Zenith', project_name: 'Alpha', app_name: 'Aaa' }),
      app({ app_id: 'z', org_name: 'Middle', project_name: 'Mu', app_name: 'Zzz' }),
    ];
    expect(aOrder(rows, 'org', 'asc')).toEqual(['x', 'z', 'y']);
    expect(aOrder(rows, 'project', 'asc')).toEqual(['y', 'z', 'x']);
    expect(aOrder(rows, 'app', 'asc')).toEqual(['y', 'x', 'z']);
  });

  it('keeps an app with no project name last in both directions', () => {
    // `project_name` is an empty string — not null — for a report cached by a
    // build that predates the field, and the cell renders an em dash. `?? ''`
    // would leave it collating first ascending, as though its project were
    // named "".
    const rows = [
      app({ app_id: 'zed', project_name: 'Zeta' }),
      app({ app_id: 'blank', project_name: '' }),
      app({ app_id: 'alpha', project_name: 'Alpha' }),
    ];
    expect(aOrder(rows, 'project', 'asc')).toEqual(['alpha', 'zed', 'blank']);
    expect(aOrder(rows, 'project', 'desc')).toEqual(['zed', 'alpha', 'blank']);
  });

  it('reproduces the endpoint order exactly when seeded at its default', () => {
    // The claim `STORAGE_APP_DEFAULT_SORT` makes, checked rather than
    // asserted: the response arrives ordered by (org, project, app), and
    // `sortRows` is stable, so re-ordering it by org alone must leave the
    // project and app sub-orders untouched. If stability ever regressed, the
    // page would silently open in a different order than it does today and
    // only this would say so.
    const asServed = [
      app({ app_id: '1', org_name: 'Acme', project_name: 'Alpha', app_name: 'Aaa' }),
      app({ app_id: '2', org_name: 'Acme', project_name: 'Alpha', app_name: 'Bbb' }),
      app({ app_id: '3', org_name: 'Acme', project_name: 'Beta', app_name: 'Aaa' }),
      app({ app_id: '4', org_name: 'Zenith', project_name: 'Alpha', app_name: 'Aaa' }),
    ];
    expect(aOrder(asServed, STORAGE_APP_DEFAULT_SORT.key, STORAGE_APP_DEFAULT_SORT.dir)).toEqual([
      '1',
      '2',
      '3',
      '4',
    ]);
    expect(STORAGE_APP_DEFAULT_SORT).toEqual({ key: 'org', dir: 'asc' });
  });

  it('falls back to Org for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Cold bytes run opposite to org name, so a fallback to any other column
    // would show up here.
    const rows = [
      app({ app_id: 'zzz', org_name: 'Zenith', cold_bytes_total: 1 }),
      app({ app_id: 'aaa', org_name: 'Acme', cold_bytes_total: 900 }),
    ];
    expect(aOrder(rows, 'nope', 'asc')).toEqual(['aaa', 'zzz']);
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('nope');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    tOrder([table({ name: 'a' })], 'size', 'desc');
    aOrder([app({ app_id: 'a' })], 'cold_bytes', 'desc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
