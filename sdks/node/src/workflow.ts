/**
 * Workflow name/reason normalization — pure helpers, no state of their own.
 *
 * Unlike the browser SDK (`sdks/js/src/workflow.ts`), the active workflow is
 * NOT held in a module-level variable here: a module global would leak one
 * HTTP request's workflow into every other concurrent request's telemetry.
 * Instead it lives on the per-request {@link Scope} (`scope.data.workflow`),
 * isolated by `AsyncLocalStorage` exactly like `user`/`tags`/`breadcrumbs`
 * already are (see `scope.ts`). `getWorkflow()` below is a bare read of that
 * scope field — it takes no client and never throws, mirroring the existing
 * `getCurrentScope()`/`getGlobalScope()` getters.
 */
import { getCurrentScope } from './scope.js';
import type { ActiveWorkflow } from './types.js';

/** Cap on a workflow name, after trimming. */
export const WORKFLOW_NAME_MAX = 120;
/** Cap on a cancel reason. */
export const WORKFLOW_REASON_MAX = 120;

/**
 * Returns the trimmed name, or `null` when invalid (not a string, empty after
 * trimming, or over {@link WORKFLOW_NAME_MAX} characters). Trims BEFORE
 * checking length/emptiness so an all-whitespace or over-long-but-padded name
 * is rejected rather than silently truncated.
 */
export function normalizeWorkflowName(name: unknown): string | null {
  if (typeof name !== 'string') return null;
  const trimmed = name.trim();
  if (trimmed.length === 0 || trimmed.length > WORKFLOW_NAME_MAX) return null;
  return trimmed;
}

/**
 * Normalize a cancel reason: default to `'user'` for a non-string or
 * all-whitespace value, else trim and cap at {@link WORKFLOW_REASON_MAX}.
 * The internal `force`-supersede path also routes its literal `'superseded'`
 * reason through this, so every reason on the wire is consistently shaped.
 */
export function normalizeReason(reason: unknown): string {
  if (typeof reason !== 'string' || reason.trim().length === 0) return 'user';
  return reason.trim().slice(0, WORKFLOW_REASON_MAX);
}

/**
 * The active workflow on the CURRENT scope — the `AsyncLocalStorage` child
 * inside `withScope`/`runWithAsyncScope`, else the process-wide global scope —
 * or `null` if none. Client-agnostic: reflects scope state regardless of
 * whether an SDK client is initialized, same as `getCurrentScope()` itself.
 */
export function getWorkflow(): ActiveWorkflow | null {
  return getCurrentScope().data.workflow;
}
