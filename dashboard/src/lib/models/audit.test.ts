import { describe, expect, it } from 'vitest';
import type { AuditEntry } from '../api/audit';
import {
  DEFAULT_RANGE,
  EMPTY_FILTERS,
  describeAction,
  describeScope,
  filtersFromQuery,
  filtersToQuery,
  formatFieldName,
  formatValue,
  isDefaultFilter,
  rangeStart,
  sameFilters,
  toApiFilters,
  withSelected,
} from './audit';

describe('describeAction', () => {
  it('reads as a sentence for every action the backend writes', () => {
    expect(describeAction('environment.create').label).toBe('Created environment');
    expect(describeAction('member.reset_password').label).toBe('Reset password for member');
    expect(describeAction('environment.rotate_key').label).toBe(
      'Rotated ingest key for environment',
    );
    expect(describeAction('role.update').label).toBe('Updated role');
    expect(describeAction('grant.delete').label).toBe('Deleted access grant');
    expect(describeAction('alert_channel.test').label).toBe('Sent test through alert channel');
  });

  it('marks destructive and credential actions so they do not read like a rename', () => {
    expect(describeAction('project.delete').tone).toBe('destructive');
    expect(describeAction('environment.retire').tone).toBe('destructive');
    expect(describeAction('tier_pin.release').tone).toBe('destructive');
    expect(describeAction('member.reset_password').tone).toBe('credential');
    expect(describeAction('environment.rotate_key').tone).toBe('credential');
    expect(describeAction('pii.reveal').tone).toBe('credential');
    expect(describeAction('project.create').tone).toBe('neutral');
  });

  it('degrades to the raw action rather than rendering undefined', () => {
    // A backend that ships a new action before the dashboard knows it must
    // still produce a readable cell.
    expect(describeAction('widget.frobnicate').label).toBe('widget.frobnicate');
    expect(describeAction('nodot').label).toBe('nodot');
    expect(describeAction('').label).toBe('');
    expect(describeAction('trailing.').label).toBe('trailing.');
    expect(describeAction('.leading').label).toBe('.leading');
  });
});

describe('formatValue', () => {
  it('renders absence as a dash, never as the word null', () => {
    // On a create, the whole `from` column is null; a column of the literal
    // string "null" reads as data rather than as absence.
    expect(formatValue(null)).toBe('—');
    expect(formatValue(undefined)).toBe('—');
  });

  it('distinguishes an empty string from a missing value', () => {
    expect(formatValue('')).toBe('(empty)');
    expect(formatValue([])).toBe('(none)');
  });

  it('renders booleans as words and arrays as lists', () => {
    expect(formatValue(true)).toBe('yes');
    expect(formatValue(false)).toBe('no');
    expect(formatValue(['issue:read', 'event:read'])).toBe('issue:read, event:read');
  });

  it('renders objects as JSON rather than [object Object]', () => {
    expect(formatValue({ scope_type: 'org' })).toBe('{"scope_type":"org"}');
  });

  it('renders zero and false rather than treating them as absent', () => {
    // A `revoked_sessions: 0` must show 0, not a dash — "revoked nothing" is
    // a meaningful outcome.
    expect(formatValue(0)).toBe('0');
    expect(formatValue(false)).toBe('no');
  });
});

describe('formatFieldName', () => {
  it('humanises snake_case', () => {
    expect(formatFieldName('interval_seconds')).toBe('Interval seconds');
    expect(formatFieldName('name')).toBe('Name');
  });
});

describe('describeScope', () => {
  const base: AuditEntry = {
    id: 'x',
    actor_id: null,
    actor_email: 'a@x.com',
    action: 'project.create',
    entity_type: 'project',
    entity_id: null,
    entity_name: '',
    project_id: null,
    project_name: '',
    app_id: null,
    app_name: '',
    environment_id: null,
    environment_name: '',
    changes: {},
    created_at: '2026-08-11T00:00:00Z',
    source: 'audit',
  };

  it('joins only the levels that are present', () => {
    expect(describeScope({ ...base, project_name: 'Acme' })).toBe('Acme');
    expect(describeScope({ ...base, project_name: 'Acme', app_name: 'checkout' })).toBe(
      'Acme / checkout',
    );
    expect(
      describeScope({
        ...base,
        project_name: 'Acme',
        app_name: 'checkout',
        environment_name: 'staging',
      }),
    ).toBe('Acme / checkout / staging');
  });

  it('is empty for org-level actions rather than showing stray separators', () => {
    expect(describeScope(base)).toBe('');
  });
});

