// Pure row-shaping helpers for the Workflows feature — kept in their own
// `.ts` module (rather than inline in `WorkflowsList.svelte`) because there
// is no jsdom and no `@testing-library/svelte` in this project, so component
// tests are not possible; these are the pieces worth unit-testing.

import type { WorkflowRow, WorkflowStatus } from './models';

/** Fraction of started runs that completed, in `[0, 1]`. `0` (not `NaN`) when nothing started. */
export function completionRate(row: WorkflowRow): number {
  return row.started === 0 ? 0 : row.completed / row.started;
}

/** Maps an effective workflow status to the `Badge` tone that renders it. */
export function statusTone(status: WorkflowStatus): 'success' | 'neutral' | 'warning' | 'error' {
  switch (status) {
    case 'completed':
      return 'success';
    case 'active':
      return 'neutral';
    case 'cancelled':
      return 'warning';
    case 'abandoned':
      return 'error';
  }
}

/** Renders a duration for display: `'—'` for null, ms/s/m+s tiers otherwise. */
export function formatDuration(ms: number | null): string {
  if (ms === null) return '—';
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const totalSeconds = Math.round(ms / 1000);
  return `${Math.floor(totalSeconds / 60)}m ${totalSeconds % 60}s`;
}
