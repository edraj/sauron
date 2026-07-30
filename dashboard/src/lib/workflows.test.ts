import { describe, it, expect } from 'vitest';
import { completionRate, statusTone, formatDuration } from './workflows';

describe('completionRate', () => {
  it('is completed over started', () => {
    expect(completionRate({ started: 4, completed: 3 } as never)).toBeCloseTo(0.75);
  });
  it('is 0 rather than NaN when nothing started', () => {
    expect(completionRate({ started: 0, completed: 0 } as never)).toBe(0);
  });
});

describe('statusTone', () => {
  it('maps every status to a Badge tone', () => {
    expect(statusTone('completed')).toBe('success');
    expect(statusTone('active')).toBe('neutral');
    expect(statusTone('cancelled')).toBe('warning');
    expect(statusTone('abandoned')).toBe('error');
  });
});

describe('formatDuration', () => {
  it('renders an em dash for null', () => { expect(formatDuration(null)).toBe('—'); });
  it('renders sub-second in ms', () => { expect(formatDuration(850)).toBe('850ms'); });
  it('renders seconds with one decimal', () => { expect(formatDuration(2500)).toBe('2.5s'); });
  it('renders minutes and seconds', () => { expect(formatDuration(95000)).toBe('1m 35s'); });
});
