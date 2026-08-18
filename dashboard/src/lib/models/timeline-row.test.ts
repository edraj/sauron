import { describe, it, expect } from 'vitest';
import type { AnalyticsEvent, ErrorEvent, TimelineItem, Transaction } from './index';
import {
  NO_TIMELINE_FILTER,
  ROW_CATEGORIES,
  categoryCounts,
  filterTimeline,
  httpStatusTone,
  isHttp,
  isNavigation,
  isTimelineFiltered,
  offsetMs,
  opCounts,
  rowCategory,
  rowCulprit,
  rowKind,
  rowTitle,
  transactionOp,
  type TimelineFilter,
} from './timeline-row';

function ev(
  at: string,
  name: string,
  properties: Record<string, unknown> | null = null,
  screen: string | null = null,
): TimelineItem {
  const event = {
    id: `e-${at}-${name}`,
    name,
    distinct_id: 'u1',
    properties,
    screen,
    occurred_at: at,
  } as AnalyticsEvent;
  return { kind: 'event', at, event };
}

function err(at: string, over: Partial<ErrorEvent> = {}): TimelineItem {
  const error = { id: `x-${at}`, level: 'error', occurred_at: at, ...over } as ErrorEvent;
  return { kind: 'error', at, error };
}

function tx(at: string, name: string, over: Partial<Transaction> = {}): TimelineItem {
  const transaction = {
    id: `t-${at}`,
    name,
    op: 'custom',
    duration_ms: 12,
    ...over,
  } as Transaction;
  return { kind: 'transaction', at, transaction };
}

function http(name: string, over: Partial<Transaction> = {}): TimelineItem {
  return tx('2026-08-11T14:17:16Z', name, { op: 'http', ...over });
}

describe('rowKind / isNavigation', () => {
  // The whole definition of "navigation related": the one event name both SDKs
  // auto-emit from setScreen. An app event merely *named* like a navigation
  // ("Onboarding menu", "route_change") is still the app's own event.
  it('tags only $screen events as navigation', () => {
    expect(isNavigation(ev('2026-08-11T14:17:03Z', '$screen'))).toBe(true);
    expect(rowKind(ev('2026-08-11T14:17:03Z', '$screen'))).toBe('navigation');

    expect(isNavigation(ev('2026-08-11T14:17:04Z', 'Onboarding menu'))).toBe(false);
    expect(rowKind(ev('2026-08-11T14:17:04Z', 'Onboarding menu'))).toBe('event');
    expect(rowKind(ev('2026-08-11T14:17:04Z', 'route_change'))).toBe('event');
  });

  // A transaction whose op is navigation is still a TRANSACTION row: it already
  // carries its own op badge and latency, and relabelling it would hide which
  // half of the timeline union it came from.
  it('leaves errors and non-http transactions on their own kind', () => {
    expect(rowKind(err('2026-08-11T14:17:14Z'))).toBe('error');
    expect(rowKind(tx('2026-08-11T14:17:14Z', '/checkout'))).toBe('transaction');
    expect(rowKind(tx('2026-08-11T14:17:14Z', '/checkout', { op: 'navigation' }))).toBe(
      'transaction',
    );
    expect(isNavigation(tx('2026-08-11T14:17:14Z', '/checkout'))).toBe(false);
  });
});

describe('isHttp / rowKind for HTTP', () => {
  // `op` is a first-class field the SDK fills alongside http_method/status/url,
  // so this is a read of the wire contract, not a guess from the name.
  it('tags transactions whose op is http', () => {
    expect(isHttp(http('GET /api/login'))).toBe(true);
    expect(rowKind(http('GET /api/login'))).toBe('http');
  });

  // Events carry no HTTP marker at all — nothing in the SDKs or the ingest
  // stamps one — so no event can ever answer this, whatever it is named.
  it('never tags an event, however it is named', () => {
    expect(isHttp(ev('2026-08-11T14:17:04Z', 'GET /api/login'))).toBe(false);
    expect(isHttp(ev('2026-08-11T14:17:04Z', '$screen'))).toBe(false);
    expect(isHttp(err('2026-08-11T14:17:14Z'))).toBe(false);
    expect(isHttp(tx('2026-08-11T14:17:14Z', '/checkout', { op: 'resource' }))).toBe(false);
  });
});

