import { describe, expect, it, vi } from 'vitest';
import { pageRefresher } from './page-refresh.svelte';

describe('pageRefresher', () => {
  it('runs the body and reports busy around it', async () => {
    let sawBusy = false;
    const r = pageRefresher(async () => {
      sawBusy = r.busy;
    });
    expect(r.busy).toBe(false);
    await r.run();
    expect(sawBusy).toBe(true);
    expect(r.busy).toBe(false);
  });

  it('ignores a second click while one is running', async () => {
    let resolve!: () => void;
    const gate = new Promise<void>((res) => (resolve = res));
    const body = vi.fn().mockReturnValue(gate);
    const r = pageRefresher(body);

    const first = r.run();
    const second = r.run();
    resolve();
    await Promise.all([first, second]);

    // A second click wants the answer the first is already fetching; running
    // the body again would spend the server cooldown for nothing.
    expect(body).toHaveBeenCalledTimes(1);
  });

  it('clears busy even when the body throws', async () => {
    const r = pageRefresher(async () => {
      throw new Error('boom');
    });
    await expect(r.run()).resolves.toBeUndefined();
    // A stuck busy flag disables the button for the life of the page.
    expect(r.busy).toBe(false);
  });
});
