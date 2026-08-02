import { describe, it, expect } from 'vitest';
import { groupFindings, formatMatchCount, findingBadges } from './inspector-findings';

const base = {
  id: 'f1',
  app_id: 'a1',
  environment_id: null,
  env_scope: 'no_env_column',
  source_table: 'issues',
  source_column: 'title',
  key_path: '',
  matched_key: 'email',
  detector: '',
  value_type: 'string',
  match_count: 3,
  match_count_exact: true,
  sample_preview: 'j…m',
  partition_kind: 'rollup',
  last_seen_at: '2026-08-01T00:00:00Z',
};

describe('formatMatchCount', () => {
  it('is exact when the scan was not truncated', () => {
    expect(formatMatchCount(41200, true)).toBe('41,200');
  });

  // Hitting INSPECTOR_MAX_PHASE2_ROWS_PER_UNIT makes every count a LOWER
  // BOUND. Rendering it as an exact number would be a quiet lie.
  it('says at least when the unit was truncated', () => {
    expect(formatMatchCount(200000, false)).toBe('at least 200,000');
  });
});

describe('findingBadges', () => {
  it('marks a rollup as recurring', () => {
    const b = findingBadges(base);
    expect(b.map((x) => x.label)).toContain('recurring');
    expect(b.find((x) => x.label === 'recurring')?.title).toMatch(/undone by the next event/i);
  });

  it('marks a default-partition finding as never ageing out', () => {
    const b = findingBadges({ ...base, partition_kind: 'default' });
    expect(b.map((x) => x.label)).toContain('never ages out');
  });

  it('marks a non-maskable table', () => {
    for (const table of ['devices', 'identities', 'workflows']) {
      const b = findingBadges({ ...base, source_table: table });
      expect(b.map((x) => x.label)).toContain('not maskable');
    }
  });

  it('distinguishes unattributed from no environment column', () => {
    expect(findingBadges({ ...base, env_scope: 'unattributed' }).map((x) => x.label)).toContain(
      'no environment',
    );
    expect(findingBadges({ ...base, env_scope: 'no_env_column' }).map((x) => x.label)).toContain(
      'app-wide table',
    );
  });
});

describe('groupFindings', () => {
  it('groups by table then column and sorts by match count', () => {
    const rows = [
      { ...base, id: 'a', source_table: 'error_events', source_column: 'extra', match_count: 1 },
      { ...base, id: 'b', source_table: 'error_events', source_column: 'extra', match_count: 9 },
      { ...base, id: 'c', source_table: 'issues', source_column: 'title', match_count: 5 },
    ];
    const groups = groupFindings(rows);
    expect(groups.map((g) => g.key)).toEqual(['error_events.extra', 'issues.title']);
    expect(groups[0].findings.map((f) => f.id)).toEqual(['b', 'a']);
    expect(groups[0].total).toBe(10);
  });
});
