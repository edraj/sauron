import { getClient } from '../client.js';
import type { Breadcrumb, Hint, Level } from '../types.js';
import { nowIso } from '../utils.js';

/** Partial breadcrumb — missing fields are filled with sensible defaults. */
export interface BreadcrumbInput {
  type?: string;
  category?: string;
  message?: string | null;
  level?: Level;
  timestamp?: string;
  data?: Record<string, unknown> | null;
}

/** Normalize a partial breadcrumb into the full wire shape. */
export function normalizeBreadcrumb(input: BreadcrumbInput): Breadcrumb {
  return {
    type: input.type ?? 'default',
    category: input.category ?? 'default',
    message: input.message ?? null,
    level: input.level ?? 'info',
    timestamp: input.timestamp ?? nowIso(),
    data: input.data ?? null,
  };
}

/**
 * Record a breadcrumb. Runs through `beforeBreadcrumb` (the PII escape hatch)
 * and is stored in the ring buffer — nothing is sent until an error is captured.
 */
export function addBreadcrumb(input: BreadcrumbInput, hint?: Hint): void {
  const client = getClient();
  if (!client) return;
  client.addBreadcrumb(normalizeBreadcrumb(input), hint);
}

/**
 * How a navigation moved the history stack. Shared vocabulary with the Flutter
 * SDK's `SauronNavigatorObserver`, so a trail reads the same whichever SDK sent
 * it. `remove` has no web equivalent and is never emitted here.
 */
export type NavigationOperation = 'push' | 'pop' | 'replace' | 'remove';

/**
 * Convenience helper for navigation breadcrumbs.
 *
 * `operation` is what the *browser* did, which is not always what the user
 * meant: `history.forward()` fires the same `popstate` as `history.back()` and
 * carries nothing to separate them, so both arrive here as `pop`.
 */
export function addNavigationBreadcrumb(
  from: string | null,
  to: string | null,
  operation: NavigationOperation = 'push',
): void {
  addBreadcrumb({
    type: 'navigation',
    category: 'history',
    level: 'info',
    message: null,
    data: { from, to, operation },
  });
}
