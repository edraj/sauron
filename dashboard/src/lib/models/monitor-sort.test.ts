import { describe, expect, it, vi } from 'vitest';
import { MONITOR_DEFAULT_SORT, monitorAccessor } from './monitor-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { MonitorListItem, MonitorStatus } from './index';

/**
 * Fields default to CONSTANTS, never to something derived from another field.
 *
 * `target` used to default to `https://${name}.example.com`, which made the
 * name and target columns collate identically — so `name: (m) => m.target`,
 * the most plausible wrong accessor there is, produced the expected answer and
 * the test could not fail. Every fixture below is built so that a plausible
 * mis-wiring changes the result: fields that a test distinguishes are given
 * orders that disagree with each other, and fields it does not are equal
 * across rows, so reading one of those instead collapses to input order.
 *
 * `id` defaulted to `over.name` until the final review — the same defect, one
 * field further along and against the rule stated directly above it. Rows are
 * labelled by `m.name`, so `name: (m) => m.id` — a Monitors table sorted by
 * uuid — reproduced every expectation in this file. It is a constant now, and
 * it must stay one: nothing here needs ids to differ.
 */
function mon(over: Partial<MonitorListItem> & { name: string }): MonitorListItem {
  return {
    id: 'mon-constant',
    kind: 'http',
    target: 'https://constant.example.com/health',
    status: 'up',
    enabled: true,
    last_response_time_ms: null,
    last_checked_at: null,
    uptime_24h: null,
    ...over,
  };
}

/**
 * Name order and target order deliberately disagree, and neither matches input
 * order: by name it is Auth, checkout, db; by target it is checkout, db, Auth.
 */
const trio = (): MonitorListItem[] => [
  mon({ name: 'checkout', target: 'https://aaa.example.com/health' }),
  mon({ name: 'Auth', target: 'https://zzz.example.com/health' }),
  mon({ name: 'db', target: 'https://mmm.example.com/health' }),
];

/** Sort through the real accessor and report the resulting row order by name. */
const order = (rows: MonitorListItem[], key: string, dir: SortDir): string[] =>
  sortRows(rows, monitorAccessor(key), dir).map((m) => m.name);

