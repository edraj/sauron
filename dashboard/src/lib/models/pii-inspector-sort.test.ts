import { describe, expect, it, vi } from 'vitest';
import {
  FINDING_DEFAULT_SORT,
  MASK_DEFAULT_SORT,
  SCAN_DEFAULT_SORT,
  findingAccessor,
  maskActionAccessor,
  scanAccessor,
} from './pii-inspector-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { FindingView } from './inspector-findings';
import type { InspectorMaskAction, InspectorScan } from './index';

/**
 * Defaults are CONSTANTS, never derived from another field, and any field a
 * test does not distinguish ties across that test's rows — so an accessor
 * reading a neighbour either collates differently or collapses to input order,
 * and input order is never the expected order.
 */
function finding(over: Partial<FindingView> & { id: string }): FindingView {
  return {
    app_id: 'app',
    environment_id: null,
    env_scope: 'enrollment',
    source_table: 'events',
    source_column: 'payload',
    key_path: 'constant.path',
    matched_key: 'email',
    detector: '',
    value_type: 'string',
    match_count: 5,
    match_count_exact: true,
    sample_preview: '***',
    partition_kind: 'ranged',
    last_seen_at: '2026-05-01T00:00:00Z',
    ...over,
  };
}

const findOrder = (rows: FindingView[], key: string, dir: SortDir): string[] =>
  sortRows(rows, findingAccessor(key), dir).map((f) => f.id);

describe('findingAccessor', () => {
  it('orders Matches by the count, not by the "at least N" text the cell renders', () => {
    // `formatMatchCount` renders an inexact count as "at least 1,234". As TEXT
    // every inexact row collates together under "a" whatever its size, and
    // "1,234" sorts before "999". The numbers say something different.
    const rows = [
      finding({ id: 'small-exact', match_count: 999, match_count_exact: true }),
      finding({ id: 'big-inexact', match_count: 1234, match_count_exact: false }),
      finding({ id: 'mid-inexact', match_count: 1000, match_count_exact: false }),
    ];
    expect(findOrder(rows, 'matches', 'desc')).toEqual([
      'big-inexact',
      'mid-inexact',
      'small-exact',
    ]);
    expect(findOrder(rows, 'matches', 'asc')).toEqual([
      'small-exact',
      'mid-inexact',
      'big-inexact',
    ]);
  });

  it('orders Path and Type by their own fields', () => {
    // Path order and type order disagree, so neither accessor can satisfy the
    // other's assertion.
    const rows = [
      finding({ id: 'a', key_path: 'user.email', value_type: 'array' }),
      finding({ id: 'b', key_path: 'billing.card', value_type: 'string' }),
      finding({ id: 'c', key_path: 'meta.token', value_type: 'object' }),
    ];
    expect(findOrder(rows, 'path', 'asc')).toEqual(['b', 'c', 'a']);
    expect(findOrder(rows, 'type', 'asc')).toEqual(['a', 'c', 'b']);
  });

  it('orders Last seen by instant and keeps a never-seen finding last both ways', () => {
    const rows = [
      finding({ id: 'older', last_seen_at: '2026-05-01T09:00:00Z' }),
      finding({ id: 'never', last_seen_at: null }),
      finding({ id: 'newer', last_seen_at: '2026-05-04T09:00:00Z' }),
    ];
    expect(findOrder(rows, 'last_seen', 'desc')).toEqual(['newer', 'older', 'never']);
    expect(findOrder(rows, 'last_seen', 'asc')).toEqual(['older', 'newer', 'never']);
  });

  it('falls back to Matches for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Path order runs opposite to match-count order, so a fallback to Path
    // would invert this.
    const rows = [
      finding({ id: 'few', match_count: 2, key_path: 'aaa' }),
      finding({ id: 'many', match_count: 90, key_path: 'zzz' }),
    ];
    expect(findOrder(rows, 'no-such-column', 'desc')).toEqual(['many', 'few']);
    expect(FINDING_DEFAULT_SORT).toEqual({ key: 'matches', dir: 'desc' });
    expect(String(warn.mock.calls[0]?.[0])).toContain('findings');
    warn.mockRestore();
  });
});