describe('httpStatusTone', () => {
  it('buckets by response class', () => {
    expect(httpStatusTone(200)).toBe('success');
    expect(httpStatusTone(204)).toBe('success');
    expect(httpStatusTone(301)).toBe('neutral');
    expect(httpStatusTone(404)).toBe('warning');
    expect(httpStatusTone(429)).toBe('warning');
    expect(httpStatusTone(500)).toBe('error');
    expect(httpStatusTone(503)).toBe('error');
  });

  // A missing status is "the SDK never recorded one" — a network failure, or an
  // in-flight call — and must not be coloured as though it succeeded.
  it('stays neutral for a missing or meaningless code', () => {
    expect(httpStatusTone(null)).toBe('neutral');
    expect(httpStatusTone(undefined)).toBe('neutral');
    expect(httpStatusTone(NaN)).toBe('neutral');
    expect(httpStatusTone(100)).toBe('neutral');
    expect(httpStatusTone(0)).toBe('neutral');
  });
});

describe('rowCulprit', () => {
  it('labels an error row with the crash site', () => {
    expect(rowCulprit(err('2026-08-11T14:17:14Z', { culprit: 'checkout (cart_bloc.dart)' }))).toBe(
      'checkout (cart_bloc.dart)',
    );
  });

  it('is null for every non-error row', () => {
    // Only errors have a culprit. A transaction's `name` and an event's
    // `properties` are not one, and coercing either into this slot would put a
    // URL where the reader expects a frame.
    expect(rowCulprit(ev('2026-08-11T14:17:03Z', '$screen'))).toBeNull();
    expect(rowCulprit(tx('2026-08-11T14:17:14Z', '/checkout'))).toBeNull();
  });

  it('is null when the occurrence has no frames, rather than an empty label', () => {
    // `""` is what `build_culprit` stores for a message-only capture, and the
    // column is absent entirely on pre-migration-30 rows. Both must render
    // nothing at all -- an empty span still costs the separator and the gap.
    expect(rowCulprit(err('2026-08-11T14:17:14Z', { culprit: '' }))).toBeNull();
    expect(rowCulprit(err('2026-08-11T14:17:14Z', { culprit: '   ' }))).toBeNull();
    expect(rowCulprit(err('2026-08-11T14:17:14Z', { culprit: null }))).toBeNull();
    expect(rowCulprit(err('2026-08-11T14:17:14Z'))).toBeNull();
  });
});

describe('rowTitle', () => {
  it('shows the screen a $screen event announces, marked with $', () => {
    expect(rowTitle(ev('2026-08-11T14:17:03Z', '$screen', { screen: 'Onboarding menu' }))).toBe(
      '$Onboarding menu',
    );
  });

  // Both SDKs set their current-screen state before emitting, so for a $screen
  // row the top-level column holds the screen being ENTERED, not the one left.
  it('falls back to the top-level screen column, then to the raw name', () => {
    expect(rowTitle(ev('2026-08-11T14:17:03Z', '$screen', null, 'Home'))).toBe('$Home');
    expect(rowTitle(ev('2026-08-11T14:17:03Z', '$screen'))).toBe('$screen');
  });

  // Property bags are unknown-valued on the wire: a number or a blank string
  // must not become the title "$123" or a bare "$".
  it('ignores non-string and blank screen values', () => {
    expect(rowTitle(ev('2026-08-11T14:17:03Z', '$screen', { screen: 123 }))).toBe('$screen');
    expect(rowTitle(ev('2026-08-11T14:17:03Z', '$screen', { screen: '   ' }))).toBe('$screen');
    expect(rowTitle(ev('2026-08-11T14:17:03Z', '$screen', { screen: '  Cart ' }))).toBe('$Cart');
  });

  it('leaves ordinary events, errors and transactions unchanged', () => {
    expect(rowTitle(ev('2026-08-11T14:17:04Z', 'Onboarding menu', { screen: 'Home' }))).toBe(
      'Onboarding menu',
    );
    expect(
      rowTitle(err('2026-08-11T14:17:14Z', { exception_type: 'Iwa', exception_value: 'boom' })),
    ).toBe('Iwa: boom');
    expect(rowTitle(err('2026-08-11T14:17:14Z', { exception_type: 'Iwa' }))).toBe('Iwa');
    expect(rowTitle(err('2026-08-11T14:17:14Z', { message: 'plain' }))).toBe('plain');
    expect(rowTitle(err('2026-08-11T14:17:14Z'))).toBe('Error');
    expect(rowTitle(tx('2026-08-11T14:17:14Z', '/checkout'))).toBe('/checkout');
  });

  // The JS SDK's auto-instrumentation already names the transaction
  // `${method} ${path}`, so an unconditional prefix would render "GET GET /x".
  it('does not double the method the SDK already put in the name', () => {
    expect(rowTitle(http('GET /api/login', { http_method: 'GET' }))).toBe('GET /api/login');
    expect(rowTitle(http('get /api/login', { http_method: 'get' }))).toBe('get /api/login');
  });

  // A hand-rolled trackTransaction, or another SDK, may name it anything.
  it('restores the method when the name lacks it', () => {
    expect(rowTitle(http('/api/login', { http_method: 'post' }))).toBe('POST /api/login');
    expect(rowTitle(http('checkout flow', { http_method: 'DELETE' }))).toBe(
      'DELETE checkout flow',
    );
    // "GETTING /x" starts with the letters but is not the method token.
    expect(rowTitle(http('GETTING /x', { http_method: 'GET' }))).toBe('GET GETTING /x');
  });

  it('leaves the name alone when there is no method to restore', () => {
    expect(rowTitle(http('/api/login'))).toBe('/api/login');
    expect(rowTitle(http('/api/login', { http_method: '  ' }))).toBe('/api/login');
    // Not an http op: the method, if any, is not ours to prepend.
    expect(rowTitle(tx('2026-08-11T14:17:14Z', '/checkout', { http_method: 'GET' }))).toBe(
      '/checkout',
    );
  });
});

