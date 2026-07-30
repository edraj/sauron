/**
 * Current-workflow state. The SDK stamps `workflow_id`/`workflow_name` on
 * every event/error/transaction while a workflow is active, and emits the
 * reserved `$workflow_start` / `$workflow_end` / `$workflow_cancel` analytics
 * events around its lifecycle (see api/product.ts).
 */
import type { ActiveWorkflow } from './types.js';

/** Cap on a workflow name, after trimming. */
export const WORKFLOW_NAME_MAX = 120;
/** Cap on a cancel reason. */
export const WORKFLOW_REASON_MAX = 120;

let current: ActiveWorkflow | null = null;

/** The active workflow, or null if none. */
export function getWorkflow(): ActiveWorkflow | null {
  return current;
}

/** Set (or clear, with `null`) the active workflow. */
export function setWorkflowState(workflow: ActiveWorkflow | null): void {
  current = workflow;
}

/** Drop the in-memory value (tests + teardown). */
export function resetWorkflow(): void {
  current = null;
}

/** Returns the trimmed name, or null when invalid (empty or over the cap). */
export function normalizeWorkflowName(name: unknown): string | null {
  if (typeof name !== 'string') return null;
  const trimmed = name.trim();
  if (trimmed.length === 0 || trimmed.length > WORKFLOW_NAME_MAX) return null;
  return trimmed;
}

/** Normalize a cancel reason: default to `'user'`, else trim and cap. */
export function normalizeReason(reason: unknown): string {
  if (typeof reason !== 'string' || reason.trim().length === 0) return 'user';
  return reason.trim().slice(0, WORKFLOW_REASON_MAX);
}
