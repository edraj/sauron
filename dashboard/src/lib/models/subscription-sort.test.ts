import { describe, expect, it, vi } from 'vitest';
import { SUBSCRIPTION_DEFAULT_SORT, subscriptionAccessor } from './subscription-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { NotificationSubscription, SubscriptionKind } from './index';

/**
 * Defaults are CONSTANTS, never derived from another field, and any field a
 * test does not distinguish ties across that test's rows — so an accessor
 * reading a neighbour either collates differently or collapses to input order,
 * and input order is never the expected order.
 */
function sub(
  over: Partial<NotificationSubscription> & { id: string },
): NotificationSubscription {
  return {
    scope_type: 'project',
    scope_id: 'scope',
    scope_name: 'Constant scope',
    project_id: null,
    kind: 'uptime',
    enabled: true,
    disabled_reason: null,
    environment_ids: [],
    conditions: {},
    delivery: 'immediate',
    effective_delivery: 'immediate',
    throttle_seconds: 0,
    quiet_start_min: null,
    quiet_end_min: null,
    quiet_tz: 'UTC',
    created_at: '2026-07-01T00:00:00Z',
    ...over,
  };
}

/**
 * Inverts the raw kind order, so an accessor spelled `s.kind` fails outright.
 * (The component's real map also disagrees with raw order, but only in part.)
 */
const KIND_FAKE: Record<string, string> = {
  uptime: 'Alpha',
  error_spike: 'Zulu',
  error_new_issue: 'Mike',
};
const kindLabel = (k: SubscriptionKind) => KIND_FAKE[k] ?? k;

const order = (rows: NotificationSubscription[], key: string, dir: SortDir): string[] =>
  sortRows(rows, subscriptionAccessor(key, kindLabel), dir).map((s) => s.id);