describe('monitorAccessor', () => {
  it('orders by name case-insensitively, and by name rather than target', () => {
    const rows = trio();
    expect(order(rows, 'name', 'asc')).toEqual(['Auth', 'checkout', 'db']);
    expect(order(rows, 'name', 'desc')).toEqual(['db', 'checkout', 'Auth']);
  });

  it('orders by target rather than name', () => {
    const rows = trio();
    expect(order(rows, 'target', 'asc')).toEqual(['checkout', 'db', 'Auth']);
    expect(order(rows, 'target', 'desc')).toEqual(['Auth', 'db', 'checkout']);
  });

  it('orders Status by health RANK, which is not its alphabetical order', () => {
    // Four states and four names, chosen so that all four candidate answers
    // are different — a status fixture whose values happen to be in
    // alphabetical order cannot tell a ranking from a spelling:
    //   by name   → atlas, mercury, nova, zenith
    //   text asc  → mercury(down), atlas(paused), zenith(unknown), nova(up)
    //   rank asc  → nova(up), atlas(paused), zenith(unknown), mercury(down)
    //   rank desc → mercury, zenith, atlas, nova
    // The old accessor `(m) => m.status` produces the text rows above and fails
    // both assertions; `(m) => m.name` collapses to alphabetical and fails too.
    const rows = [
      mon({ name: 'zenith', status: 'unknown' }),
      mon({ name: 'nova', status: 'up' }),
      mon({ name: 'mercury', status: 'down' }),
      mon({ name: 'atlas', status: 'paused' }),
    ];
    expect(order(rows, 'status', 'asc')).toEqual(['nova', 'atlas', 'zenith', 'mercury']);
    expect(order(rows, 'status', 'desc')).toEqual(['mercury', 'zenith', 'atlas', 'nova']);
  });

  it('ranks a state this build has never heard of last in BOTH directions', () => {
    // `MonitorStatus` is this dashboard's idea of the backend's enum, not the
    // backend's — a `degraded` added server-side arrives as a plain string and
    // type-checks nowhere. The cast is the point of the test: an unranked state
    // must not lead the worst-first list it was never ranked for, nor the
    // best-first one.
    const rows = [
      mon({ name: 'future', status: 'degraded' as MonitorStatus }),
      mon({ name: 'broken', status: 'down' }),
      mon({ name: 'fine', status: 'up' }),
    ];
    expect(order(rows, 'status', 'asc')).toEqual(['fine', 'broken', 'future']);
    expect(order(rows, 'status', 'desc')).toEqual(['broken', 'fine', 'future']);
  });

  it('ranks every MonitorStatus — an added state fails to compile here', () => {
    // The annotation does the work: a `Record<MonitorStatus, number>` literal
    // must name every member of the union, so widening `MonitorStatus` breaks
    // this line, and the loop then reports whether the LADDER was updated too.
    // The ladder itself is only `readonly MonitorStatus[]`, which a missing
    // member satisfies perfectly well.
    const expected: Record<MonitorStatus, number> = { up: 0, paused: 1, unknown: 2, down: 3 };
    const accessor = monitorAccessor('status');
    for (const [status, rank] of Object.entries(expected)) {
      expect(accessor(mon({ name: status, status: status as MonitorStatus }))).toBe(rank);
    }
  });

  it('orders uptime by magnitude and keeps an unmeasured monitor last both ways', () => {
    // The trap this catches is an accessor spelled `m.uptime_24h ?? 0`: null
    // would then sort as 0%, so a monitor that has never reported would lead a
    // worst-first sort as if it were the least available one. It is unknown,
    // not down.
    const rows = [
      mon({ name: 'ok', uptime_24h: 99.9 }),
      mon({ name: 'never', uptime_24h: null }),
      mon({ name: 'flaky', uptime_24h: 87.5 }),
    ];
    expect(order(rows, 'uptime', 'asc')).toEqual(['flaky', 'ok', 'never']);
    expect(order(rows, 'uptime', 'desc')).toEqual(['ok', 'flaky', 'never']);
  });

  it('orders latency by magnitude and keeps an unmeasured monitor last both ways', () => {
    const rows = [
      mon({ name: 'slow', last_response_time_ms: 1200 }),
      mon({ name: 'never', last_response_time_ms: null }),
      mon({ name: 'fast', last_response_time_ms: 90 }),
      mon({ name: 'mid', last_response_time_ms: 130 }),
    ];
    expect(order(rows, 'latency', 'desc')).toEqual(['slow', 'mid', 'fast', 'never']);
    expect(order(rows, 'latency', 'asc')).toEqual(['fast', 'mid', 'slow', 'never']);
  });

  it('orders "checked" by instant, not by the formatted time of day', () => {
    // The cell renders `toLocaleTimeString`, which drops the date. An accessor
    // that reused that formatting would call these two equal — same clock
    // time, three days apart — and leave them in input order, which reads as a
    // working sort.
    const rows = [
      mon({ name: 'older', last_checked_at: '2026-08-05T14:31:00Z' }),
      mon({ name: 'newer', last_checked_at: '2026-08-08T14:31:00Z' }),
      mon({ name: 'never', last_checked_at: null }),
    ];
    expect(order(rows, 'checked', 'desc')).toEqual(['newer', 'older', 'never']);
    expect(order(rows, 'checked', 'asc')).toEqual(['older', 'newer', 'never']);
  });

  it('falls back to the default column for an unknown key, and says so in dev', () => {
    // A key with no accessor must not throw and must not leave the table in an
    // arbitrary order. It sorts by the default column instead — which looks
    // like a working sort, so the fallback also warns. `trio`'s target order
    // differs from its name order, so this pins the fallback to Name
    // specifically rather than to "some ordering".
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const rows = trio();
    expect(order(rows, 'no-such-column', 'asc')).toEqual(['Auth', 'checkout', 'db']);
    expect(MONITOR_DEFAULT_SORT.key).toBe('name');
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    order(trio(), 'name', 'asc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
