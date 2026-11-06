import { describe, expect, it } from 'vitest';
import { purgeKindName } from './purge-kinds';

describe('purgeKindName', () => {
  it('names every kind the purge crate defines (sauron-purge PurgeKind::slug)', () => {
    const slugs = [
      'error_events',
      'analytics_events',
      'transactions',
      'inspector',
      'issues',
      'sessions',
      'devices',
      'persons',
      'workflows',
    ];
    for (const slug of slugs) expect(purgeKindName(slug)).not.toContain('_');
    expect(purgeKindName('error_events')).toBe('Error events');
  });

  it('shows a kind the catalog has not learned yet as its slug, spaced', () => {
    expect(purgeKindName('crash_reports')).toBe('crash reports');
  });

  it('does not read inherited Object properties as kinds', () => {
    expect(purgeKindName('constructor')).toBe('constructor');
  });
});
