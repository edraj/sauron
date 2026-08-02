import { describe, it, expect } from 'vitest';
import {
  UNREACHABLE_COPY,
  describeTarget,
  expandCompanionTargets,
  maskConfirmReady,
  csvFilename,
} from './inspector';

describe('UNREACHABLE_COPY', () => {
  // One source, rendered verbatim in the MaskDialog, in the Audit tab detail
  // and in the wiki — so support answers and the product cannot diverge.
  it('leads with the hot-Postgres headline', () => {
    expect(UNREACHABLE_COPY[0].headline).toBe(true);
    expect(UNREACHABLE_COPY[0].what).toMatch(/hot Postgres only/i);
  });

  it('carries all twelve enumerated rows beneath the headline', () => {
    // A dropped row is a promise the dialog stops making.
    expect(UNREACHABLE_COPY.filter((r) => !r.headline)).toHaveLength(12);
    const subjects = UNREACHABLE_COPY.map((r) => r.what.toLowerCase()).join(' | ');
    for (const must of [
      'cold parquet',
      'tier_hot_days',
      'redis ingest stream',
      'dlq',
      'breadcrumbs',
      'alert_events',
      'already-delivered',
      'event_users.properties',
      'devices',
      'symbolicated',
      'backups',
      'active-users',
    ]) {
      expect(subjects).toContain(must);
    }
  });

  it('never claims a mask is permanent or removed', () => {
    const all = UNREACHABLE_COPY.map((r) => `${r.what} ${r.why} ${r.bounded}`).join(' ');
    expect(all).not.toMatch(/permanently removed/i);
  });

  it('marks the active-users row as read-before-confirm', () => {
    const row = UNREACHABLE_COPY.find((r) => r.what.toLowerCase().includes('active-user'));
    expect(row?.readFirst).toBe(true);
  });
});

describe('describeTarget', () => {
  it('names a jsonb path', () => {
    expect(describeTarget({ table: 'error_events', column: 'extra', path: 'customer.email' })).toBe(
      'error_events.extra → customer.email',
    );
  });

  it('says whole value for a text column', () => {
    expect(describeTarget({ table: 'issues', column: 'title', path: '' })).toBe(
      'issues.title → the whole value',
    );
  });
});

describe('expandCompanionTargets', () => {
  // Mirrors the backend map so the dialog can describe the blast radius
  // BEFORE the server answers.
  it('expands error_events.title to the wire sources and issues.title', () => {
    const out = expandCompanionTargets({ table: 'error_events', column: 'title', path: '' });
    const pairs = out.map((t) => `${t.table}.${t.column}`);
    expect(pairs).toContain('error_events.title');
    expect(pairs).toContain('issues.title');
    expect(pairs).toContain('error_events.exception_value');
    expect(pairs).toContain('error_events.exception_type');
    expect(pairs).toContain('error_events.message');
  });

  // The path is relative to the COLUMN and `error_events.stacktrace` is an
  // array at its root, so the wildcard is bare — same convention as
  // `parse_mask_path` in Task 11 and `apply_mask_path` in Task 14.
  it('expands stacktrace to its symbolicated copy, keeping the path', () => {
    const out = expandCompanionTargets({
      table: 'error_events',
      column: 'stacktrace',
      path: '[*].abs_path',
    });
    expect(out).toContainEqual({
      table: 'error_events',
      column: 'stacktrace_symbolicated',
      path: '[*].abs_path',
    });
  });

  it('expands context to sessions.context for both event tables', () => {
    for (const table of ['error_events', 'analytics_events'] as const) {
      const out = expandCompanionTargets({ table, column: 'context', path: 'user.email' });
      expect(out).toContainEqual({ table: 'sessions', column: 'context', path: 'user.email' });
    }
  });

  it('expands everything else to itself', () => {
    const one = { table: 'error_events', column: 'extra', path: 'a.b' } as const;
    expect(expandCompanionTargets(one)).toEqual([one]);
  });
});

describe('maskConfirmReady', () => {
  const preview = { status: 'previewed', previewed_at: new Date().toISOString(), estimated_rows: 10 };

  it('is false for the wrong slug', () => {
    expect(maskConfirmReady('wrong', 'my-app-a1b2', preview, 900, 20000000)).toBe(false);
  });

  it('is true for the right slug on a fresh preview', () => {
    expect(maskConfirmReady('my-app-a1b2', 'my-app-a1b2', preview, 900, 20000000)).toBe(true);
  });

  it('trims whitespace but not case', () => {
    expect(maskConfirmReady('  my-app-a1b2 ', 'my-app-a1b2', preview, 900, 20000000)).toBe(true);
    expect(maskConfirmReady('MY-APP-A1B2', 'my-app-a1b2', preview, 900, 20000000)).toBe(false);
  });

  it('is false while the preview is still counting', () => {
    expect(
      maskConfirmReady('my-app-a1b2', 'my-app-a1b2', { status: 'preview', previewed_at: null, estimated_rows: 0 }, 900, 20000000),
    ).toBe(false);
  });

  it('is false once the preview is stale', () => {
    const old = { ...preview, previewed_at: new Date(Date.now() - 3600_000).toISOString() };
    expect(maskConfirmReady('my-app-a1b2', 'my-app-a1b2', old, 900, 20000000)).toBe(false);
  });

  it('is false above the row ceiling', () => {
    expect(
      maskConfirmReady('my-app-a1b2', 'my-app-a1b2', { ...preview, estimated_rows: 99 }, 900, 10),
    ).toBe(false);
  });
});

describe('csvFilename', () => {
  it('is stable and carries the scope and range', () => {
    expect(csvFilename('findings', 'my-app', '2026-07-01', '2026-08-01')).toBe(
      'sauron-inspector-findings_my-app_2026-07-01_2026-08-01.csv',
    );
  });
});