describe('subscriptionAccessor', () => {
  it('orders Scope by the rendered phrase, which is not the same as by name', () => {
    // `describeSubscription` renders `App “Zulu”` / `Project “Alpha”`, so the
    // noun leads: the App row sorts FIRST despite having the last name
    // alphabetically. An accessor spelled `s.scope_name` would answer
    // ['proj-a', 'proj-b', 'app'], and `s.scope_id` — constant here — would
    // collapse to input order. Both die on this assertion.
    //
    // The prefixes are load-bearing. Labelled `app` / `proj-a` / `proj-b` the
    // ids collated in exactly the rendered-phrase order, so `scope: (s) => s.id`
    // — the row label read as if it were the column — passed both assertions.
    const rows = [
      sub({ id: 'm-proj-beta', scope_type: 'project', scope_name: 'Beta' }),
      sub({ id: 'z-app', scope_type: 'app', scope_name: 'Zulu' }),
      sub({ id: 'a-proj-alpha', scope_type: 'project', scope_name: 'Alpha' }),
    ];
    expect(order(rows, 'scope', 'asc')).toEqual(['z-app', 'a-proj-alpha', 'm-proj-beta']);
    expect(order(rows, 'scope', 'desc')).toEqual(['m-proj-beta', 'a-proj-alpha', 'z-app']);
  });

  it('orders "Notify about" by the rendered label, not by the raw kind', () => {
    // Raw order: error_new_issue < error_spike < uptime → ['n', 's', 'u'].
    // Label order: Alpha < Mike < Zulu → ['u', 'n', 's'].
    const rows = [
      sub({ id: 'n', kind: 'error_new_issue' }),
      sub({ id: 's', kind: 'error_spike' }),
      sub({ id: 'u', kind: 'uptime' }),
    ];
    expect(order(rows, 'kind', 'asc')).toEqual(['u', 'n', 's']);
  });

  it('orders Environments by how many are selected, with "All" (none) first ascending', () => {
    // Labelled `all` / `one` / `two` the ids collated in count order, so
    // `environments: (s) => s.id` passed both assertions. The prefixes put the
    // id order (a-two, m-all, z-one) outside both.
    const rows = [
      sub({ id: 'a-two', environment_ids: ['a', 'b'] }),
      sub({ id: 'm-all', environment_ids: [] }),
      sub({ id: 'z-one', environment_ids: ['a'] }),
    ];
    expect(order(rows, 'environments', 'asc')).toEqual(['m-all', 'z-one', 'a-two']);
    expect(order(rows, 'environments', 'desc')).toEqual(['a-two', 'z-one', 'm-all']);
  });

  it('orders Delivery by the EFFECTIVE value, not the requested one', () => {
    // `capped` rows ask for `immediate` and get `hourly`; the cell shows the
    // latter. An accessor reading `delivery` would order every capped row as
    // though it were immediate — the "sorts by one value, shows another"
    // shape. Here `delivery` is identical on all three, so that mutant
    // collapses to input order and dies.
    //
    // The immediate row is `a-now`, not `now`: `capped-daily` < `capped-hourly`
    // < `now` collated in exactly cadence order, so `delivery: (s) => s.id`
    // passed both assertions below.
    const rows = [
      sub({ id: 'capped-hourly', delivery: 'immediate', effective_delivery: 'hourly' }),
      sub({ id: 'a-now', delivery: 'immediate', effective_delivery: 'immediate' }),
      sub({ id: 'capped-daily', delivery: 'immediate', effective_delivery: 'daily' }),
    ];
    // Least frequent first. Task 5 checked this column and left it as text:
    // alphabetical order and cadence order are the SAME here (`daily < hourly
    // < immediate` both ways), so a rank would produce this identical
    // assertion. The previous comment claimed the two differ; they do not.
    expect(order(rows, 'delivery', 'asc')).toEqual(['capped-daily', 'capped-hourly', 'a-now']);
    expect(order(rows, 'delivery', 'desc')).toEqual(['a-now', 'capped-hourly', 'capped-daily']);
  });

  it('orders Quiet hours by the start minute and keeps "Always on" last both ways', () => {
    // `quiet_start_min: null` renders as "Always on" — an absent window, not a
    // time of day. As label text it would lead one direction and trail the
    // other; as a null it trails both.
    const rows = [
      sub({ id: 'evening', quiet_start_min: 1320, quiet_end_min: 420 }),
      sub({ id: 'always', quiet_start_min: null, quiet_end_min: null }),
      sub({ id: 'morning', quiet_start_min: 60, quiet_end_min: 480 }),
    ];
    expect(order(rows, 'quiet_hours', 'asc')).toEqual(['morning', 'evening', 'always']);
    expect(order(rows, 'quiet_hours', 'desc')).toEqual(['evening', 'morning', 'always']);
  });

  it('orders State with both Off variants together, ahead of On', () => {
    const rows = [
      sub({ id: 'on', enabled: true }),
      sub({ id: 'off-revoked', enabled: false, disabled_reason: 'access_revoked' }),
      sub({ id: 'off', enabled: false, disabled_reason: 'unsubscribed' }),
    ];
    expect(order(rows, 'state', 'asc')).toEqual(['off-revoked', 'off', 'on']);
    expect(order(rows, 'state', 'desc')).toEqual(['on', 'off-revoked', 'off']);
  });

  it('falls back to Scope for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Kind-label order runs opposite to scope order, so a fallback to any
    // other column would show up here.
    const rows = [
      sub({ id: 'z', scope_name: 'Zulu', kind: 'uptime' }),
      sub({ id: 'a', scope_name: 'Alpha', kind: 'error_spike' }),
    ];
    expect(order(rows, 'no-such-column', 'asc')).toEqual(['a', 'z']);
    expect(SUBSCRIPTION_DEFAULT_SORT).toEqual({ key: 'scope', dir: 'asc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    order([sub({ id: 'a' })], 'quiet_hours', 'asc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
