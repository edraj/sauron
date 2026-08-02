import { describe, it, expect } from 'vitest';
import {
  UNREACHABLE_COPY,
  describeTarget,
  expandCompanionTargets,
  maskConfirmReady,
  csvFilename,
  parseKeyInput,
  createPolicyBlockedReason,
  defaultEnvEnrollmentId,
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

describe('parseKeyInput', () => {
  // The Policy tab's existing "add a key" form lowercases a single key before
  // sending it, because the backend lowercases at write and the matcher
  // compares lowercased. The create form takes a whole list at once and must
  // do the same, or the policy round-trips with keys the user did not type.
  it('lowercases every key', () => {
    expect(parseKeyInput('Email, Phone')).toEqual([
      { key: 'email', scope: 'any' },
      { key: 'phone', scope: 'any' },
    ]);
  });

  it('splits on commas, whitespace and newlines alike', () => {
    // Paste from a spec doc is the realistic input, not a tidy CSV.
    expect(parseKeyInput('email, phone\ntoken  password').map((k) => k.key)).toEqual([
      'email',
      'phone',
      'token',
      'password',
    ]);
  });

  it('drops empties and collapses separator runs', () => {
    expect(parseKeyInput(' , ,email,,  ,')).toEqual([{ key: 'email', scope: 'any' }]);
  });

  it('dedupes case-insensitively, keeping first occurrence', () => {
    // A duplicate is not a 400 — it is a policy that lists the same key twice
    // and reports doubled match counts.
    expect(parseKeyInput('email, EMAIL, Email').map((k) => k.key)).toEqual(['email']);
  });

  it('is empty for blank input', () => {
    expect(parseKeyInput('   \n  ')).toEqual([]);
    expect(parseKeyInput('')).toEqual([]);
  });
});

describe('createPolicyBlockedReason', () => {
  // Mirrors the backend's two hard 400s so the button explains itself instead
  // of round-tripping to an error toast.
  it('is null when a target and at least one key are chosen', () => {
    expect(createPolicyBlockedReason('app-1', parseKeyInput('email'), [])).toBeNull();
  });

  it('accepts a detector-only policy', () => {
    // normalize_matchers takes EITHER — keys or detectors.
    expect(createPolicyBlockedReason('app-1', [], ['email'])).toBeNull();
  });

  it('names the missing target', () => {
    expect(createPolicyBlockedReason('', parseKeyInput('email'), [])).toMatch(/target/i);
    expect(createPolicyBlockedReason(null, parseKeyInput('email'), [])).toMatch(/target/i);
  });

  it('explains that a matcher-less policy is a false negative, not just invalid', () => {
    // The backend rejects this precisely because a policy with neither scans
    // nothing and reports zero findings with full coverage. The UI must give
    // the same reason, not a generic "required field".
    const why = createPolicyBlockedReason('app-1', [], []);
    expect(why).toBeTruthy();
    expect(why).toMatch(/tracked key|detector/i);
  });
});

describe('defaultEnvEnrollmentId', () => {
  // Regression: the picker used to start on `''`, which matches no <option>,
  // so the select rendered BLANK while the submit path fell back to the first
  // enrollment — a policy created against an environment the operator was
  // never shown.
  it('prefers the default enrollment over document order', () => {
    expect(
      defaultEnvEnrollmentId([
        { id: 'staging-enrollment', is_default: false },
        { id: 'prod-enrollment', is_default: true },
      ]),
    ).toBe('prod-enrollment');
  });

  it('falls back to the first when none is marked default', () => {
    expect(
      defaultEnvEnrollmentId([
        { id: 'a', is_default: false },
        { id: 'b', is_default: false },
      ]),
    ).toBe('a');
  });

  it('is null when every environment is retired', () => {
    // A real state, not an impossible one — the scope must be disabled rather
    // than silently target something.
    expect(defaultEnvEnrollmentId([])).toBeNull();
  });
});
