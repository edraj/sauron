import { describe, expect, it, vi } from 'vitest';
import {
  alertEventAccessor,
  channelAccessor,
  ruleAccessor,
  CHANNEL_DEFAULT_SORT,
  EVENT_DEFAULT_SORT,
  RULE_DEFAULT_SORT,
} from './alert-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type {
  AlertEvent,
  AlertRule,
  AlertSeverity,
  ChannelKind,
  NotificationChannel,
} from './index';

/**
 * Every fixture default below is a CONSTANT, never something derived from
 * another field, and every field a given test does not distinguish is equal
 * across that test's rows. Both rules exist so a plausible wrong accessor
 * changes the answer: an accessor that reads a neighbouring field then either
 * collates differently (fields under test are given disagreeing orders) or
 * collapses to input order (fields not under test tie), and input order is
 * never the expected order.
 */
function chan(over: Partial<NotificationChannel> & { id: string }): NotificationChannel {
  return {
    org_id: 'org',
    name: 'constant channel name',
    kind: 'slack',
    config: null,
    config_error: false,
    enabled: true,
    has_secret: true,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    ...over,
  };
}

/**
 * A label map that INVERTS the raw enum order.
 *
 * Deliberately not the page's real wording: the point is to prove the accessor
 * sorts by what the injected map returns, and a fake whose order disagrees with
 * the raw `kind` order is the only fixture that can prove it. (The real map
 * also disagrees — `matrix` renders "Element / Matrix" and belongs under E,
 * between `discord` and `email` — but only by one row, so it discriminates
 * weakly.)
 */
const KIND_FAKE: Record<string, string> = { email: 'Zeta', matrix: 'Alpha', slack: 'Mu' };
const kindLabel = (k: ChannelKind) => KIND_FAKE[k] ?? k;

const chanOrder = (rows: NotificationChannel[], key: string, dir: SortDir): string[] =>
  sortRows(rows, channelAccessor(key, kindLabel), dir).map((c) => c.id);

