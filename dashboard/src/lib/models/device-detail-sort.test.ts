import { describe, expect, it, vi } from 'vitest';
import {
  DEVICE_PERF_DEFAULT_SORT,
  DEVICE_SESSION_DEFAULT_SORT,
  devicePerfAccessor,
  deviceSessionAccessor,
} from './device-detail-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { PerfSummaryRow, Session } from './index';

/**
 * Defaults are CONSTANTS, never derived from another field, and any field a
 * test does not distinguish ties across that test's rows — so an accessor that
 * reads a neighbour either collates differently or collapses to input order,
 * and input order is never the expected order.
 *
 * One rule beyond that, because it is the subtler half and the Session column
 * got it wrong first time: **the label a row is identified by is itself a
 * plausible accessor target.** `sOrder()` maps rows back to `s.id`, and `id`
 * is a real `Session` field — the primary key — so `session: (s) => s.id`
 * instead of `s.session_id` is a mis-wiring a reader would never spot. The
 * first version labelled the rows `a`/`m`/`z` beside session ids
 * `sess-aaa`/`sess-mmm`/`sess-zzz`, which collate in the same order, so the
 * test could not tell the right accessor from that wrong one.
 *
 * The Session test therefore uses opaque ids anti-correlated with
 * `session_id`. Every other test here is already safe by construction and the
 * mutation runs in the task report check each one.
 */
function sess(over: Partial<Session> & { id: string }): Session {
  return {
    app_id: 'app',
    session_id: 'sess-mmm',
    distinct_id: null,
    device_key: 'dev-1',
    started_at: '2026-03-01T10:00:00Z',
    last_event_at: '2026-03-01T10:05:00Z',
    events_count: 7,
    errors_count: 2,
    context: null,
    release: null,
    environment_id: null,
    ip_address: null,
    created_at: '2026-03-01T10:00:00Z',
    updated_at: '2026-03-01T10:05:00Z',
    ...over,
  };
}

function perf(over: Partial<PerfSummaryRow> & { name: string }): PerfSummaryRow {
  return {
    op: 'http',
    count: 40,
    p50: 100,
    p75: 150,
    p95: 200,
    p99: 300,
    avg: 120,
    error_rate: 0.01,
    ...over,
  };
}

const sOrder = (rows: Session[], key: string, dir: SortDir): string[] =>
  sortRows(rows, deviceSessionAccessor(key), dir).map((s) => s.id);

const pOrder = (rows: PerfSummaryRow[], key: string, dir: SortDir): string[] =>
  sortRows(rows, devicePerfAccessor(key), dir).map((p) => p.name);

