import { describe, it, expect } from 'vitest';
import { dayTotals, divergingScale, rangeTotals, shouldShowStoreSection } from './stores';
import type { StoreDay } from '../api/stores';

describe('dayTotals', () => {
  it('treats an absent store as absent, not as zero', () => {
    const d: StoreDay = { day: '2026-08-01', google_play: { installs: 10, uninstalls: 1 } };
    expect(dayTotals(d)).toEqual({ installs: 10, uninstalls: 1 });
  });

  it('sums both stores when both reported', () => {
    const d: StoreDay = {
      day: '2026-08-01',
      google_play: { installs: 10, uninstalls: 1 },
      app_store: { installs: 5, uninstalls: 2 },
    };
    expect(dayTotals(d)).toEqual({ installs: 15, uninstalls: 3 });
  });
});

describe('divergingScale', () => {
  it('uses ONE scale across both directions', () => {
    // Scaling each half to its own maximum would put a 3-uninstall day level
    // with a 300-install day. UserActivityChart documents having made exactly
    // this mistake once already.
    const series: StoreDay[] = [
      { day: '2026-08-01', google_play: { installs: 300, uninstalls: 3 } },
    ];
    expect(divergingScale(series)).toBe(300);
  });

  it('sums stores within a day before taking the max', () => {
    const series: StoreDay[] = [
      {
        day: '2026-08-01',
        google_play: { installs: 100, uninstalls: 10 },
        app_store: { installs: 80, uninstalls: 5 },
      },
    ];
    expect(divergingScale(series)).toBe(180);
  });

  it('lets uninstalls set the scale when they exceed installs', () => {
    const series: StoreDay[] = [
      { day: '2026-08-01', google_play: { installs: 5, uninstalls: 400 } },
    ];
    expect(divergingScale(series)).toBe(400);
  });

  it('never returns zero, so an all-zero range cannot divide by zero', () => {
    expect(
      divergingScale([{ day: '2026-08-01', google_play: { installs: 0, uninstalls: 0 } }]),
    ).toBe(1);
    expect(divergingScale([])).toBe(1);
  });
});

describe('rangeTotals', () => {
  it('reports installs, uninstalls and net across the range', () => {
    const series: StoreDay[] = [
      { day: '2026-08-01', google_play: { installs: 100, uninstalls: 10 } },
      { day: '2026-08-02', app_store: { installs: 50, uninstalls: 5 } },
    ];
    expect(rangeTotals(series)).toEqual({ installs: 150, uninstalls: 15, net: 135 });
  });

  it('reports a negative net when a range lost more than it gained', () => {
    // Shrinking is a real outcome and must not be clamped to zero.
    const series: StoreDay[] = [
      { day: '2026-08-01', google_play: { installs: 10, uninstalls: 40 } },
    ];
    expect(rangeTotals(series).net).toBe(-30);
  });

  it('is all zeroes on an empty range', () => {
    expect(rangeTotals([])).toEqual({ installs: 0, uninstalls: 0, net: 0 });
  });
});

describe('shouldShowStoreSection', () => {
  const app = (envId: string | null) => ({ store_environment_id: envId });

  it('hides when no environment is designated', () => {
    expect(shouldShowStoreSection(app(null), 'env-1')).toBe(false);
  });

  it('hides when a different environment is selected', () => {
    expect(shouldShowStoreSection(app('env-1'), 'env-2')).toBe(false);
  });

  it('shows when the designated environment is selected', () => {
    expect(shouldShowStoreSection(app('env-1'), 'env-1')).toBe(true);
  });

  it('hides when no environment is selected at all', () => {
    // "All environments" is not the store environment. Showing the section
    // there would attach store numbers to a view that spans environments.
    expect(shouldShowStoreSection(app('env-1'), null)).toBe(false);
  });

  it('hides when neither is set, rather than matching null to null', () => {
    expect(shouldShowStoreSection(app(null), null)).toBe(false);
  });
});
