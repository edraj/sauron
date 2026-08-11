import { describe, expect, it, vi } from 'vitest';
import {
  MONITOR_CHECK_DEFAULT_SORT,
  MONITOR_INCIDENT_DEFAULT_SORT,
  monitorCheckAccessor,
  monitorIncidentAccessor,
} from './monitor-detail-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { MonitorCheck, MonitorIncident } from './index';

/**
 * Defaults are CONSTANTS, never derived from another field, and any field a
 * test does not distinguish ties across that test's rows — so an accessor that
 * reads a neighbour either collates differently or collapses to input order,
 * and input order is never the expected order.
 *
 * The same rule applies BETWEEN COLUMNS, not just between fields: Duration is
 * computed from Resolved, so holding `started_at` constant would make the two
 * collate identically and nothing could distinguish them. See the Duration
 * test's comment.
 *
 * `MonitorCheck` has no id of its own, so `error` carries the label. It is a
 * free-text column that never sorts, which makes it a safe marker: no accessor
 * reads it, so labelling with it cannot make a wrong accessor look right.
 */
function check(label: string, over: Partial<MonitorCheck> = {}): MonitorCheck {
  return {
    checked_at: '2026-03-01T10:00:00Z',
    up: true,
    response_time_ms: 120,
    status_code: 200,
    error: label,
    ...over,
  };
}

function incident(over: Partial<MonitorIncident> & { id: string }): MonitorIncident {
  return {
    monitor_id: 'mon-1',
    started_at: '2026-03-01T10:00:00Z',
    resolved_at: '2026-03-01T10:05:00Z',
    cause: 'timeout',
    last_error: null,
    ...over,
  };
}

const cOrder = (rows: MonitorCheck[], key: string, dir: SortDir): (string | null)[] =>
  sortRows(rows, monitorCheckAccessor(key), dir).map((c) => c.error);

const iOrder = (rows: MonitorIncident[], key: string, dir: SortDir): string[] =>
  sortRows(rows, monitorIncidentAccessor(key), dir).map((i) => i.id);

