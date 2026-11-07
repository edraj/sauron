import { describe, expect, it } from 'vitest';
import { sectionState } from './section-state';

const view = (over: Partial<{ hasData: boolean; error: string | null; loading: boolean }> = {}) => ({
  hasData: false,
  error: null,
  loading: true,
  ...over,
});

describe('sectionState', () => {
  it('loads while the request is out and there is nothing to show', () => {
    expect(sectionState(view())).toBe('loading');
  });

  it('fails only once the request has settled with nothing to show', () => {
    expect(sectionState(view({ error: 'Gateway Timeout', loading: false }))).toBe('failed');
  });

  it('goes back to loading while a retry is out', () => {
    expect(sectionState(view({ error: 'Gateway Timeout', loading: true }))).toBe('loading');
  });

  it('keeps showing data it has, even when a revalidation failed', () => {
    expect(sectionState(view({ hasData: true, loading: false }))).toBe('content');
    expect(sectionState(view({ hasData: true, error: 'Gateway Timeout', loading: false }))).toBe('content');
  });

  it('holds the content back while waiting on another request', () => {
    expect(sectionState(view({ hasData: true, loading: false }), true)).toBe('loading');
  });
});