function scan(over: Partial<InspectorScan> & { id: string }): InspectorScan {
  return {
    policy_id: 'pol',
    org_id: 'org',
    trigger_type: 'manual',
    status: 'succeeded',
    coverage: 'full',
    coverage_note: '',
    window_from: '2026-05-01T00:00:00Z',
    window_to: '2026-05-02T00:00:00Z',
    units_total: 1,
    units_done: 1,
    rows_scanned: 100,
    findings_count: 3,
    findings_reaped_at: null,
    attempts: 1,
    cancel_requested_at: null,
    error: '',
    started_at: '2026-05-02T10:00:00Z',
    finished_at: '2026-05-02T10:05:00Z',
    created_at: '2026-05-02T09:59:00Z',
    ...over,
  };
}

const scanOrder = (rows: InspectorScan[], key: string, dir: SortDir): string[] =>
  sortRows(rows, scanAccessor(key), dir).map((s) => s.id);

describe('scanAccessor', () => {
  it('orders Rows scanned by the number, not by its grouped-digit text', () => {
    // The cell renders `toLocaleString()`, so 1000 shows as "1,000". As text
    // that sorts BEFORE "999"; as a number it does not.
    const rows = [
      scan({ id: 'k', rows_scanned: 1000 }),
      scan({ id: 'nine', rows_scanned: 999 }),
      scan({ id: 'm', rows_scanned: 1_000_000 }),
    ];
    expect(scanOrder(rows, 'rows_scanned', 'desc')).toEqual(['m', 'k', 'nine']);
    expect(scanOrder(rows, 'rows_scanned', 'asc')).toEqual(['nine', 'k', 'm']);
  });

  it('orders Findings independently of Rows scanned, in both directions', () => {
    // The two counts run in opposite directions, so reading the wrong one
    // inverts the answer.
    //
    // BOTH directions of Findings are asserted and neither may be dropped.
    // Descending alone happens to be the INPUT order, so every other column of
    // this table — `started`, `finished`, `status`, `coverage` — ties across
    // these two rows, collapses to input order, and satisfies it: with one
    // direction this was the only test pinning Findings and four wrong
    // accessors passed it. Asserting the reverse as well means a tying accessor
    // has to produce two different orders from one constant and cannot.
    //
    // Rows scanned needs no second direction for the same reason it did not
    // need one before: its expected order is NOT the input order, so a tie
    // already fails it. That contrast is the whole of rule R3 in two lines.
    const rows = [
      scan({ id: 'thorough', rows_scanned: 10, findings_count: 900 }),
      scan({ id: 'wide', rows_scanned: 9000, findings_count: 1 }),
    ];
    expect(scanOrder(rows, 'findings', 'desc')).toEqual(['thorough', 'wide']);
    expect(scanOrder(rows, 'findings', 'asc')).toEqual(['wide', 'thorough']);
    expect(scanOrder(rows, 'rows_scanned', 'desc')).toEqual(['wide', 'thorough']);
  });

  it('keeps a queued scan last under Started, in both directions', () => {
    // A queued scan has no `started_at` and its cell shows an em dash. It is
    // NOT back-filled from `created_at`, so it sorts as absent rather than
    // being ranked among rows that display an instant.
    const rows = [
      scan({ id: 'earlier', started_at: '2026-05-02T08:00:00Z' }),
      scan({ id: 'queued', status: 'queued', started_at: null, created_at: '2026-05-09T00:00:00Z' }),
      scan({ id: 'later', started_at: '2026-05-04T08:00:00Z' }),
    ];
    expect(scanOrder(rows, 'started', 'desc')).toEqual(['later', 'earlier', 'queued']);
    expect(scanOrder(rows, 'started', 'asc')).toEqual(['earlier', 'later', 'queued']);
  });

  it('orders Finished and Coverage by their own fields', () => {
    // Coverage is TEXT on purpose and is the one status-shaped column here that
    // a rank would not move: `full < partial` alphabetically is already `full <
    // partial` by completeness, so this assertion holds for both accessors —
    // which is exactly why the column was left alone rather than given a ladder
    // and a test that cannot fail. See `pii-inspector-sort.ts`.
    const rows = [
      scan({
        id: 'a',
        finished_at: '2026-05-02T12:00:00Z',
        status: 'succeeded',
        coverage: 'full',
      }),
      scan({ id: 'b', finished_at: null, status: 'failed', coverage: 'partial' }),
      scan({
        id: 'c',
        finished_at: '2026-05-01T12:00:00Z',
        status: 'cancelled',
        coverage: 'full',
      }),
    ];
    expect(scanOrder(rows, 'finished', 'desc')).toEqual(['a', 'c', 'b']);
    expect(scanOrder(rows, 'coverage', 'desc')).toEqual(['b', 'a', 'c']);
    expect(scanOrder(rows, 'coverage', 'asc')).toEqual(['a', 'c', 'b']);
  });

  it('orders Status by RANK, which shares no position with its spelling', () => {
    // All five states, and the two orders agree nowhere:
    //   text asc  → stop(cancelled), bad(failed), wait(queued), go(running),
    //               ok(succeeded)   — a deliberate stop leading, a failure third
    //   rank asc  → ok, stop, wait, go, bad     (least worth looking at first)
    //   rank desc → bad, go, wait, stop, ok
    // Every other field is the fixture's constant, so an accessor reading one
    // of those collapses to input order, which is neither answer.
    const rows = [
      scan({ id: 'go', status: 'running' }),
      scan({ id: 'bad', status: 'failed' }),
      scan({ id: 'ok', status: 'succeeded' }),
      scan({ id: 'wait', status: 'queued' }),
      scan({ id: 'stop', status: 'cancelled' }),
    ];
    expect(scanOrder(rows, 'status', 'asc')).toEqual(['ok', 'stop', 'wait', 'go', 'bad']);
    expect(scanOrder(rows, 'status', 'desc')).toEqual(['bad', 'go', 'wait', 'stop', 'ok']);
  });

  it('ranks a scan state this build has never heard of last in BOTH directions', () => {
    const rows = [
      scan({ id: 'new', status: 'paused' as InspectorScan['status'] }),
      scan({ id: 'bad', status: 'failed' }),
      scan({ id: 'ok', status: 'succeeded' }),
    ];
    expect(scanOrder(rows, 'status', 'asc')).toEqual(['ok', 'bad', 'new']);
    expect(scanOrder(rows, 'status', 'desc')).toEqual(['bad', 'ok', 'new']);
  });

  it('ranks every scan state — a sixth one fails to compile here', () => {
    // The `Record` annotation is the guard: the ladder is only
    // `readonly InspectorScan['status'][]`, which an incomplete ladder
    // satisfies, so widening the union has to break something that enumerates
    // it. This does.
    const expected: Record<InspectorScan['status'], number> = {
      succeeded: 0,
      cancelled: 1,
      queued: 2,
      running: 3,
      failed: 4,
    };
    const accessor = scanAccessor('status');
    for (const [status, rank] of Object.entries(expected)) {
      expect(accessor(scan({ id: status, status: status as InspectorScan['status'] }))).toBe(rank);
    }
  });

  it('falls back to the scans default column, naming the scans table in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const rows = [
      scan({ id: 'earlier', started_at: '2026-05-01T08:00:00Z', rows_scanned: 900 }),
      scan({ id: 'later', started_at: '2026-05-06T08:00:00Z', rows_scanned: 1 }),
    ];
    expect(scanOrder(rows, 'nope', 'desc')).toEqual(['later', 'earlier']);
    expect(SCAN_DEFAULT_SORT).toEqual({ key: 'started', dir: 'desc' });
    expect(String(warn.mock.calls[0]?.[0])).toContain('scans');
    warn.mockRestore();
  });
});

