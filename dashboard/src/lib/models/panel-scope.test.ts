import { describe, it, expect } from 'vitest';
import { panelScopeNote, type PanelScope } from './panel-scope';

/** Nothing ignored — the shape a caller starts from. */
const AGREES: PanelScope = {
  ignoredFilters: 0,
  ignoresSearch: false,
  ignoresDateRange: false,
};

describe('panelScopeNote', () => {
  // The whole point of the caption is that it is not always there. A panel
  // fetched with the same query as the list below it has nothing to disclose,
  // and a permanent notice would train the reader to stop seeing it.
  it('says nothing when the panel and the list carry the same query', () => {
    expect(panelScopeNote(AGREES, 'these totals')).toBeNull();
  });

  it('names a single ignored chip in the singular', () => {
    expect(panelScopeNote({ ...AGREES, ignoredFilters: 1 }, 'these totals')).toBe(
      "The filter doesn't apply to these totals.",
    );
  });

  it('names several ignored chips in the plural', () => {
    expect(panelScopeNote({ ...AGREES, ignoredFilters: 3 }, 'this chart')).toBe(
      "The filters don't apply to this chart.",
    );
  });

  it('names the search box on its own', () => {
    expect(panelScopeNote({ ...AGREES, ignoresSearch: true }, 'this list')).toBe(
      "The search doesn't apply to this list.",
    );
  });

  it('names the date range on its own', () => {
    expect(panelScopeNote({ ...AGREES, ignoresDateRange: true }, 'these totals')).toBe(
      "The date range doesn't apply to these totals.",
    );
  });

  // The Issues tiles' worst case: `repo::issue_stats` takes no date predicate
  // at all and the route takes neither `filter` nor `q`, so with a chip, a
  // search and a narrowed range up, all three are being ignored at once.
  it('lists every ignored control, with a comma series and one "and"', () => {
    expect(
      panelScopeNote(
        { ignoredFilters: 2, ignoresSearch: true, ignoresDateRange: true },
        'these totals',
      ),
    ).toBe("The filters, search and date range don't apply to these totals.");
  });

  it('pairs two ignored controls with "and" and no comma', () => {
    expect(
      panelScopeNote(
        { ignoredFilters: 2, ignoresSearch: false, ignoresDateRange: true },
        'these totals',
      ),
    ).toBe("The filters and date range don't apply to these totals.");
  });

  // Verb agreement is decided by the LIST, not by its first entry: one chip
  // plus the search is still two things, so it is "don't" even though the
  // first noun is singular.
  it('uses "don\'t" for a list even when the first noun is singular', () => {
    expect(
      panelScopeNote(
        { ignoredFilters: 1, ignoresSearch: true, ignoresDateRange: false },
        'this chart',
      ),
    ).toBe("The filter and search don't apply to this chart.");
  });

  describe('a panel that applies one chip and ignores the rest', () => {
    // Events' volume chart: the page forwards the `name:eq` chip as the
    // `name` parameter, so the negative sentence would be false about it.
    it('states the one filter it does apply', () => {
      expect(
        panelScopeNote(
          { ...AGREES, ignoredFilters: 1, appliedFilterLabel: 'Event' },
          'this chart',
        ),
      ).toBe('Only the Event filter applies to this chart.');
    });

    it('covers an ignored search with the same sentence', () => {
      expect(
        panelScopeNote(
          { ...AGREES, ignoresSearch: true, appliedFilterLabel: 'Event' },
          'this chart',
        ),
      ).toBe('Only the Event filter applies to this chart.');
    });

    // "Only the Event filter applies" says nothing about dates, so a panel
    // that also dropped the range would be under-reporting. Unreachable from
    // either page today — the volume chart takes `since_days` — and guarded
    // anyway, because the failure is a caption that is quietly incomplete.
    it('still discloses an ignored date range', () => {
      expect(
        panelScopeNote(
          {
            ignoredFilters: 1,
            ignoresSearch: false,
            ignoresDateRange: true,
            appliedFilterLabel: 'Event',
          },
          'this chart',
        ),
      ).toBe("Only the Event filter applies to this chart — the date range doesn't.");
    });

    // With no chip and no search ignored there is nothing to contrast the
    // applied filter against, so the positive form would answer a question
    // nobody asked and bury the date range — the one fact that matters here.
    it('falls back to the plain sentence when only the date range is ignored', () => {
      expect(
        panelScopeNote(
          { ...AGREES, ignoresDateRange: true, appliedFilterLabel: 'Event' },
          'this chart',
        ),
      ).toBe("The date range doesn't apply to this chart.");
    });

    it('says nothing when the applied filter is the only one there is', () => {
      expect(panelScopeNote({ ...AGREES, appliedFilterLabel: 'Event' }, 'this chart')).toBeNull();
    });

    // `null` is what a page hands over when no `name:eq` chip is up, straight
    // from `filters.find(...)?.value ?? null` — it must read as "no applied
    // filter", not as a label.
    it('treats a null label as no applied filter', () => {
      expect(
        panelScopeNote(
          { ...AGREES, ignoredFilters: 1, appliedFilterLabel: null },
          'this chart',
        ),
      ).toBe("The filter doesn't apply to this chart.");
    });
  });

  it('ends on whichever subject the caller passes', () => {
    const scope = { ...AGREES, ignoredFilters: 2 };
    expect(panelScopeNote(scope, 'these totals')).toBe(
      "The filters don't apply to these totals.",
    );
    expect(panelScopeNote(scope, 'this list')).toBe("The filters don't apply to this list.");
  });
});
