import type { StoreDay } from '../api/stores';

// Pure helpers for the Overview store section. Kept out of the `.svelte` files
// so the arithmetic that decides bar heights — and the rule that decides
// whether the section appears at all — are directly testable.

/** One day's totals across both stores. */
export function dayTotals(d: StoreDay): { installs: number; uninstalls: number } {
  return {
    installs: (d.google_play?.installs ?? 0) + (d.app_store?.installs ?? 0),
    uninstalls: (d.google_play?.uninstalls ?? 0) + (d.app_store?.uninstalls ?? 0),
  };
}

/**
 * ONE scale for both halves of the diverging chart.
 *
 * The denominator is the largest single-day total in EITHER direction, so an
 * install bar and an uninstall bar of the same height mean the same number.
 * Scaling each half to its own maximum would make a 3-uninstall day as tall as
 * a 300-install day — the mistake `UserActivityChart` documents having already
 * made once.
 *
 * Floors at 1: an all-zero range would otherwise divide by zero.
 */
export function divergingScale(series: StoreDay[]): number {
  let max = 0;
  for (const d of series) {
    const t = dayTotals(d);
    max = Math.max(max, t.installs, t.uninstalls);
  }
  return Math.max(max, 1);
}

/** Range totals for the stat tiles. `net` may legitimately be negative. */
export function rangeTotals(series: StoreDay[]): {
  installs: number;
  uninstalls: number;
  net: number;
} {
  let installs = 0;
  let uninstalls = 0;
  for (const d of series) {
    const t = dayTotals(d);
    installs += t.installs;
    uninstalls += t.uninstalls;
  }
  return { installs, uninstalls, net: installs - uninstalls };
}

/**
 * The store section is visible only in the environment the admin designated as
 * the store build.
 *
 * The store data itself is app-wide — this gate is the entire mechanism by
 * which the designation means anything. Both values must be set and equal:
 * "all environments" (`null`) is not the store environment, and matching
 * `null === null` would show the section to every app that never configured
 * one.
 */
export function shouldShowStoreSection(
  app: { store_environment_id: string | null },
  currentEnvironmentId: string | null,
): boolean {
  return !!app.store_environment_id && app.store_environment_id === currentEnvironmentId;
}
