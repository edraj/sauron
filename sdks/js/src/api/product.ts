import { getClient } from '../client.js';
import { getSessionId } from '../identity.js';
import { getScreen, setScreenState } from '../screen.js';
import { mergeMeta } from '../scope.js';
import type {
  ActiveWorkflow,
  EventItem,
  IdentifyItem,
  TrackOptions,
  TransactionItem,
  TransactionOp,
  WorkflowResult,
} from '../types.js';
import { capTransactionExtra, makeLogger, nowIso, uuidv4 } from '../utils.js';
import {
  getWorkflow,
  normalizeReason,
  normalizeWorkflowName,
  resetWorkflow,
  setWorkflowState,
} from '../workflow.js';

/**
 * Record a product-analytics event (PostHog-style). The `distinct_id` is the
 * current user id when identified, otherwise a stable anonymous id.
 */
export function track(
  name: string,
  properties: Record<string, unknown> = {},
  options: TrackOptions = {},
): void {
  const client = getClient();
  if (!client) return;
  const scope = client.getScope();
  const item: EventItem = {
    type: 'event',
    name,
    distinct_id: client.getDistinctId(),
    session_id: getSessionId(),
    screen: options.screen ?? getScreen(),
    timestamp: nowIso(),
    properties: properties ?? {},
  };
  const tags = mergeMeta(scope.tags, options.tags);
  if (Object.keys(tags).length > 0) item.tags = tags;
  const contexts = mergeMeta(scope.contexts, options.contexts);
  if (Object.keys(contexts).length > 0) item.contexts = contexts;
  const extra = mergeMeta(scope.extra, options.extra);
  if (Object.keys(extra).length > 0) item.extra = extra;
  client.captureItem(item);
}

/**
 * Set the current screen. On an actual change, emits a `$screen` view event
 * (carrying the new screen) so dwell can be computed server-side.
 */
export function setScreen(name: string): void {
  if (!setScreenState(name)) return;
  track('$screen', { screen: name });
}

/**
 * Associate the current session with a known user. Emits an identify item that
 * links the prior anonymous id (if any) to the new distinct id.
 */
export function identify(id: string, traits: Record<string, unknown> = {}): void {
  const client = getClient();
  if (!client) return;
  // Coerce ONCE, use everywhere below. `IdentifyItem.distinct_id` is a
  // non-`Option` Rust `String` on the wire (`envelope.rs`), so a plain-JS
  // caller passing a number (`Sauron.identify(user.id)`, entirely possible —
  // nothing at runtime enforces the TS `id: string` signature) would send a
  // JSON number there. That fails to deserialize and rejects the WHOLE
  // envelope (`400 invalid_envelope`), taking every other item batched
  // alongside it — the identical failure class `CHANGELOG.md` 1.4.0 documents
  // for `captureMessage`'s `exception.type: null`.
  const distinctId = String(id);
  // `null` unless the anon id was actually used as a distinct_id in this
  // browser session, or when it belonged to a different person than the last
  // one who identified on this device (`prepareIdentify` mints a fresh anon id
  // first in that case, since the old one is already burned server-side).
  // `process_identify` inserts a permanent `identities(app_id, alias_id,
  // distinct_id)` row for any non-empty anonymous_id, and that row is now a
  // LIVE signal (the 000038 backfill reads it), so a speculative or
  // cross-user alias is a durable server-side mis-merge.
  const anonymousId = client.prepareIdentify(distinctId);
  client.getScope().setUser({ id: distinctId, traits });
  const item: IdentifyItem = {
    type: 'identify',
    distinct_id: distinctId,
    anonymous_id: anonymousId,
    traits: traits ?? {},
  };
  client.captureItem(item);
}

/** Loose (camelCase) input accepted by {@link trackTransaction}. */
export interface TransactionInput {
  name: string;
  op?: string;
  durationMs: number;
  status?: string | null;
  httpMethod?: string | null;
  httpStatus?: number | null;
  url?: string | null;
  /**
   * Flat string tags for this transaction.
   *
   * **Per-call only — the scope is NOT merged in**, which is the one place
   * transactions differ from `track()` and `captureException()`. Those two
   * merge `setTag`/`setExtra` defaults; a transaction carries only what its own
   * call site attached. Transactions are the highest-volume signal (one per
   * navigation and per HTTP call), so inheriting a global blob would write it
   * onto every row.
   */
  tags?: Record<string, string>;
  /**
   * Freeform JSON for this transaction — the request body, the response body,
   * an order id, a retry count.
   *
   * Per-call only, for the reason on {@link TransactionInput.tags}. Serialized
   * and capped at {@link MAX_TRANSACTION_EXTRA_BYTES}; past that the whole map
   * is replaced with `{ _truncated: true, _bytes: N }` so one large body cannot
   * take a batched envelope over the ingest limit and drop every span in it.
   *
   * Nothing here is scrubbed. `beforeSend` is the redaction seam.
   */
  extra?: Record<string, unknown>;
}

