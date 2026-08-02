import { describe, expect, it } from 'vitest';
import {
  clampConditions,
  describeSubscription,
  kindScopeTypes,
  kindSupportsEnvFilter,
  quietHoursLabel,
  selectionToSubscriptionScope,
  validateSubscription,
} from './notification-prefs';
import type { NotificationSubscription } from './index';

describe('selectionToSubscriptionScope', () => {
  it('accepts exactly one project or one app', () => {
    expect(
      selectionToSubscriptionScope({ org: false, projects: ['p1'], apps: [], envs: [] }),
    ).toEqual({ ok: true, scope_type: 'project', scope_id: 'p1' });
    expect(
      selectionToSubscriptionScope({ org: false, projects: [], apps: ['a1'], envs: [] }),
    ).toEqual({ ok: true, scope_type: 'app', scope_id: 'a1' });
  });

  it('rejects a multi-node selection', () => {
    // Subscriptions are one row per scope, not a collapsed grant set, so
    // grant-plan.ts's coverage-diff machinery is deliberately not reused.
    const r = selectionToSubscriptionScope({
      org: false,
      projects: ['p1'],
      apps: ['a1'],
      envs: [],
    });
    expect(r.ok).toBe(false);
  });

  it('rejects an org selection', () => {
    // One org tick would fan out to every app in the org.
    const r = selectionToSubscriptionScope({ org: true, projects: [], apps: [], envs: [] });
    expect(r.ok).toBe(false);
  });

  it('rejects a non-empty envs array rather than ignoring it', () => {
    // ScopeTree's env rows are ENROLLMENT ids; a subscription stores CATALOGUE
    // ids. Silently dropping them would put two id spaces in one form. Failing
    // loudly is what catches a regression that re-enables the level.
    const r = selectionToSubscriptionScope({
      org: false,
      projects: [],
      apps: ['a1'],
      envs: ['e1'],
    });
    expect(r.ok).toBe(false);
  });

  it('rejects an empty selection', () => {
    expect(
      selectionToSubscriptionScope({ org: false, projects: [], apps: [], envs: [] }).ok,
    ).toBe(false);
  });
});

describe('kind metadata', () => {
  it('uptime has no environment filter and is project-only', () => {
    expect(kindSupportsEnvFilter('uptime')).toBe(false);
    expect(kindScopeTypes('uptime')).toEqual(['project']);
  });

  it('the error kinds narrow by environment and accept both scope types', () => {
    for (const k of ['error_spike', 'error_new_issue', 'error_regression'] as const) {
      expect(kindSupportsEnvFilter(k)).toBe(true);
      expect(kindScopeTypes(k)).toEqual(['project', 'app']);
    }
  });
});

describe('clampConditions', () => {
  // These numbers are hardcoded on purpose and duplicate the backend's clamps
  // exactly. A mismatch is the drift this test exists to catch.
  it('matches the backend clamps', () => {
    expect(clampConditions('error_spike', { window_seconds: 5 }).window_seconds).toBe(300);
    expect(clampConditions('error_spike', { window_seconds: 999999 }).window_seconds).toBe(86400);
    expect(clampConditions('error_spike', { factor: 0.1 }).factor).toBe(1.5);
    expect(clampConditions('error_spike', { factor: 900 }).factor).toBe(100);
    expect(clampConditions('error_spike', { min_count: 0 }).min_count).toBe(1);
    expect(clampConditions('error_spike', { min_count: 9999999 }).min_count).toBe(100000);
  });

  it('applies the documented defaults', () => {
    const c = clampConditions('error_spike', {});
    expect(c.window_seconds).toBe(900);
    expect(c.factor).toBe(3);
    expect(c.min_count).toBe(10);
    expect(c.level).toBeNull();
    expect(clampConditions('error_new_issue', {}).level).toBe('error');
    expect(clampConditions('error_regression', {}).level).toBe('error');
  });

  it('rejects a non-finite factor', () => {
    expect(clampConditions('error_spike', { factor: Number.NaN }).factor).toBe(3);
    expect(clampConditions('error_spike', { factor: Number.POSITIVE_INFINITY }).factor).toBe(3);
  });
});