describe('rangeStart', () => {
  const now = new Date('2026-08-11T12:00:00.000Z');

  it('resolves named ranges to an absolute lower bound', () => {
    expect(rangeStart('24h', now)).toBe('2026-08-10T12:00:00.000Z');
    expect(rangeStart('7d', now)).toBe('2026-08-04T12:00:00.000Z');
  });

  it('returns null for all time, meaning no lower bound rather than the epoch', () => {
    expect(rangeStart('all', now)).toBeNull();
  });
});

describe('filter URL round trip', () => {
  it('produces an empty query for the default view', () => {
    expect(filtersToQuery(EMPTY_FILTERS)).toBe('');
  });

  it('round-trips a filtered view', () => {
    const f = {
      ...EMPTY_FILTERS,
      range: '30d' as const,
      project_id: 'p1',
      actor_id: 'u1',
      action: 'role.update',
    };
    const back = filtersFromQuery(filtersToQuery(f));
    expect(back.range).toBe('30d');
    expect(back.project_id).toBe('p1');
    expect(back.actor_id).toBe('u1');
    expect(back.action).toBe('role.update');
    expect(back.app_id).toBeNull();
  });

  it('falls back to the default range on a bogus one', () => {
    // A hand-edited or stale URL must not produce an undefined range that
    // silently drops the time bound.
    expect(filtersFromQuery('range=last-tuesday').range).toBe(DEFAULT_RANGE);
    expect(filtersFromQuery('').range).toBe(DEFAULT_RANGE);
  });
});

describe('isDefaultFilter', () => {
  it('is true only when nothing narrows the feed', () => {
    expect(isDefaultFilter(EMPTY_FILTERS)).toBe(true);
    expect(isDefaultFilter({ ...EMPTY_FILTERS, actor_id: 'u1' })).toBe(false);
    expect(isDefaultFilter({ ...EMPTY_FILTERS, range: 'all' })).toBe(false);
  });
});

describe('toApiFilters', () => {
  it('resolves the range into a from bound and drops the range key', () => {
    const now = new Date('2026-08-11T12:00:00.000Z');
    const out = toApiFilters({ ...EMPTY_FILTERS, range: '24h' }, now);
    expect(out.from).toBe('2026-08-10T12:00:00.000Z');
    expect('range' in out).toBe(false);
  });

  it('sends no from bound for all time', () => {
    expect(toApiFilters({ ...EMPTY_FILTERS, range: 'all' }).from).toBeNull();
  });
});

describe('sameFilters', () => {
  it('ignores key order, so a pasted URL and the canonical encoding agree', () => {
    // The bug this exists to prevent: comparing encoded strings instead of
    // values made the URL→state and state→URL effects disagree forever, and
    // the page reloaded in a loop with the spinner stuck on.
    const pasted = filtersFromQuery('action=role.update&range=30d');
    const canonical = filtersFromQuery(filtersToQuery(pasted));
    expect(filtersToQuery(pasted)).not.toBe('action=role.update&range=30d');
    expect(sameFilters(pasted, canonical)).toBe(true);
  });

  it('treats null and undefined and absent as the same', () => {
    const a = { ...EMPTY_FILTERS, project_id: null };
    const b = { ...EMPTY_FILTERS, project_id: undefined };
    expect(sameFilters(a, b)).toBe(true);
  });

  it('still detects a real difference on every axis', () => {
    const base = EMPTY_FILTERS;
    expect(sameFilters(base, { ...base, range: 'all' })).toBe(false);
    expect(sameFilters(base, { ...base, project_id: 'p' })).toBe(false);
    expect(sameFilters(base, { ...base, app_id: 'a' })).toBe(false);
    expect(sameFilters(base, { ...base, environment_id: 'e' })).toBe(false);
    expect(sameFilters(base, { ...base, actor_id: 'u' })).toBe(false);
    expect(sameFilters(base, { ...base, action: 'role.update' })).toBe(false);
    expect(sameFilters(base, { ...base, entity_type: 'role' })).toBe(false);
  });

  it('round-trips a deep link without drift', () => {
    // The exact deep link the runtime drive caught restoring the wrong range.
    const f = filtersFromQuery('action=role.update&range=30d');
    expect(f.range).toBe('30d');
    expect(f.action).toBe('role.update');
    expect(sameFilters(f, filtersFromQuery(filtersToQuery(f)))).toBe(true);
  });
});