describe('channelAccessor', () => {
  it('orders by name, case-insensitively', () => {
    // `kind` is equal across the three, so an accessor reading it instead
    // would leave input order — which is not the expected order either way.
    const rows = [
      chan({ id: 'b', name: 'ops slack' }),
      chan({ id: 'a', name: 'Escalations' }),
      chan({ id: 'c', name: 'webhooks' }),
    ];
    expect(chanOrder(rows, 'name', 'asc')).toEqual(['a', 'b', 'c']);
    expect(chanOrder(rows, 'name', 'desc')).toEqual(['c', 'b', 'a']);
  });

  it('orders Type by the rendered label, not by the raw kind', () => {
    // Raw kind order is email < matrix < slack, i.e. ['e', 'm', 's'].
    // Label order is Alpha < Mu < Zeta, i.e. ['m', 's', 'e']. An accessor
    // spelled `c.kind` therefore fails this outright.
    const rows = [
      chan({ id: 'e', kind: 'email' }),
      chan({ id: 's', kind: 'slack' }),
      chan({ id: 'm', kind: 'matrix' }),
    ];
    expect(chanOrder(rows, 'type', 'asc')).toEqual(['m', 's', 'e']);
    expect(chanOrder(rows, 'type', 'desc')).toEqual(['e', 's', 'm']);
  });

  it('orders Status disabled-first ascending', () => {
    const rows = [
      chan({ id: 'on1', enabled: true }),
      chan({ id: 'off', enabled: false }),
      chan({ id: 'on2', enabled: true }),
    ];
    expect(chanOrder(rows, 'status', 'asc')).toEqual(['off', 'on1', 'on2']);
    expect(chanOrder(rows, 'status', 'desc')).toEqual(['on1', 'on2', 'off']);
  });

  it('falls back to the default column for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Name order and kind-label order disagree here, so this pins the fallback
    // to Name specifically rather than to "some ordering".
    const rows = [
      chan({ id: 'e', name: 'zulu', kind: 'email' }),
      chan({ id: 's', name: 'alpha', kind: 'slack' }),
      chan({ id: 'm', name: 'mike', kind: 'matrix' }),
    ];
    expect(chanOrder(rows, 'no-such-column', 'asc')).toEqual(['s', 'm', 'e']);
    expect(CHANNEL_DEFAULT_SORT.key).toBe('name');
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    chanOrder([chan({ id: 'a' })], 'type', 'asc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});

function rule(over: Partial<AlertRule> & { id: string }): AlertRule {
  return {
    org_id: 'org',
    project_id: null,
    app_id: null,
    monitor_id: null,
    name: 'constant rule name',
    trigger_type: 'monitor_down',
    enabled: true,
    conditions: {},
    severity: 'warning',
    throttle_seconds: 300,
    message_template: null,
    last_evaluated_at: null,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    channel_ids: ['c1'],
    ...over,
  };
}

/** Inverts raw trigger order, for the reason `KIND_FAKE` does. */
const TRIGGER_FAKE: Record<string, string> = {
  error_spike: 'Zulu',
  issue_new: 'Mike',
  monitor_down: 'Alpha',
};
const triggerLabel = (t: AlertRule['trigger_type']) => TRIGGER_FAKE[t] ?? t;

const ruleOrder = (rows: AlertRule[], key: string, dir: SortDir): string[] =>
  sortRows(rows, ruleAccessor(key, triggerLabel), dir).map((r) => r.id);

describe('ruleAccessor', () => {
  it('orders by name, which is not the order of the row ids', () => {
    // The ids used to be `a` / `b` / `c` in name order, so `name: (r) => r.id`
    // — the accessor reading the row label instead of the column — reproduced
    // this assertion exactly. Rule R2. The prefixes below put the id order
    // (a-signup, m-api, z-p95) outside both expected orders.
    const rows = [
      rule({ id: 'z-p95', name: 'p95 latency' }),
      rule({ id: 'a-signup', name: 'Signup errors' }),
      rule({ id: 'm-api', name: 'API down' }),
    ];
    expect(ruleOrder(rows, 'name', 'asc')).toEqual(['m-api', 'z-p95', 'a-signup']);
    expect(ruleOrder(rows, 'name', 'desc')).toEqual(['a-signup', 'z-p95', 'm-api']);
  });

  it('orders Trigger by the rendered label, not by the raw trigger_type', () => {
    // Raw order: error_spike < issue_new < monitor_down → ['e', 'i', 'm'].
    // Label order: Alpha < Mike < Zulu → ['m', 'i', 'e'].
    const rows = [
      rule({ id: 'i', trigger_type: 'issue_new' }),
      rule({ id: 'e', trigger_type: 'error_spike' }),
      rule({ id: 'm', trigger_type: 'monitor_down' }),
    ];
    expect(ruleOrder(rows, 'trigger', 'asc')).toEqual(['m', 'i', 'e']);
  });

  it('orders Severity by RANK, which disagrees with its spelling both ways', () => {
    // The three severities are not in alphabetical order as a ladder, which is
    // what makes this test able to fail:
    //   text asc  → critical, info, warning   (info above warning, critical
    //                                          least severe of all: wrong)
    //   text desc → warning, info, critical
    //   rank asc  → info, warning, critical   (least severe first)
    //   rank desc → critical, warning, info
    // The accessor this replaces, `(r) => r.severity`, produces the text rows
    // and fails both assertions.
    const rows = [
      rule({ id: 'w', severity: 'warning' }),
      rule({ id: 'c', severity: 'critical' }),
      rule({ id: 'i', severity: 'info' }),
    ];
    expect(ruleOrder(rows, 'severity', 'asc')).toEqual(['i', 'w', 'c']);
    expect(ruleOrder(rows, 'severity', 'desc')).toEqual(['c', 'w', 'i']);
  });

  it('ranks a severity this build has never heard of last in BOTH directions', () => {
    // `AlertSeverity` is the dashboard's copy of the server's enum. A fourth
    // one added server-side arrives as a plain string; it is unknown, not the
    // most severe thing on the page, and must lead neither direction.
    const rows = [
      rule({ id: 'new', severity: 'blocker' as AlertSeverity }),
      rule({ id: 'c', severity: 'critical' }),
      rule({ id: 'i', severity: 'info' }),
    ];
    expect(ruleOrder(rows, 'severity', 'asc')).toEqual(['i', 'c', 'new']);
    expect(ruleOrder(rows, 'severity', 'desc')).toEqual(['c', 'i', 'new']);
  });

  it('ranks every AlertSeverity — a fourth one fails to compile here', () => {
    // The annotation does the work: a `Record<AlertSeverity, number>` literal
    // must name every member of the union, so adding one breaks this line and
    // the loop then reports whether `SEVERITY_ORDER` was updated too. The
    // ladder itself is `readonly AlertSeverity[]`, which a ladder missing a
    // member satisfies perfectly well.
    const expected: Record<AlertSeverity, number> = { info: 0, warning: 1, critical: 2 };
    const accessor = ruleAccessor('severity', triggerLabel);
    for (const [severity, rank] of Object.entries(expected)) {
      expect(accessor(rule({ id: severity, severity: severity as AlertSeverity }))).toBe(rank);
    }
  });

  it('orders Throttle and Channels by their own numbers, which run opposite', () => {
    // The two numeric columns sit next to each other and disagree row for row,
    // so an accessor reading the wrong one inverts the answer rather than
    // coincidentally agreeing with it.
    const rows = [
      rule({ id: 'mid', throttle_seconds: 300, channel_ids: ['a', 'b'] }),
      rule({ id: 'long', throttle_seconds: 3600, channel_ids: ['a'] }),
      rule({ id: 'short', throttle_seconds: 60, channel_ids: ['a', 'b', 'c'] }),
    ];
    expect(ruleOrder(rows, 'throttle', 'desc')).toEqual(['long', 'mid', 'short']);
    expect(ruleOrder(rows, 'throttle', 'asc')).toEqual(['short', 'mid', 'long']);
    expect(ruleOrder(rows, 'channels', 'desc')).toEqual(['short', 'mid', 'long']);
    expect(ruleOrder(rows, 'channels', 'asc')).toEqual(['long', 'mid', 'short']);
  });

  it('counts channels rather than ordering by the first channel id or the row id', () => {
    // A rule fanning out to one late-alphabet channel must not outrank one
    // fanning out to three early-alphabet channels. Ordering by
    // `channel_ids[0]` would answer ['single', 'broadcast'] descending.
    //
    // The rows are `broadcast` (3) and `single` (1) rather than `many` / `few`
    // because `few` < `many` collates exactly as the counts do, which made
    // `channels: (r) => r.id` pass both assertions. With two rows there are
    // only two orders, so the id has to be spelled into the WRONG one — it
    // cannot be spelled into neither.
    const rows = [
      rule({ id: 'broadcast', channel_ids: ['aaa', 'bbb', 'ccc'] }),
      rule({ id: 'single', channel_ids: ['zzz'] }),
    ];
    expect(ruleOrder(rows, 'channels', 'desc')).toEqual(['broadcast', 'single']);
    expect(ruleOrder(rows, 'channels', 'asc')).toEqual(['single', 'broadcast']);
  });

  it('orders Status disabled-first ascending, by the flag and not the row id', () => {
    // `off` < `on` collated exactly as the flag does, so `status: (r) => r.id`
    // passed. `active` / `paused` reads the same way round and collates the
    // other, which is the whole trick: with two rows the id order must be one
    // of the two, so make it the wrong one.
    const rows = [rule({ id: 'active', enabled: true }), rule({ id: 'paused', enabled: false })];
    expect(ruleOrder(rows, 'status', 'asc')).toEqual(['paused', 'active']);
    expect(ruleOrder(rows, 'status', 'desc')).toEqual(['active', 'paused']);
  });

  it('falls back to the rules default column, not the channels one', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const rows = [
      rule({ id: 'z', name: 'zulu', trigger_type: 'monitor_down' }),
      rule({ id: 'a', name: 'alpha', trigger_type: 'error_spike' }),
    ];
    expect(ruleOrder(rows, 'nope', 'asc')).toEqual(['a', 'z']);
    expect(RULE_DEFAULT_SORT.key).toBe('name');
    expect(String(warn.mock.calls[0]?.[0])).toContain('rules');
    warn.mockRestore();
  });
});

function evt(over: Partial<AlertEvent> & { id: string }): AlertEvent {
  return {
    org_id: 'org',
    rule_id: 'r1',
    channel_id: 'ch-constant',
    trigger_type: 'monitor_down',
    dedup_key: 'dedup',
    status: 'sent',
    title: 'constant title',
    body: '',
    error: null,
    attempts: 1,
    created_at: '2026-02-02T09:00:00Z',
    ...over,
  };
}

/** Channel-id order and channel-name order deliberately disagree. */
const CHANNEL_NAMES: Record<string, string> = {
  'id-a': 'Telegram oncall',
  'id-b': 'Email digest',
  'id-c': 'Ops Slack',
};
/**
 * Returns `null`, never the em dash the cell renders, for a delivery with no
 * channel or one whose channel has been deleted — the contract `ChannelName`
 * states and the page implements.
 */
const channelName = (id: string | null) => (id ? (CHANNEL_NAMES[id] ?? null) : null);

const evtOrder = (rows: AlertEvent[], key: string, dir: SortDir): string[] =>
  sortRows(rows, alertEventAccessor(key, channelName), dir).map((h) => h.id);

describe('alertEventAccessor', () => {
  it('orders When by instant, not by the formatted local date', () => {
    // Same wall-clock time, three days apart. An accessor that reused the
    // cell's `toLocaleString` would still order these correctly under an
    // ISO-like locale and WRONGLY under a day-first one, so the fixture pins
    // the instant instead: the two differ only in the date part.
    const rows = [
      evt({ id: 'older', created_at: '2026-02-02T09:00:00Z' }),
      evt({ id: 'newer', created_at: '2026-02-05T09:00:00Z' }),
      evt({ id: 'oldest', created_at: '2026-01-30T09:00:00Z' }),
    ];
    expect(evtOrder(rows, 'when', 'desc')).toEqual(['newer', 'older', 'oldest']);
    expect(evtOrder(rows, 'when', 'asc')).toEqual(['oldest', 'older', 'newer']);
  });

  it('orders Channel by the resolved name, not by the channel id', () => {
    // Id order is id-a < id-b < id-c → ['a', 'b', 'c'].
    // Name order is Email digest < Ops Slack < Telegram oncall → ['b','c','a'].
    const rows = [
      evt({ id: 'a', channel_id: 'id-a' }),
      evt({ id: 'b', channel_id: 'id-b' }),
      evt({ id: 'c', channel_id: 'id-c' }),
    ];
    expect(evtOrder(rows, 'channel', 'asc')).toEqual(['b', 'c', 'a']);
  });

  it('keeps a delivery with no resolvable channel last, in BOTH directions', () => {
    // The cell renders an em dash for these. Ordering by that literal would
    // collate it before every real name — leading the ascending sort and
    // trailing the descending one — which is a channel-less delivery claiming
    // to be alphabetically first. It is absent, not small.
    const rows = [
      evt({ id: 'ops', channel_id: 'id-c' }),
      evt({ id: 'orphan', channel_id: 'id-deleted' }), // channel since deleted
      evt({ id: 'none', channel_id: null }), // never had one
      evt({ id: 'email', channel_id: 'id-b' }),
    ];
    expect(evtOrder(rows, 'channel', 'asc')).toEqual(['email', 'ops', 'orphan', 'none']);
    expect(evtOrder(rows, 'channel', 'desc')).toEqual(['ops', 'email', 'orphan', 'none']);
  });

  it('orders Title by its own field', () => {
    const rows = [
      evt({ id: 'x', title: 'Zeta went down' }),
      evt({ id: 'y', title: 'Alpha recovered' }),
      evt({ id: 'z', title: 'Mid spike' }),
    ];
    expect(evtOrder(rows, 'title', 'asc')).toEqual(['y', 'z', 'x']);
  });

  it('orders Status by delivery RANK, not by the outcome word', () => {
    // All four outcomes, and the two orders share no position:
    //   text asc  → failed, sent, skipped, throttled  (a failure leading the
    //               ascending sort by spelling alone)
    //   text desc → throttled, skipped, sent, failed
    //   rank asc  → sent, skipped, throttled, failed  (least worth looking at)
    //   rank desc → failed, throttled, skipped, sent
    // Titles are left at the fixture's constant, so an accessor reading
    // `h.title` collapses to input order and dies here too.
    const rows = [
      evt({ id: 'thr', status: 'throttled' }),
      evt({ id: 'sent', status: 'sent' }),
      evt({ id: 'fail', status: 'failed' }),
      evt({ id: 'skip', status: 'skipped' }),
    ];
    expect(evtOrder(rows, 'status', 'asc')).toEqual(['sent', 'skip', 'thr', 'fail']);
    expect(evtOrder(rows, 'status', 'desc')).toEqual(['fail', 'thr', 'skip', 'sent']);
  });

  it('ranks an outcome this build has never heard of last in BOTH directions', () => {
    const rows = [
      evt({ id: 'new', status: 'deferred' as AlertEvent['status'] }),
      evt({ id: 'fail', status: 'failed' }),
      evt({ id: 'sent', status: 'sent' }),
    ];
    expect(evtOrder(rows, 'status', 'asc')).toEqual(['sent', 'fail', 'new']);
    expect(evtOrder(rows, 'status', 'desc')).toEqual(['fail', 'sent', 'new']);
  });

  it('ranks every delivery outcome — a fifth one fails to compile here', () => {
    // See the severity version above for why the `Record` annotation, and not
    // the ladder's own `readonly AlertEvent['status'][]`, is what catches an
    // outcome added to the union and forgotten in the ladder.
    const expected: Record<AlertEvent['status'], number> = {
      sent: 0,
      skipped: 1,
      throttled: 2,
      failed: 3,
    };
    const accessor = alertEventAccessor('status', channelName);
    for (const [status, rank] of Object.entries(expected)) {
      expect(accessor(evt({ id: status, status: status as AlertEvent['status'] }))).toBe(rank);
    }
  });

  it('orders Attempts by magnitude', () => {
    const rows = [
      evt({ id: 'one', attempts: 1 }),
      evt({ id: 'ten', attempts: 10 }),
      evt({ id: 'two', attempts: 2 }),
    ];
    expect(evtOrder(rows, 'attempts', 'desc')).toEqual(['ten', 'two', 'one']);
  });

  it('falls back to When, the history default, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Title order runs OPPOSITE to When order, so a fallback to Title — the
    // next column along — would invert this and the test would catch it.
    const rows = [
      evt({ id: 'older', created_at: '2026-02-02T09:00:00Z', title: 'zzz' }),
      evt({ id: 'newer', created_at: '2026-02-05T09:00:00Z', title: 'aaa' }),
    ];
    expect(evtOrder(rows, 'nope', 'desc')).toEqual(['newer', 'older']);
    expect(EVENT_DEFAULT_SORT).toEqual({ key: 'when', dir: 'desc' });
    expect(String(warn.mock.calls[0]?.[0])).toContain('history');
    warn.mockRestore();
  });
});
