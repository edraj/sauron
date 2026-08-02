import { describe, expect, it } from 'vitest';
import { filenameFromDisposition } from './download';

describe('filenameFromDisposition', () => {
  it('reads a quoted filename', () => {
    expect(
      filenameFromDisposition('attachment; filename="sauron-active-users-p1-20260501_20260508.csv"'),
    ).toBe('sauron-active-users-p1-20260501_20260508.csv');
  });

  it('reads an unquoted filename', () => {
    expect(filenameFromDisposition('attachment; filename=report.csv')).toBe('report.csv');
  });

  it('returns null when the header is absent or has no filename', () => {
    expect(filenameFromDisposition('')).toBeNull();
    expect(filenameFromDisposition('attachment')).toBeNull();
  });
});