describe('withSelected', () => {
  const facets = [
    { id: 'p1', label: 'Alpha' },
    { id: 'p2', label: 'Beta' },
  ];

  it('pins a selected value that the facets do not (yet) contain', () => {
    // The loop this prevents: facets arrive one request AFTER the filters are
    // hydrated from the URL, so on a deep link the <select> has no matching
    // <option>, `bind:value` writes null back into the filter state, that
    // write retriggers the load, and the page never settles.
    const out = withSelected(facets, 'p9');
    expect(out).toHaveLength(3);
    expect(out[0]).toEqual({ id: 'p9', label: 'p9' });
  });

  it('does not duplicate a value the facets already contain', () => {
    expect(withSelected(facets, 'p1')).toHaveLength(2);
  });

  it('leaves the list alone when nothing is selected', () => {
    expect(withSelected(facets, null)).toBe(facets);
    expect(withSelected(facets, undefined)).toBe(facets);
    expect(withSelected(facets, '')).toBe(facets);
  });

  it('works while the facets are still empty, which is the deep-link case', () => {
    expect(withSelected([], 'role.update')).toEqual([
      { id: 'role.update', label: 'role.update' },
    ]);
  });

  it('uses the label function for the pinned entry', () => {
    const out = withSelected([], 'role.update', (v) => describeAction(v).label);
    expect(out[0].label).toBe('Updated role');
  });
});

describe('auth actions', () => {
  it('read as complete phrases with no dangling noun', () => {
    expect(describeAction('auth.login').label).toBe('Signed in');
    expect(describeAction('auth.login_failed').label).toBe('Failed sign-in');
    expect(describeAction('auth.logout').label).toBe('Signed out');
    expect(describeAction('auth.password_change').label).toBe('Changed own password');
  });

  it('marks failed sign-ins and password changes as credential events', () => {
    // A burst of failed sign-ins against one account is the thing worth
    // spotting in a wall of two hundred rows.
    expect(describeAction('auth.login_failed').tone).toBe('credential');
    expect(describeAction('auth.password_change').tone).toBe('credential');
    expect(describeAction('auth.login').tone).toBe('neutral');
  });

  it('degrades to the raw action for an unknown auth verb', () => {
    expect(describeAction('auth.teleported').label).toBe('auth.teleported');
  });
});

describe('include_auth', () => {
  it('is off by default, so the admin feed is not buried in logins', () => {
    expect(EMPTY_FILTERS.include_auth).toBe(false);
    expect(isDefaultFilter(EMPTY_FILTERS)).toBe(true);
    expect(isDefaultFilter({ ...EMPTY_FILTERS, include_auth: true })).toBe(false);
  });

  it('round-trips through the URL', () => {
    const on = { ...EMPTY_FILTERS, include_auth: true };
    expect(filtersToQuery(on)).toContain('include_auth=1');
    expect(filtersFromQuery(filtersToQuery(on)).include_auth).toBe(true);
    expect(filtersFromQuery('').include_auth).toBe(false);
  });

  it('is part of semantic filter equality', () => {
    expect(sameFilters(EMPTY_FILTERS, { ...EMPTY_FILTERS, include_auth: true })).toBe(false);
  });

  it('reaches the API payload', () => {
    expect(toApiFilters({ ...EMPTY_FILTERS, include_auth: true }).include_auth).toBe(true);
  });
});