describe('quietHoursLabel', () => {
  it('renders a window with its effective zone', () => {
    expect(quietHoursLabel(1320, 360, 'Europe/Paris')).toBe('22:00 – 06:00 (Europe/Paris)');
    expect(quietHoursLabel(null, null, 'UTC')).toBe('Always on');
    expect(quietHoursLabel(1320, null, 'UTC')).toBe('Always on');
  });
});

describe('validateSubscription', () => {
  it('enumerates every reason the save button is disabled', () => {
    expect(
      validateSubscription({
        kind: 'error_spike',
        selection: { org: false, projects: [], apps: [], envs: [] },
        environmentIds: [],
        conditions: {},
        delivery: 'immediate',
        throttleSeconds: 900,
        quietStartMin: null,
        quietEndMin: null,
        quietTz: 'UTC',
      }),
    ).toContain('Pick one project or one app.');

    expect(
      validateSubscription({
        kind: 'uptime',
        selection: { org: false, projects: [], apps: ['a1'], envs: [] },
        environmentIds: [],
        conditions: {},
        delivery: 'immediate',
        throttleSeconds: 900,
        quietStartMin: null,
        quietEndMin: null,
        quietTz: 'UTC',
      }),
    ).toContain('Uptime subscriptions are project-scoped.');

    expect(
      validateSubscription({
        kind: 'error_spike',
        selection: { org: false, projects: ['p1'], apps: [], envs: [] },
        environmentIds: [],
        conditions: {},
        delivery: 'immediate',
        throttleSeconds: 900,
        quietStartMin: 1320,
        quietEndMin: null,
        quietTz: 'UTC',
      }),
    ).toContain('Set both a quiet-hours start and end, or neither.');

    expect(
      validateSubscription({
        kind: 'error_spike',
        selection: { org: false, projects: ['p1'], apps: [], envs: [] },
        environmentIds: [],
        conditions: {},
        delivery: 'immediate',
        throttleSeconds: -1,
        quietStartMin: null,
        quietEndMin: null,
        quietTz: 'UTC',
      }),
    ).toContain('Throttle must be between 0 and 604800 seconds.');

    expect(
      validateSubscription({
        kind: 'error_spike',
        selection: { org: false, projects: ['p1'], apps: [], envs: [] },
        environmentIds: [],
        conditions: {},
        delivery: 'immediate',
        throttleSeconds: 900,
        quietStartMin: null,
        quietEndMin: null,
        quietTz: 'UTC',
      }),
    ).toEqual([]);
  });
});

describe('describeSubscription', () => {
  it('names the scope and falls back when the target is gone', () => {
    const base: NotificationSubscription = {
      id: 's1',
      scope_type: 'project',
      scope_id: 'p1',
      scope_name: 'Checkout',
      project_id: 'p1',
      kind: 'error_spike',
      enabled: true,
      disabled_reason: null,
      environment_ids: [],
      conditions: {},
      delivery: 'immediate',
      effective_delivery: 'immediate',
      throttle_seconds: 900,
      quiet_start_min: null,
      quiet_end_min: null,
      quiet_tz: 'UTC',
      created_at: '2026-08-01T00:00:00Z',
    };
    expect(describeSubscription(base)).toBe('Project “Checkout”');
    expect(describeSubscription({ ...base, scope_name: null })).toBe('Project (deleted)');
    expect(
      describeSubscription({ ...base, scope_type: 'app', scope_name: 'web' }),
    ).toBe('App “web”');
  });
});

describe('the subscription dialog never offers an org or an environment row', () => {
  it('a selection carrying either is refused by the model, not silently trimmed', () => {
    // `ScopeTree` gains `allowOrg`/`allowEnv` so the dialog cannot produce
    // these — but the model refuses them anyway, so a regression that
    // re-enables the level fails loudly at save time rather than storing an
    // enrollment id where a catalogue id belongs.
    expect(
      selectionToSubscriptionScope({ org: true, projects: [], apps: [], envs: [] }).ok,
    ).toBe(false);
    expect(
      selectionToSubscriptionScope({ org: false, projects: ['p'], apps: [], envs: ['e'] }).ok,
    ).toBe(false);
  });
});