describe('monitorCheckAccessor', () => {
  it('orders Latency by milliseconds, not by LatencyBadge text', () => {
    // `LatencyBadge` renders 1200 as "1.2 s" and 980 as "980 ms"; ordering
    // that text descending puts the 980 ms check first. Every other field
    // ties here, so a mis-wired accessor collapses to input order.
    const rows = [
      check('mid', { response_time_ms: 980 }),
      check('slow', { response_time_ms: 1200 }),
      check('fast', { response_time_ms: 9 }),
    ];
    expect(cOrder(rows, 'latency', 'desc')).toEqual(['slow', 'mid', 'fast']);
    expect(cOrder(rows, 'latency', 'asc')).toEqual(['fast', 'mid', 'slow']);
  });

  it('keeps a check with no latency last in both directions', () => {
    // The trap is `?? 0`: a TCP check that never connected has no measurement,
    // and 0 would make it the fastest check on record.
    const rows = [
      check('slow', { response_time_ms: 1200 }),
      check('none', { response_time_ms: null }),
      check('fast', { response_time_ms: 9 }),
    ];
    expect(cOrder(rows, 'latency', 'desc')).toEqual(['slow', 'fast', 'none']);
    expect(cOrder(rows, 'latency', 'asc')).toEqual(['fast', 'slow', 'none']);
  });

  it('keeps a check with no status code last in both directions', () => {
    const rows = [
      check('server-error', { status_code: 500 }),
      check('tcp', { status_code: null }),
      check('ok', { status_code: 200 }),
    ];
    expect(cOrder(rows, 'code', 'desc')).toEqual(['server-error', 'ok', 'tcp']);
    expect(cOrder(rows, 'code', 'asc')).toEqual(['ok', 'server-error', 'tcp']);
  });

  it('orders Time by the raw instant', () => {
    // Latency runs opposite to time here, so the default column cannot
    // produce this order and neither can Result (all three are up).
    const rows = [
      check('oldest', { checked_at: '2026-03-01T08:00:00Z', response_time_ms: 900 }),
      check('newest', { checked_at: '2026-03-01T12:00:00Z', response_time_ms: 10 }),
      check('middle', { checked_at: '2026-03-01T10:00:00Z', response_time_ms: 400 }),
    ];
    expect(cOrder(rows, 'time', 'desc')).toEqual(['newest', 'middle', 'oldest']);
    expect(cOrder(rows, 'time', 'asc')).toEqual(['oldest', 'middle', 'newest']);
  });

  it('orders Result by the boolean, failures first ascending', () => {
    const rows = [
      check('up-1', { up: true }),
      check('down', { up: false }),
      check('up-2', { up: true }),
    ];
    expect(cOrder(rows, 'result', 'asc')).toEqual(['down', 'up-1', 'up-2']);
    expect(cOrder(rows, 'result', 'desc')).toEqual(['up-1', 'up-2', 'down']);
  });

  it('falls back to Time for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Latency order runs opposite to Time order, so a fallback to any other
    // column would show up here.
    const rows = [
      check('older', { checked_at: '2026-03-01T08:00:00Z', response_time_ms: 900 }),
      check('newer', { checked_at: '2026-03-05T08:00:00Z', response_time_ms: 10 }),
    ];
    expect(cOrder(rows, 'no-such-column', 'desc')).toEqual(['newer', 'older']);
    expect(MONITOR_CHECK_DEFAULT_SORT).toEqual({ key: 'time', dir: 'desc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });
});

describe('monitorIncidentAccessor', () => {
  it('orders Duration by elapsed milliseconds, apart from Resolved and from the label', () => {
    // These render "1h 00m", "10m 0s" and "30s"; as text with the shared
    // collator's numeric: true, ascending reads the leading digit runs as
    // 1, 10, 30 and gives hour, ten-min, half-min — exactly backwards.
    //
    // The three incidents START at three different instants ON PURPOSE, and
    // they overlap. Held constant — which is what this fixture used to do —
    // `started_at` makes `durationBetween(started_at, resolved_at)` a strictly
    // monotonic function of `resolved_at`, so Duration and Resolved collate
    // identically and no assertion over these rows can tell the two columns
    // apart. Both directions of that swap survived: `duration` reading
    // `resolved_at` and `resolved` reading the elapsed time each passed the
    // whole file. That is "no field derived from another" — the rule this
    // header states — violated between two COLUMNS rather than between two
    // fixture fields.
    //
    // Staggered, all three time columns disagree, so each accessor in the map
    // produces its own answer:
    //   started  asc → hour, half-min, ten-min
    //   resolved asc → half-min, hour, ten-min
    //   duration asc → half-min, ten-min, hour
    //   cause          the fixture's constant → ties to input order, which is
    //                  none of the four expectations below
    const rows = [
      incident({
        id: 'ten-min',
        started_at: '2026-03-01T10:20:00Z',
        resolved_at: '2026-03-01T10:30:00Z',
      }),
      incident({
        id: 'half-min',
        started_at: '2026-03-01T09:30:00Z',
        resolved_at: '2026-03-01T09:30:30Z',
      }),
      incident({
        id: 'hour',
        started_at: '2026-03-01T09:00:00Z',
        resolved_at: '2026-03-01T10:00:00Z',
      }),
    ];
    expect(iOrder(rows, 'duration', 'desc')).toEqual(['hour', 'ten-min', 'half-min']);
    expect(iOrder(rows, 'duration', 'asc')).toEqual(['half-min', 'ten-min', 'hour']);
    expect(iOrder(rows, 'resolved', 'desc')).toEqual(['ten-min', 'hour', 'half-min']);
    expect(iOrder(rows, 'resolved', 'asc')).toEqual(['half-min', 'hour', 'ten-min']);
  });

  it('keeps an ongoing incident last in both directions for Resolved and Duration', () => {
    // An open incident's cells read "Ongoing" and "—". Ordering it by `now`
    // would rank it as the most recently resolved incident on the page, and
    // ordering it by 0 would call it the shortest outage ever recorded.
    const rows = [
      incident({ id: 'long', resolved_at: '2026-03-01T12:00:00Z' }),
      incident({ id: 'open', resolved_at: null }),
      incident({ id: 'short', resolved_at: '2026-03-01T10:01:00Z' }),
    ];
    expect(iOrder(rows, 'resolved', 'desc')).toEqual(['long', 'short', 'open']);
    expect(iOrder(rows, 'resolved', 'asc')).toEqual(['short', 'long', 'open']);
    expect(iOrder(rows, 'duration', 'desc')).toEqual(['long', 'short', 'open']);
    expect(iOrder(rows, 'duration', 'asc')).toEqual(['short', 'long', 'open']);
  });

  it('orders Started by the raw instant, independently of Duration', () => {
    // The earliest incident is the longest one, so Started order and Duration
    // order disagree and neither accessor can satisfy the other's test.
    const rows = [
      incident({ id: 'newest', started_at: '2026-03-03T10:00:00Z', resolved_at: '2026-03-03T10:00:10Z' }),
      incident({ id: 'oldest', started_at: '2026-03-01T10:00:00Z', resolved_at: '2026-03-01T12:00:00Z' }),
      incident({ id: 'middle', started_at: '2026-03-02T10:00:00Z', resolved_at: '2026-03-02T10:01:00Z' }),
    ];
    expect(iOrder(rows, 'started', 'desc')).toEqual(['newest', 'middle', 'oldest']);
    expect(iOrder(rows, 'started', 'asc')).toEqual(['oldest', 'middle', 'newest']);
  });

  it('orders Cause by the cause, not by the error text beside it or the row id', () => {
    // `last_error` runs opposite to `cause`, and the cell renders both in one
    // td — so an accessor reaching for the wrong one is a real mistake and it
    // dies here.
    //
    // The `a-` / `m-` / `z-` prefixes are load-bearing. Labelled `timeout` /
    // `dns` / `status` after their own causes, the ids collated in cause order
    // and `cause: (i) => i.id` passed both assertions. The prefixes put the id
    // order (a-timeout, m-dns, z-status) outside both expected orders.
    const rows = [
      incident({ id: 'a-timeout', cause: 'timeout', last_error: 'aaa' }),
      incident({ id: 'm-dns', cause: 'dns failure', last_error: 'zzz' }),
      incident({ id: 'z-status', cause: 'status 500', last_error: 'mmm' }),
    ];
    expect(iOrder(rows, 'cause', 'asc')).toEqual(['m-dns', 'z-status', 'a-timeout']);
    expect(iOrder(rows, 'cause', 'desc')).toEqual(['a-timeout', 'z-status', 'm-dns']);
  });

  it('falls back to Started for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const rows = [
      incident({ id: 'older', started_at: '2026-03-01T10:00:00Z', cause: 'zzz' }),
      incident({ id: 'newer', started_at: '2026-03-09T10:00:00Z', cause: 'aaa' }),
    ];
    expect(iOrder(rows, 'nope', 'desc')).toEqual(['newer', 'older']);
    expect(MONITOR_INCIDENT_DEFAULT_SORT).toEqual({ key: 'started', dir: 'desc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('nope');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    cOrder([check('a')], 'latency', 'desc');
    iOrder([incident({ id: 'a' })], 'duration', 'desc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