describe('offsetMs', () => {
  const started = '2026-08-11T14:17:03.000Z';
  const items = [
    ev('2026-08-11T14:17:03.000Z', '$screen'),
    ev('2026-08-11T14:17:03.962Z', '$screen'),
    ev('2026-08-11T14:17:03.964Z', 'Onboarding menu'),
    ev('2026-08-11T14:17:13.740Z', '$screen'),
  ];

  it('measures from the session start in session mode', () => {
    expect(items.map((_, i) => offsetMs(items, i, started, 'session'))).toEqual([
      0, 962, 964, 10740,
    ]);
  });

  it('measures from the previous row in delta mode', () => {
    expect(items.map((_, i) => offsetMs(items, i, started, 'delta'))).toEqual([
      null, 962, 2, 9776,
    ]);
  });

  // null, not 0: the first row has no predecessor, and "+<1 ms" there would
  // report a gap that was never measured.
  it('has no delta for the first row even when startedAt is known', () => {
    expect(offsetMs(items, 0, started, 'delta')).toBeNull();
  });

  it('returns null for an unusable reference point', () => {
    expect(offsetMs(items, 2, null, 'session')).toBeNull();
    expect(offsetMs(items, 2, 'not-a-date', 'session')).toBeNull();
    expect(offsetMs(items, 9, started, 'session')).toBeNull();
  });

  // Out-of-order rows would otherwise render a negative offset; the old
  // component suppressed those and so does this.
  it('returns null rather than a negative offset', () => {
    const backwards = [ev('2026-08-11T14:17:05Z', 'a'), ev('2026-08-11T14:17:04Z', 'b')];
    expect(offsetMs(backwards, 1, started, 'delta')).toBeNull();
    expect(offsetMs(backwards, 0, '2026-08-11T14:17:06Z', 'session')).toBeNull();
  });
});

describe('rowCategory', () => {
  // The fold `rowKind` does NOT do: the filter's four buckets are coarser than
  // the five badges. An HTTP row keeps its HTTP badge but files under
  // "transaction", because that is the lane a user asking for "transactions"
  // means — and there is no separate HTTP chip for it to hide behind.
  it('folds http in with transactions and calls errors issues', () => {
    expect(rowCategory(http('/api/login'))).toBe('transaction');
    expect(rowCategory(tx('2026-08-11T14:17:14Z', '/checkout'))).toBe('transaction');
    expect(rowCategory(err('2026-08-11T14:17:14Z'))).toBe('issue');
  });

  it('separates $screen events from ordinary events', () => {
    expect(rowCategory(ev('2026-08-11T14:17:03Z', '$screen'))).toBe('navigation');
    expect(rowCategory(ev('2026-08-11T14:17:04Z', 'Onboarding menu'))).toBe('event');
  });

  // Totality is the property that matters: a row with no category is a row no
  // chip can ever show, and it would vanish the moment a filter is applied.
  it('gives every timeline row a category drawn from ROW_CATEGORIES', () => {
    const rows = [
      ev('2026-08-11T14:17:03Z', '$screen'),
      ev('2026-08-11T14:17:04Z', 'Onboarding menu'),
      err('2026-08-11T14:17:14Z'),
      tx('2026-08-11T14:17:14Z', '/checkout'),
      http('/api/login'),
    ];
    for (const row of rows) {
      expect(ROW_CATEGORIES).toContain(rowCategory(row));
    }
  });
});

describe('transactionOp', () => {
  it('reads the op off a transaction and nothing else', () => {
    expect(transactionOp(http('/api/login'))).toBe('http');
    expect(transactionOp(ev('2026-08-11T14:17:03Z', '$screen'))).toBeNull();
    expect(transactionOp(err('2026-08-11T14:17:14Z'))).toBeNull();
  });

  // A blank op is a real bucket, not a missing one. Dropping it would leave
  // those rows filterable by category but unreachable by op — visible when the
  // transaction chip is on, and impossible to isolate.
  it('normalizes a blank or padded op to the empty bucket', () => {
    expect(transactionOp(tx('2026-08-11T14:17:14Z', 'a', { op: '  db  ' }))).toBe('db');
    expect(transactionOp(tx('2026-08-11T14:17:14Z', 'a', { op: '   ' }))).toBe('');
    expect(transactionOp(tx('2026-08-11T14:17:14Z', 'a', { op: '' }))).toBe('');
  });
});

