import { describe, expect, it, vi } from 'vitest';
import { JOURNEY_TRANSITION_DEFAULT_SORT, journeyTransitionAccessor } from './journey-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { JourneyLink } from './index';

/**
 * Defaults are CONSTANTS, never derived from another field, and any field a
 * test does not distinguish ties across that test's rows — so an accessor that
 * reads a neighbour either collates differently or collapses to input order,
 * and input order is never the expected order.
 *
 * `JourneyLink` has no id, so each row is identified by the tuple the fixture
 * builder is given; the helpers below map back to a label passed in explicitly
 * rather than to a field an accessor might read.
 */
function link(label: string, over: Partial<JourneyLink> = {}): JourneyLink & { label: string } {
  return {
    from_step: 0,
    from_event: 'app_open',
    to_event: 'view_home',
    count: 500,
    label,
    ...over,
  };
}

const order = (
  rows: (JourneyLink & { label: string })[],
  key: string,
  dir: SortDir,
): string[] => sortRows(rows, journeyTransitionAccessor(key), dir).map((l) => l.label);

describe('journeyTransitionAccessor', () => {
  it('orders Users by the count, not the thousands-separated text', () => {
    // The cell renders `toLocaleString()`, so 1_000_052 shows as "1,000,052"
    // and 48 as "48" — as text the million sorts FIRST ascending. Every other
    // field ties here, so a mis-wired accessor collapses to input order.
    const rows = [
      link('mid', { count: 700 }),
      link('busy', { count: 1_000_052 }),
      link('rare', { count: 48 }),
    ];
    expect(order(rows, 'users', 'desc')).toEqual(['busy', 'mid', 'rare']);
    expect(order(rows, 'users', 'asc')).toEqual(['rare', 'mid', 'busy']);
  });

  it('orders From by the event AND the step the cell renders beside it', () => {
    // All three share one event name, so an accessor spelled `l.from_event`
    // calls them equal and leaves input order — which is not the expected
    // order. Steps 1 and 9 straddle 10 to pin the collator's numeric mode:
    // as plain text "step 10" would sort between "step 1" and "step 9".
    const rows = [
      link('nine', { from_event: 'checkout', from_step: 8 }),
      link('ten', { from_event: 'checkout', from_step: 9 }),
      link('one', { from_event: 'checkout', from_step: 0 }),
    ];
    expect(order(rows, 'from', 'asc')).toEqual(['one', 'nine', 'ten']);
    expect(order(rows, 'from', 'desc')).toEqual(['ten', 'nine', 'one']);
  });

  it('orders From by event name first, step second', () => {
    // `count` runs opposite to the event names, so the default column cannot
    // produce this order either.
    const rows = [
      link('z1', { from_event: 'zzz_event', from_step: 0, count: 900 }),
      link('a2', { from_event: 'aaa_event', from_step: 1, count: 1 }),
      link('a1', { from_event: 'aaa_event', from_step: 0, count: 2 }),
    ];
    expect(order(rows, 'from', 'asc')).toEqual(['a1', 'a2', 'z1']);
  });

  it('orders To by the destination event alone', () => {
    // `from_event` runs opposite to `to_event`, so an accessor reading the
    // wrong end of the transition dies here.
    const rows = [
      link('to-z', { from_event: 'aaa_from', to_event: 'zzz_to' }),
      link('to-a', { from_event: 'zzz_from', to_event: 'aaa_to' }),
      link('to-m', { from_event: 'mmm_from', to_event: 'mmm_to' }),
    ];
    expect(order(rows, 'to', 'asc')).toEqual(['to-a', 'to-m', 'to-z']);
    expect(order(rows, 'to', 'desc')).toEqual(['to-z', 'to-m', 'to-a']);
  });

  it('falls back to Users for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Event-name order runs opposite to count order — the SMALL row is `zzz`
    // — so a fallback to From or To shows up here. With the names the other
    // way round the two orders coincided and this could not tell Users from
    // either of them.
    const rows = [
      link('small', { count: 3, from_event: 'zzz', to_event: 'zzz' }),
      link('big', { count: 900, from_event: 'aaa', to_event: 'aaa' }),
    ];
    expect(order(rows, 'no-such-column', 'desc')).toEqual(['big', 'small']);
    expect(JOURNEY_TRANSITION_DEFAULT_SORT).toEqual({ key: 'users', dir: 'desc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    order([link('a')], 'from', 'asc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
