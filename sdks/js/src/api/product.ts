import { getClient } from '../client.js';
import { getSessionId } from '../identity.js';
import { getScreen, setScreenState } from '../screen.js';
import { mergeMeta } from '../scope.js';
import type { EventItem, IdentifyItem, TrackOptions, TransactionItem, TransactionOp } from '../types.js';
import { nowIso } from '../utils.js';

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
  const anonymousId = client.getAnonymousId();
  client.getScope().setUser({ id, traits });
  const item: IdentifyItem = {
    type: 'identify',
    distinct_id: id,
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
  return {
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
}

/** Enqueue a performance transaction item. */
export function trackTransaction(input: TransactionInput): void {
  const client = getClient();
  if (!client) return;
  const item = buildTransactionItem(input, client.getDistinctId(), getSessionId());
  client.captureItem(item);
}