const TRANSACTION_OPS: readonly TransactionOp[] = [
  'navigation',
  'http',
  'resource',
  'screen_load',
  'custom',
];

/** Coerce a free-form op string to a known {@link TransactionOp}, else `custom`. */
function normalizeOp(op: string | undefined): TransactionOp {
  return op && (TRANSACTION_OPS as readonly string[]).includes(op)
    ? (op as TransactionOp)
    : 'custom';
}

/**
 * Build a wire-shaped transaction item from camelCase input. Pure — the caller
 * supplies the current identity so this stays testable without a client.
 */
export function buildTransactionItem(
  input: TransactionInput,
  distinctId: string | null,
  sessionId: string | null,
): TransactionItem {
  const item: TransactionItem = {
    type: 'transaction',
    name: input.name,
    op: normalizeOp(input.op),
    duration_ms: input.durationMs,
    status: input.status ?? null,
    http_method: input.httpMethod ?? null,
    http_status: input.httpStatus ?? null,
    url: input.url ?? null,
    distinct_id: distinctId,
    session_id: sessionId,
    timestamp: nowIso(),
  };
  // Per-call only — no `mergeMeta(scope.tags, …)` here, unlike `track()` above.
  // Omitted when empty so an app that never sets them is byte-identical on the
  // wire to before these fields existed.
  if (input.tags && Object.keys(input.tags).length > 0) item.tags = { ...input.tags };
  if (input.extra && Object.keys(input.extra).length > 0) {
    item.extra = capTransactionExtra({ ...input.extra });
  }
  return item;
}

/** Enqueue a performance transaction item. */
export function trackTransaction(input: TransactionInput): void {
  const client = getClient();
  if (!client) return;
  const item = buildTransactionItem(input, client.getDistinctId(), getSessionId());
  client.captureItem(item);
}

/* ----------------------------------------------------------------- workflow */

/**
 * Discards everything. Used as the logger until a real one can be built from
 * the client, so the `catch` blocks below can log unconditionally even when
 * the failure happened while acquiring the client itself.
 */
const NOOP_LOGGER = makeLogger(false);

/**
 * Emit the closing lifecycle event (`$workflow_end`/`$workflow_cancel`) for
 * `active` while it is STILL the active workflow (so it is stamped with the
 * workflow it closes), then clear the state.
 *
 * NEVER THROWS, and clears the state in a `finally`. That pairing is the whole
 * contract: callers report `ok` on the strength of it, and `ok` must mean the
 * workflow really is closed locally. If the emit could throw past this
 * function, the state would stay set while the caller was told the workflow
 * ended — every later signal would then be stamped with a workflow the app
 * considers finished, and it would never leave `active` server-side. A lost
 * lifecycle event is recoverable (the server materializes the rollup row from
 * whatever stamped events it does receive); a permanently desynced local state
 * is not.
 */
function emitWorkflowClose(
  active: ActiveWorkflow,
  eventName: '$workflow_end' | '$workflow_cancel',
  reason: string | undefined,
  logger: { warn: (...args: unknown[]) => void },
): void {
  const properties: Record<string, unknown> = {
    workflow_id: active.workflowId,
    workflow_name: active.name,
    duration_ms: Math.max(0, Date.now() - Date.parse(active.startedAt)),
  };
  if (eventName === '$workflow_cancel') {
    properties.reason = normalizeReason(reason);
  }
  try {
    track(eventName, properties);
  } catch (err) {
    logger.warn(`${eventName}: failed to emit the lifecycle event`, err);
  } finally {
    resetWorkflow();
  }
}