function mask(over: Partial<InspectorMaskAction> & { id: string }): InspectorMaskAction {
  return {
    org_id: 'org',
    app_id: 'app',
    kind: 'mask',
    finding_id: null,
    scan_id: null,
    targets: [{ table: 'events', column: 'payload', path: 'user.email' }],
    status: 'done',
    requested_by_email: 'constant@example.com',
    cancelled_by_email: '',
    cancelled_at: null,
    requested_at: '2026-06-01T10:00:00Z',
    previewed_at: null,
    confirmed_at: null,
    started_at: null,
    finished_at: null,
    confirm_source: 'ui',
    estimated_rows: 10,
    rows_scanned: 10,
    rows_masked: 10,
    cold_rows_skipped: 0,
    cold_boundary_at: null,
    phase: '',
    vacuum_advised: false,
    error: '',
    ...over,
  };
}

const maskOrder = (rows: InspectorMaskAction[], key: string, dir: SortDir): string[] =>
  sortRows(rows, maskActionAccessor(key), dir).map((a) => a.id);

describe('maskActionAccessor', () => {
  it('keeps a row with no requester email last under Who, in both directions', () => {
    // The server stores an unattributed action as an EMPTY STRING, and the
    // cell renders an em dash for it. Left as '' it would collate before every
    // real address and lead the ascending sort; it is absent, not smallest.
    const rows = [
      mask({ id: 'zoe', requested_by_email: 'zoe@example.com' }),
      mask({ id: 'nobody', requested_by_email: '' }),
      mask({ id: 'ana', requested_by_email: 'ana@example.com' }),
    ];
    expect(maskOrder(rows, 'who', 'asc')).toEqual(['ana', 'zoe', 'nobody']);
    expect(maskOrder(rows, 'who', 'desc')).toEqual(['zoe', 'ana', 'nobody']);
  });

  it('applies the same rule to Cancelled by', () => {
    const rows = [
      mask({ id: 'live', cancelled_by_email: '' }),
      mask({ id: 'stopped', cancelled_by_email: 'ops@example.com' }),
    ];
    expect(maskOrder(rows, 'cancelled_by', 'asc')).toEqual(['stopped', 'live']);
    expect(maskOrder(rows, 'cancelled_by', 'desc')).toEqual(['stopped', 'live']);
  });

  it('orders Targets by how many there are', () => {
    // The empty row is labelled `zero`, not `none`: `none` collated between
    // `one` and `three` and therefore reproduced the count order exactly, so
    // `targets: (a) => a.id` — the row label read as if it were a column —
    // satisfied the assertion below. Rule R2: the label is itself an accessor
    // target, and a mnemonic label is the likeliest one to collide.
    const rows = [
      mask({ id: 'one', targets: [{ table: 't', column: 'c', path: '' }] }),
      mask({
        id: 'three',
        targets: [
          { table: 't', column: 'a', path: '' },
          { table: 't', column: 'b', path: '' },
          { table: 't', column: 'c', path: '' },
        ],
      }),
      mask({ id: 'zero', targets: [] }),
    ];
    expect(maskOrder(rows, 'targets', 'asc')).toEqual(['zero', 'one', 'three']);
  });

  it('orders the two row counts independently of each other and of the row label', () => {
    // They run in opposite directions, so reading the wrong column inverts it.
    //
    // THREE rows, and neither the input order nor the id order is any of the
    // three expected orders. Two rows are not enough here, and the reason is
    // the counting bound: two rows admit only two orderings, the two count
    // columns take one each, so however the ids are spelled they reproduce one
    // of the two and `rows_masked: (a) => a.id` passes. The pair `hot` / `cold`
    // did exactly that. A third row is what buys an id order that is neither:
    //   ids asc          → cold, hot, mid
    //   rows_masked asc  → cold, mid, hot
    //   cold_skipped asc → hot, mid, cold
    //   input order      → mid, hot, cold   (what every tying accessor returns)
    //
    // Rows masked is asserted in BOTH directions and neither may be dropped:
    // descending alone is satisfied by any accessor that ties, because `when`,
    // `who`, `targets`, `status` and `cancelled_by` are all fixture constants.
    // Five wrong accessors shipped green on that.
    const rows = [
      mask({ id: 'mid', rows_masked: 40, cold_rows_skipped: 30 }),
      mask({ id: 'hot', rows_masked: 900, cold_rows_skipped: 1 }),
      mask({ id: 'cold', rows_masked: 2, cold_rows_skipped: 700 }),
    ];
    expect(maskOrder(rows, 'rows_masked', 'desc')).toEqual(['hot', 'mid', 'cold']);
    expect(maskOrder(rows, 'rows_masked', 'asc')).toEqual(['cold', 'mid', 'hot']);
    expect(maskOrder(rows, 'cold_skipped', 'desc')).toEqual(['cold', 'mid', 'hot']);
  });

  it('orders When by instant', () => {
    const rows = [
      mask({ id: 'older', requested_at: '2026-06-01T10:00:00Z' }),
      mask({ id: 'newer', requested_at: '2026-06-08T10:00:00Z' }),
    ];
    expect(maskOrder(rows, 'when', 'desc')).toEqual(['newer', 'older']);
  });

  it('orders Status by RANK — eight states whose spelling order is unrelated', () => {
    // Ids are the state names so the expectations read as the ladder itself.
    // Alphabetically the eight run cancelled, cancelling, done, failed,
    // pending, preview, previewed, running — which shares no position with
    // either assertion below, and `requested_at` is the fixture's constant so
    // an accessor reading `when` collapses to the input order, which is neither.
    const rows = [
      mask({ id: 'pending', status: 'pending' }),
      mask({ id: 'done', status: 'done' }),
      mask({ id: 'failed', status: 'failed' }),
      mask({ id: 'preview', status: 'preview' }),
      mask({ id: 'cancelling', status: 'cancelling' }),
      mask({ id: 'previewed', status: 'previewed' }),
      mask({ id: 'running', status: 'running' }),
      mask({ id: 'cancelled', status: 'cancelled' }),
    ];
    expect(maskOrder(rows, 'status', 'asc')).toEqual([
      'done',
      'cancelled',
      'preview',
      'previewed',
      'pending',
      'running',
      'cancelling',
      'failed',
    ]);
    expect(maskOrder(rows, 'status', 'desc')).toEqual([
      'failed',
      'cancelling',
      'running',
      'pending',
      'previewed',
      'preview',
      'cancelled',
      'done',
    ]);
  });

  it('ranks a mask state this build has never heard of last in BOTH directions', () => {
    const rows = [
      mask({ id: 'new', status: 'reverting' as InspectorMaskAction['status'] }),
      mask({ id: 'failed', status: 'failed' }),
      mask({ id: 'done', status: 'done' }),
    ];
    expect(maskOrder(rows, 'status', 'asc')).toEqual(['done', 'failed', 'new']);
    expect(maskOrder(rows, 'status', 'desc')).toEqual(['failed', 'done', 'new']);
  });

  it('ranks every mask state — a ninth one fails to compile here', () => {
    const expected: Record<InspectorMaskAction['status'], number> = {
      done: 0,
      cancelled: 1,
      preview: 2,
      previewed: 3,
      pending: 4,
      running: 5,
      cancelling: 6,
      failed: 7,
    };
    const accessor = maskActionAccessor('status');
    for (const [status, rank] of Object.entries(expected)) {
      expect(
        accessor(mask({ id: status, status: status as InspectorMaskAction['status'] })),
      ).toBe(rank);
    }
  });

  it('falls back to When, naming the audit table in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Requester order runs opposite to When order, so a fallback to Who would
    // invert this.
    const rows = [
      mask({ id: 'older', requested_at: '2026-06-01T10:00:00Z', requested_by_email: 'z@e.com' }),
      mask({ id: 'newer', requested_at: '2026-06-08T10:00:00Z', requested_by_email: 'a@e.com' }),
    ];
    expect(maskOrder(rows, 'nope', 'desc')).toEqual(['newer', 'older']);
    expect(MASK_DEFAULT_SORT).toEqual({ key: 'when', dir: 'desc' });
    expect(String(warn.mock.calls[0]?.[0])).toContain('audit');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    maskOrder([mask({ id: 'a' })], 'targets', 'asc');
    findOrder([finding({ id: 'a' })], 'path', 'asc');
    scanOrder([scan({ id: 'a' })], 'coverage', 'desc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