describe('deviceSessionAccessor', () => {
  it('orders Duration by elapsed milliseconds, not by the formatted label', () => {
    // The whole reason the accessors live outside the component. These three
    // spans render as "1h 00m", "10m 0s" and "30s"; the shared collator runs
    // with numeric: true, so as TEXT ascending they come out 1h, 10m, 30s —
    // exactly backwards. Milliseconds give the real answer.
    //
    // Neither endpoint of the span can stand in for it either: `started_at`
    // descending and `last_event_at` descending both give hour, half-minute,
    // ten-minute, which is not the expected order below.
    const rows = [
      sess({ id: 'hour', started_at: '2026-03-01T10:00:00Z', last_event_at: '2026-03-01T11:00:00Z' }),
      sess({ id: 'half-min', started_at: '2026-03-01T09:00:00Z', last_event_at: '2026-03-01T09:00:30Z' }),
      sess({ id: 'ten-min', started_at: '2026-03-01T08:00:00Z', last_event_at: '2026-03-01T08:10:00Z' }),
    ];
    expect(sOrder(rows, 'duration', 'desc')).toEqual(['hour', 'ten-min', 'half-min']);
    expect(sOrder(rows, 'duration', 'asc')).toEqual(['half-min', 'ten-min', 'hour']);
  });

  it('orders Started by the raw instant, independently of Duration', () => {
    // Started order and Duration order disagree on purpose: the earliest
    // session is the longest one, so an accessor reading the span cannot
    // satisfy this and an accessor reading the instant cannot satisfy the
    // test above.
    const rows = [
      sess({ id: 'newest', started_at: '2026-03-03T10:00:00Z', last_event_at: '2026-03-03T10:00:10Z' }),
      sess({ id: 'oldest', started_at: '2026-03-01T10:00:00Z', last_event_at: '2026-03-01T12:00:00Z' }),
      sess({ id: 'middle', started_at: '2026-03-02T10:00:00Z', last_event_at: '2026-03-02T10:01:00Z' }),
    ];
    expect(sOrder(rows, 'started', 'desc')).toEqual(['newest', 'middle', 'oldest']);
    expect(sOrder(rows, 'started', 'asc')).toEqual(['oldest', 'middle', 'newest']);
  });

  it('orders Session by the session id the cell renders, not the row key', () => {
    // Every other field ties, so an accessor reading one collapses to input
    // order — which is not the expected order either way.
    //
    // And the labels are opaque and anti-correlated with `session_id` (see the
    // file header): id order is k1, k2, k3, which is neither expected order
    // below, so `session: (s) => s.id` fails here. `id` is the row's primary
    // key and `session_id` is the client-supplied identifier the cell links
    // on — two real fields, one of them wrong.
    const rows = [
      sess({ id: 'k2', session_id: 'sess-zzz' }),
      sess({ id: 'k3', session_id: 'sess-aaa' }),
      sess({ id: 'k1', session_id: 'sess-mmm' }),
    ];
    expect(sOrder(rows, 'session', 'asc')).toEqual(['k3', 'k1', 'k2']);
    expect(sOrder(rows, 'session', 'desc')).toEqual(['k2', 'k1', 'k3']);
  });

  it('orders Events and Errors by their own counts', () => {
    // The two counts run in OPPOSITE directions across these rows, so neither
    // column can be satisfied by the other's accessor.
    const rows = [
      sess({ id: 'chatty', events_count: 900, errors_count: 0 }),
      sess({ id: 'broken', events_count: 12, errors_count: 41 }),
      sess({ id: 'mid', events_count: 120, errors_count: 3 }),
    ];
    expect(sOrder(rows, 'events', 'desc')).toEqual(['chatty', 'mid', 'broken']);
    expect(sOrder(rows, 'errors', 'desc')).toEqual(['broken', 'mid', 'chatty']);
    expect(sOrder(rows, 'errors', 'asc')).toEqual(['chatty', 'mid', 'broken']);
  });

  it('falls back to Started for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Session-id order runs opposite to Started order here, so a fallback to
    // any other column would show up.
    const rows = [
      sess({ id: 'older', started_at: '2026-03-01T10:00:00Z', session_id: 'sess-zzz' }),
      sess({ id: 'newer', started_at: '2026-03-09T10:00:00Z', session_id: 'sess-aaa' }),
    ];
    expect(sOrder(rows, 'no-such-column', 'desc')).toEqual(['newer', 'older']);
    expect(DEVICE_SESSION_DEFAULT_SORT).toEqual({ key: 'started', dir: 'desc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });
});

describe('devicePerfAccessor', () => {
  it('orders p95 by milliseconds, not by LatencyBadge text', () => {
    // `LatencyBadge` renders 1200 as "1.2 s" and 980 as "980 ms"; ordering
    // that text descending puts the 980 ms row first. Count ties across all
    // three, so the default column cannot produce this order either.
    //
    // The labels are anti-correlated with the latencies, because `name` is
    // the row label here AND a real accessor target: labelled
    // `fast`/`mid`/`slow` these rows collated in exactly p95 order, so
    // `p95: (p) => p.name` passed both assertions.
    const rows = [
      perf({ name: 'q-fast', p95: 12 }),
      perf({ name: 'a-slow', p95: 1200 }),
      perf({ name: 'z-mid', p95: 980 }),
    ];
    expect(pOrder(rows, 'p95', 'desc')).toEqual(['a-slow', 'z-mid', 'q-fast']);
    expect(pOrder(rows, 'p95', 'asc')).toEqual(['q-fast', 'z-mid', 'a-slow']);
  });

  it('orders Count by the number, not the thousands-separated text', () => {
    // `toLocaleString` renders 1_000_052 as "1,000,052" and 48 as "48"; as
    // text "1,000,052" sorts BEFORE "48". p95 runs opposite to count here, so
    // a mis-wired accessor changes the answer.
    const rows = [
      perf({ name: 'busy', count: 1_000_052, p95: 5 }),
      perf({ name: 'rare', count: 48, p95: 900 }),
      perf({ name: 'some', count: 700, p95: 400 }),
    ];
    expect(pOrder(rows, 'count', 'desc')).toEqual(['busy', 'some', 'rare']);
    expect(pOrder(rows, 'count', 'asc')).toEqual(['rare', 'some', 'busy']);
  });

  it('orders Name and Op by their own fields', () => {
    // Name order and Op order disagree, so neither can stand in for the other.
    const rows = [
      perf({ name: 'a-load', op: 'screen_load' }),
      perf({ name: 'z-fetch', op: 'http' }),
      perf({ name: 'm-nav', op: 'navigation' }),
    ];
    expect(pOrder(rows, 'name', 'asc')).toEqual(['a-load', 'm-nav', 'z-fetch']);
    expect(pOrder(rows, 'op', 'asc')).toEqual(['z-fetch', 'm-nav', 'a-load']);
  });

  it('falls back to Count for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Name order runs opposite to count order — `aaa` is the BUSY one — so a
    // fallback to Name, the row label, shows up here.
    //
    // And the rows are supplied in the OPPOSITE order to the expected one, so
    // a fallback that TIES every row shows up too. That is the case the row
    // label alone cannot catch: `op`, `p95` and every other field hold their
    // constant default here, so a fallback reading one of them collapses to
    // input order — and if input order were also the expected order, this
    // assertion would pass while testing nothing.
    const rows = [
      perf({ name: 'zzz', count: 3 }),
      perf({ name: 'aaa', count: 90 }),
    ];
    expect(pOrder(rows, 'nope', 'desc')).toEqual(['aaa', 'zzz']);
    expect(DEVICE_PERF_DEFAULT_SORT).toEqual({ key: 'count', dir: 'desc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('nope');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    sOrder([sess({ id: 'a' })], 'duration', 'desc');
    pOrder([perf({ name: 'a' })], 'p95', 'desc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
