import { describe, expect, it } from 'vitest';
import { appendPageParams, predicateParams, searchParams } from './search';

/**
 * Sibling to `lib/api/scope.test.ts` and `lib/api/client.test.ts` rather than
 * living under `lib/models/`: this is wire-encoding logic and belongs beside
 * the clients that call it.
 */
describe('predicateParams', () => {
  it('repeats `filter` once per chip and sets the scalar keys', () => {
    const p = predicateParams({
      filters: ['status:eq:unresolved', 'level:eq:error'],
      q: 'boom',
      sinceDays: 30,
    });
    expect(p.getAll('filter')).toEqual(['status:eq:unresolved', 'level:eq:error']);
    expect(p.get('q')).toBe('boom');
    expect(p.get('since_days')).toBe('30');
  });

  it('carries `query`, so the language reaches the server at all', () => {
    // The whole point of S2c on the client: without this line every `query=`
    // the search bar builds is dropped on the floor and the server silently
    // answers the unfiltered list instead.
    expect(predicateParams({ query: 'is:unresolved boom' }).get('query')).toBe(
      'is:unresolved boom',
    );
  });

  it('drops empty free text rather than sending a search that never ran', () => {
    const p = predicateParams({ q: '', query: '' });
    expect(p.has('q')).toBe(false);
    expect(p.has('query')).toBe(false);
  });

  it('sends since_days=0 — `!= null`, not truthiness', () => {
    // 0 is a legitimate value the server clamps to 1; a truthiness test would
    // drop it and silently widen the window to the 3650d default.
    expect(predicateParams({ sinceDays: 0 }).get('since_days')).toBe('0');
  });

  it('emits NO page parameters', () => {
    // This is what keeps `/events/stats` counting over the same predicate the
    // list pages: the stats route is handed only this half.
    const p = predicateParams({ filters: ['a:eq:b'], q: 'x', query: 'y', sinceDays: 7 });
    for (const key of ['sort', 'cursor', 'limit', 'offset']) {
      expect(p.has(key), `stats request must not carry \`${key}\``).toBe(false);
    }
  });
});

describe('appendPageParams', () => {
  it('sets sort, cursor and limit onto the params it is given', () => {
    const p = appendPageParams(new URLSearchParams(), {
      sort: '-last_seen',
      cursor: 'abc123',
      limit: 50,
    });
    expect(p.get('sort')).toBe('-last_seen');
    expect(p.get('cursor')).toBe('abc123');
    expect(p.get('limit')).toBe('50');
  });

  it('omits what was not asked for', () => {
    const p = appendPageParams(new URLSearchParams(), {});
    expect([...p.keys()]).toEqual([]);
  });

  it('mutates and returns the same object, so callers can chain', () => {
    const p = new URLSearchParams();
    expect(appendPageParams(p, { limit: 1 })).toBe(p);
  });
});

describe('searchParams', () => {
  it('combines both halves', () => {
    const p = searchParams({
      filters: ['status:eq:unresolved'],
      query: 'level:error',
      sinceDays: 90,
      sort: 'first_seen',
      cursor: 'tok',
      limit: 100,
    });
    expect(p.getAll('filter')).toEqual(['status:eq:unresolved']);
    expect(p.get('query')).toBe('level:error');
    expect(p.get('since_days')).toBe('90');
    expect(p.get('sort')).toBe('first_seen');
    expect(p.get('cursor')).toBe('tok');
    expect(p.get('limit')).toBe('100');
  });

  // `offset` used to be unsendable: keyset paging replaced it and the server
  // ignored it. It is sendable again for ONE purpose — a jump to a numbered
  // page, which has no cursor because nobody has walked there. Stepping is
  // still keyset, and these three cases are what keeps the two apart.
  it('never sends `offset` alongside a cursor', () => {
    // The server drops the offset when a cursor is present, so a request
    // carrying both reads as "page 3" and answers as a keyset step. Refused
    // here rather than relied on there.
    const p = searchParams({ limit: 50, cursor: 'tok', offset: 100 });
    expect(p.get('cursor')).toBe('tok');
    expect(p.has('offset')).toBe(false);
  });

  it('sends `offset` for a cursorless jump', () => {
    const p = searchParams({ limit: 50, offset: 100 });
    expect(p.get('offset')).toBe('100');
    expect(p.has('cursor')).toBe(false);
  });

  it('omits a zero `offset` rather than stating the default', () => {
    const p = searchParams({ limit: 50, offset: 0 });
    expect(p.has('offset')).toBe(false);
  });

  it('produces a stable query string for an unfiltered first page', () => {
    expect(searchParams({ sinceDays: 3650, limit: 50 }).toString()).toBe(
      'since_days=3650&limit=50',
    );
  });
});
