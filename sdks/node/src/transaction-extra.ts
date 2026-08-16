/**
 * The size cap on a transaction's developer-supplied `extra`.
 *
 * Its own module rather than a private helper inside `client.ts` so the limit
 * and its behaviour are directly testable — the failure this guards against
 * (an oversized payload taking a whole batched envelope down with it) is not
 * visible from the outside once it happens.
 */

/**
 * Largest serialized `extra` a single transaction may carry, in bytes.
 *
 * Transactions are the highest-volume signal and they ship in BATCHED
 * envelopes, so one oversized payload does not fail alone — ingest rejects the
 * whole envelope past `INGEST_MAX_BODY_BYTES` (1 MiB by default) and every
 * unrelated span batched with it is lost. Since the motivating use of
 * transaction `extra` is request and response bodies, that is not a remote
 * hazard.
 *
 * Kept identical across all five SDKs. If it moves, it moves everywhere.
 */
export const MAX_TRANSACTION_EXTRA_BYTES = 16 * 1024;

/**
 * Cap a transaction's `extra`, substituting a marker when it is too large.
 *
 * Replaces the WHOLE map rather than trimming keys: a half-written JSON value
 * is worse than an honest marker, and per-key trimming would make the result
 * depend on key iteration order, which differs across the five SDKs. The marker
 * is deliberately readable on the dashboard — `_truncated` says data was
 * dropped rather than silently serving a short object that looks complete.
 *
 * A value that cannot be serialized at all (a cycle, a BigInt) becomes the same
 * marker with `_bytes: -1`, because the alternative is throwing from inside
 * `trackTransaction` — and an SDK that crashes the app it is measuring is worse
 * than one that drops a payload.
 */
export function capTransactionExtra(
  extra: Record<string, unknown>,
  maxBytes = MAX_TRANSACTION_EXTRA_BYTES,
): Record<string, unknown> {
  let bytes: number;
  try {
    const json = JSON.stringify(extra);
    if (json === undefined) return { _truncated: true, _bytes: -1 };
    // UTF-8 byte length, not `json.length`: the latter undercounts every
    // non-ASCII byte, which is exactly what a response body full of user text
    // is made of.
    bytes = Buffer.byteLength(json, 'utf8');
  } catch {
    return { _truncated: true, _bytes: -1 };
  }
  if (bytes <= maxBytes) return extra;
  return { _truncated: true, _bytes: bytes };
}