/**
 * Start a named workflow. `force: true` supersedes an already-active workflow
 * (emitting `$workflow_cancel` with `reason: 'superseded'` for it first);
 * otherwise an active workflow makes this a no-op returning `already_active`.
 *
 * The workflow id is a fresh client-generated UUID — the server rolls counters
 * up on `(app_id, workflow_id)` app-wide, so a deterministic or reused id would
 * merge counts from unrelated environments/sessions into one row.
 */
export function startWorkflow(name: string, options?: { force?: boolean }): WorkflowResult {
  let logger = NOOP_LOGGER;
  try {
    const client = getClient();
    if (!client || !client.isEnabled()) return { status: 'disabled' };
    logger = makeLogger(client.options.debug);

    const normalized = normalizeWorkflowName(name);
    if (!normalized) {
      logger.warn('startWorkflow: invalid name', name);
      return { status: 'invalid_name' };
    }

    const active = getWorkflow();
    if (active && !options?.force) {
      logger.warn(
        `startWorkflow("${normalized}"): "${active.name}" is already active; pass { force: true } to replace it`,
      );
      return { status: 'already_active' };
    }

    // Mint the replacement BEFORE superseding the old one, so nothing that
    // could throw sits between closing the old workflow and setting the new
    // one — otherwise the `catch` below could report `disabled` ("nothing
    // changed") having already closed the previous workflow.
    const workflow: ActiveWorkflow = {
      workflowId: uuidv4(),
      name: normalized,
      startedAt: nowIso(),
    };
    if (active) emitWorkflowClose(active, '$workflow_cancel', 'superseded', logger);

    // Set the state BEFORE emitting, so $workflow_start is itself stamped.
    setWorkflowState(workflow);
    try {
      track('$workflow_start', { workflow_id: workflow.workflowId, workflow_name: workflow.name });
    } catch (err) {
      // The workflow IS live and stamping IS active — reporting `disabled`
      // here would tell the caller nothing started and hand back no id, which
      // is the one thing that cannot be recovered. The server materializes the
      // rollup row from the first stamped event regardless, so a lost
      // `$workflow_start` costs only its own properties.
      logger.warn('startWorkflow: failed to emit $workflow_start', err);
    }
    return { status: 'ok', workflowId: workflow.workflowId };
  } catch (err) {
    // Only reachable before any state was touched, so `disabled` — documented
    // as "nothing changed" — is honest here.
    logger.warn('startWorkflow failed', err);
    return { status: 'disabled' };
  }
}

/** Shared precondition + close logic for `endWorkflow`/`cancelWorkflow`. */
function closeWorkflow(
  eventName: '$workflow_end' | '$workflow_cancel',
  name?: string,
  reason?: string,
): WorkflowResult {
  let logger = NOOP_LOGGER;
  try {
    const client = getClient();
    if (!client || !client.isEnabled()) return { status: 'disabled' };
    logger = makeLogger(client.options.debug);

    const active = getWorkflow();
    if (!active) return { status: 'not_active' };
    // A malformed explicit `name` normalizes to null, which never equals an
    // active name — so it reports `name_mismatch`, not `invalid_name`. That is
    // deliberate: the caller named a workflow that is not the active one.
    if (name !== undefined && normalizeWorkflowName(name) !== active.name) {
      logger.warn(`${eventName}: "${name}" does not match active workflow "${active.name}"`);
      return { status: 'name_mismatch' };
    }
    const workflowId = active.workflowId;
    // Cannot throw, and always clears the state — so `ok` below is truthful.
    emitWorkflowClose(active, eventName, reason, logger);
    return { status: 'ok', workflowId };
  } catch (err) {
    // As in startWorkflow: only reachable before any state was touched.
    logger.warn(`${eventName} failed`, err);
    return { status: 'disabled' };
  }
}

/**
 * End the active workflow (or the one named `name`, if given). Emits
 * `$workflow_end` with `duration_ms` and clears the state. A no-op returning
 * `not_active`/`name_mismatch` when the precondition fails.
 */
export function endWorkflow(name?: string): WorkflowResult {
  return closeWorkflow('$workflow_end', name);
}

/**
 * Cancel the active workflow (or the one named `name`, if given). Emits
 * `$workflow_cancel` with `duration_ms` and `reason` (default `'user'`, capped
 * at 120 chars) and clears the state.
 */
export function cancelWorkflow(name?: string, options?: { reason?: string }): WorkflowResult {
  return closeWorkflow('$workflow_cancel', name, options?.reason);
}
