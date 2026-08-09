import { describe, expect, it } from 'vitest';
import { formatTimestamp } from './format';

describe('formatTimestamp', () => {
  // Built from local-time components so the assertion holds in any TZ the
  // suite runs in. Constructing from an ISO string with a Z suffix would make
  // the expected output depend on the runner's timezone.
  it('formats a local instant as yyyy-MM-DD HH:mm:ss', () => {
    const d = new Date(2026, 7, 6, 14, 5, 7); // 2026-08-06 14:05:07 local
    expect(formatTimestamp(d)).toBe('2026-08-06 14:05:07');
  });

  it('zero-pads single-digit month, day, hour, minute and second', () => {
    const d = new Date(2026, 0, 2, 3, 4, 5); // 2026-01-02 03:04:05 local
    expect(formatTimestamp(d)).toBe('2026-01-02 03:04:05');
  });

  it('uses a 24-hour clock', () => {
    const d = new Date(2026, 7, 6, 23, 0, 0);
    expect(formatTimestamp(d)).toBe('2026-08-06 23:00:00');
  });

  it('returns an em dash for null and undefined', () => {
    expect(formatTimestamp(null)).toBe('—');
    expect(formatTimestamp(undefined)).toBe('—');
  });

  it('returns an em dash for an unparseable value', () => {
    expect(formatTimestamp('not a date')).toBe('—');
  });

  it('accepts an ISO string', () => {
    const iso = new Date(2026, 7, 6, 14, 5, 7).toISOString();
    expect(formatTimestamp(iso)).toBe('2026-08-06 14:05:07');
  });
});
