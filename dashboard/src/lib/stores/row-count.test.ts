import { describe, expect, it, vi } from 'vitest';
import { RowCount } from './row-count.svelte';

const envelope = (total: number) => ({ total, total_is_capped: false });

describe('RowCount.reload', () => {
  it('repeats the last load, past the same-key short-circuit', async () => {
    const rc = new RowCount();
    const fetcher = vi.fn().mockResolvedValueOnce(envelope(3)).mockResolvedValueOnce(envelope(5));
    await rc.load('k', fetcher);
    expect(rc.total).toBe(3);
    await rc.reload();
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(rc.total).toBe(5);
  });

  it('is a no-op before any load', async () => {
    const rc = new RowCount();
    await rc.reload();
    expect(rc.total).toBeNull();
  });
});
