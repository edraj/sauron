import { getClient } from '../client.js';
import { getSessionId } from '../identity.js';
import { getScreen } from '../screen.js';
import { parseError } from '../stacktrace/parse.js';
import type { Breadcrumb, ErrorItem, ExceptionValue, Frame, Hint, Level, Mechanism } from '../types.js';
import { nowIso, safeStringify } from '../utils.js';

const DEFAULT_MECHANISM: Mechanism = { type: 'generic', handled: true };

/** Copy any NON-EMPTY per-call tags/contexts/extra off the hint onto the item. */
function attachCallMeta(item: ErrorItem, hint?: Hint): void {
  if (hint?.tags && Object.keys(hint.tags).length > 0) item.tags = { ...hint.tags };
  if (hint?.contexts && Object.keys(hint.contexts).length > 0) item.contexts = { ...hint.contexts };
  if (hint?.extra && Object.keys(hint.extra).length > 0) item.extra = { ...hint.extra };
}

interface ExtractedError {
  type: string;
  value: string | null;
  stacktrace: Frame[];
}

function isErrorLike(err: unknown): err is Error {
  return (
    err instanceof Error ||
    (typeof err === 'object' &&
      err !== null &&
      'name' in err &&
      'message' in err &&
      typeof (err as { message?: unknown }).message === 'string')
  );
}

/** Reduce any thrown value into `{type, value, stacktrace}`. */
function extractError(err: unknown): ExtractedError {
  if (isErrorLike(err)) {
    const e = err as Error;
    return {
      type: e.name || 'Error',
      value: e.message ?? '',
      stacktrace: parseError(e),
    };
  }
  if (typeof err === 'string') {
    return { type: 'Error', value: err, stacktrace: [] };
  }
  if (typeof err === 'object' && err !== null) {
    const ctor = (err as { constructor?: { name?: string } }).constructor;
    return {
      type: ctor?.name ?? 'Object',
      value: safeStringify(err),
      stacktrace: [],
    };
  }
  return { type: typeof err, value: String(err), stacktrace: [] };
}

/** Build an error item from a thrown value plus the current breadcrumb trail. */
export function buildErrorItem(err: unknown, breadcrumbs: Breadcrumb[], hint?: Hint): ErrorItem {
  const extracted = extractError(err);
  const mechanism = (hint?.mechanism as Mechanism | undefined) ?? DEFAULT_MECHANISM;
  const level = (hint?.level as Level | undefined) ?? 'error';
  const fingerprint = (hint?.fingerprint as string[] | null | undefined) ?? null;

  const exception: ExceptionValue = {
    type: extracted.type,
    value: extracted.value,
    mechanism,
    stacktrace: extracted.stacktrace,
  };

  const item: ErrorItem = {
    type: 'error',
    timestamp: nowIso(),
    level,
    exception,
    breadcrumbs,
    fingerprint,
    session_id: getSessionId(),
    screen: (hint?.screen as string | undefined) ?? getScreen(),
  };
  attachCallMeta(item, hint);
  return item;
}

/** Capture an exception (or any thrown value) as an error item. */
export function captureException(err: unknown, hint?: Hint): void {
  const client = getClient();
  if (!client) return;
  const breadcrumbs = client.getScope().getBreadcrumbs();
  const fullHint: Hint = { ...hint, originalException: err };
  const item = buildErrorItem(err, breadcrumbs, fullHint);
  client.captureItem(item, fullHint);
}

/**
 * Capture a plain message as an error item at the given `level` (default info).
 *
 * The item carries NO `exception` block — a message is not an exception, and the
 * backend's `ExceptionInfo.ty` is a non-`Option` `String` with no serde default,
 * so the `{ type: null }` placeholder this used to send failed to deserialize
 * and 400'd the WHOLE envelope (`invalid_envelope`), taking every unrelated
 * error and event in the batch with it — silently, because every SDK treats a
 * 400 as a non-retryable drop.
 *
 * Omitting `exception` and setting `message` is python's shape. It also routes
 * server-side onto the `message\n<normalized message>` fingerprint branch, so
 * messages group by their text instead of piling into one bucket keyed by a
 * synthetic exception type.
 */
export function captureMessage(message: string, level: Level = 'info', hint?: Hint): void {
  const client = getClient();
  if (!client) return;
  const breadcrumbs = client.getScope().getBreadcrumbs();
  const item: ErrorItem = {
    type: 'error',
    timestamp: nowIso(),
    level,
    message,
    breadcrumbs,
    fingerprint: (hint?.fingerprint as string[] | null | undefined) ?? null,
    session_id: getSessionId(),
    screen: getScreen(),
  };
  attachCallMeta(item, hint);
  client.captureItem(item, hint);
}