describe('filterTimeline', () => {
  const rows = [
    ev('2026-08-11T14:17:03Z', '$screen'),
    ev('2026-08-11T14:17:04Z', 'Onboarding menu'),
    tx('2026-08-11T14:17:05Z', '/checkout', { op: 'db' }),
    http('/api/login'),
    err('2026-08-11T14:17:14Z'),
  ];

  function filter(over: Partial<TimelineFilter> = {}): TimelineFilter {
    return { ...NO_TIMELINE_FILTER, ...over };
  }

  // An empty set means "no constraint", never "match nothing". It is what the
  // page starts with and what the All button restores, so the two cannot drift
  // apart, and no toggle sequence can land on a silently blank timeline.
  it('passes everything through when no category is selected', () => {
    expect(filterTimeline(rows, NO_TIMELINE_FILTER)).toEqual(rows);
    expect(isTimelineFiltered(NO_TIMELINE_FILTER)).toBe(false);
  });

  it('keeps only the selected category', () => {
    const only = filterTimeline(rows, filter({ categories: new Set(['navigation']) }));
    expect(only.map(rowTitle)).toEqual(['$screen']);
  });

  it('keeps the union of several selected categories', () => {
    const some = filterTimeline(rows, filter({ categories: new Set(['navigation', 'issue']) }));
    expect(some.map(rowCategory)).toEqual(['navigation', 'issue']);
  });

  it('reports itself as filtered once a category or op is chosen', () => {
    expect(isTimelineFiltered(filter({ categories: new Set(['event']) }))).toBe(true);
    expect(isTimelineFiltered(filter({ ops: new Set(['http']) }))).toBe(true);
  });

  it('lets every transaction through when no op is selected', () => {
    const txs = filterTimeline(rows, filter({ categories: new Set(['transaction']) }));
    expect(txs.map(transactionOp)).toEqual(['db', 'http']);
  });

  it('narrows transactions to the selected ops', () => {
    const only = filterTimeline(
      rows,
      filter({ categories: new Set(['transaction']), ops: new Set(['http']) }),
    );
    expect(only.map(transactionOp)).toEqual(['http']);
  });

  // The op set describes transactions and only transactions. If it also gated
  // the other lanes, turning on an op chip would silently empty the issue and
  // navigation rows the user had explicitly asked to keep.
  it('applies the op set to transactions without touching other categories', () => {
    const mixed = filterTimeline(
      rows,
      filter({ categories: new Set(['transaction', 'issue']), ops: new Set(['http']) }),
    );
    expect(mixed.map(rowCategory)).toEqual(['transaction', 'issue']);
  });

  it('isolates the blank-op bucket', () => {
    const blank = tx('2026-08-11T14:17:06Z', 'anon', { op: '' });
    const only = filterTimeline(
      [...rows, blank],
      filter({ categories: new Set(['transaction']), ops: new Set(['']) }),
    );
    expect(only).toEqual([blank]);
  });
});

describe('categoryCounts / opCounts', () => {
  const rows = [
    ev('2026-08-11T14:17:03Z', '$screen'),
    ev('2026-08-11T14:17:07Z', '$screen'),
    ev('2026-08-11T14:17:04Z', 'Onboarding menu'),
    tx('2026-08-11T14:17:05Z', '/checkout', { op: 'db' }),
    http('/api/login'),
    http('/api/me'),
  ];

  // Zero is a fact worth rendering: "this session had no issues" is what makes
  // the chip disableable instead of a dead control that filters to nothing.
  it('reports a count for every category, zeros included', () => {
    expect(categoryCounts(rows)).toEqual({
      navigation: 2,
      transaction: 3,
      event: 1,
      issue: 0,
    });
  });

  it('counts ops over transactions only, most frequent first', () => {
    expect(opCounts(rows)).toEqual([
      { op: 'http', count: 2 },
      { op: 'db', count: 1 },
    ]);
  });

  it('breaks a count tie by op name', () => {
    const tied = [
      tx('2026-08-11T14:17:05Z', 'a', { op: 'ui' }),
      tx('2026-08-11T14:17:06Z', 'b', { op: 'db' }),
      tx('2026-08-11T14:17:07Z', 'c', { op: '' }),
    ];
    expect(opCounts(tied).map((o) => o.op)).toEqual(['', 'db', 'ui']);
  });
});
