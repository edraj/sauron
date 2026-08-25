import { describe, expect, it } from 'vitest';
import { approx, rollupChip } from './freshness';

const NOW = new Date('2026-08-25T12:00:00Z');

describe('rollupChip', () => {
  it('is null while rollups are not serving the app', () => {
    expect(rollupChip(null, NOW)).toBeNull();
    expect(rollupChip({ ready: false, as_of: NOW.toISOString(), sessions_as_of: null }, NOW)).toBeNull();
    expect(rollupChip({ ready: true, as_of: null, sessions_as_of: null }, NOW)).toBeNull();
    expect(rollupChip({ ready: true, as_of: 'garbage', sessions_as_of: null }, NOW)).toBeNull();
  });

  it('labels a fresh watermark neutrally and carries the disclosure in the title', () => {
    const chip = rollupChip(
      { ready: true, as_of: '2026-08-25T11:59:30Z', sessions_as_of: null },
      NOW,
    );
    expect(chip).not.toBeNull();
    expect(chip?.tone).toBe('neutral');
    expect(chip?.label.length).toBeGreaterThan(0);
    expect(chip?.title).toContain('2026-08-25T11:59:30Z');
  });

  it('turns warning-toned when the watermark lags more than five minutes', () => {
    const chip = rollupChip(
      { ready: true, as_of: '2026-08-25T11:54:00Z', sessions_as_of: null },
      NOW,
    );
    expect(chip?.tone).toBe('warning');
  });
});

describe('approx', () => {
  it('prefixes only while rollups are active', () => {
    expect(approx('1,234', true)).toBe('≈1,234');
    expect(approx('1,234', false)).toBe('1,234');
  });
});
