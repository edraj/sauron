/** Small dependency-free helpers shared across the SDK. */

/** SDK identity, embedded in every envelope header. */
export const SDK_NAME = 'sauron.javascript';
export const SDK_VERSION = '1.6.0';

/** The ambient global, regardless of environment (window / self / global). */
export function getGlobal(): typeof globalThis {
  return globalThis;
}

interface CryptoLike {
  randomUUID?: () => string;
  getRandomValues?: <T extends ArrayBufferView | null>(array: T) => T;
}

function getCrypto(): CryptoLike | undefined {
  const g = getGlobal() as { crypto?: CryptoLike };
  return g.crypto;
}

/** RFC-4122 v4 UUID, using Web Crypto when available. */
export function uuidv4(): string {
  const c = getCrypto();
  if (c && typeof c.randomUUID === 'function') {
    return c.randomUUID();
  }
  const bytes = new Uint8Array(16);
  if (c && typeof c.getRandomValues === 'function') {
    c.getRandomValues(bytes);
  } else {
    for (let i = 0; i < 16; i++) bytes[i] = Math.floor(Math.random() * 256);
  }
  // Set version (4) and variant (10xx) bits.
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex: string[] = [];
  for (let i = 0; i < 16; i++) hex.push(bytes[i].toString(16).padStart(2, '0'));
  const s = hex.join('');
  return `${s.slice(0, 8)}-${s.slice(8, 12)}-${s.slice(12, 16)}-${s.slice(16, 20)}-${s.slice(20)}`;
}

/** Current time as an ISO-8601 UTC string, e.g. `2026-07-12T10:30:00.123Z`. */
export function nowIso(): string {
  return new Date().toISOString();
}

/**
 * `JSON.stringify` that never throws: strips circular references, functions and
 * coerces bigint to string. Returns `"{}"` on catastrophic failure.
 */
export function safeStringify(value: unknown): string {
  const seen = new WeakSet<object>();
  try {
    return JSON.stringify(value, (_key, val) => {
      if (typeof val === 'bigint') return val.toString();
      if (typeof val === 'function') return undefined;
      if (typeof val === 'object' && val !== null) {
        if (seen.has(val)) return '[Circular]';
        seen.add(val);
      }
      return val;
    });
  } catch {
    return '{}';
  }
}

/** UTF-8 byte length of a string. */
export function byteLength(s: string): number {
  if (typeof TextEncoder !== 'undefined') {
    return new TextEncoder().encode(s).length;
  }
  let len = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x80) len += 1;
    else if (c < 0x800) len += 2;
    else if (c >= 0xd800 && c <= 0xdbff) {
      len += 4;
      i++; // surrogate pair
    } else len += 3;
  }
  return len;
}

/**
 * Full-jitter exponential backoff, capped. Attempt 0 => up to `baseMs`,
 * attempt n => up to `min(capMs, baseMs * 2^n)`, then a uniform random point
 * in `[0, that]`.
 */
export function computeBackoff(attempt: number, baseMs = 1000, capMs = 30000): number {
  const ceiling = Math.min(capMs, baseMs * Math.pow(2, Math.max(0, attempt)));
  return Math.round(Math.random() * ceiling);
}

/** Clamp a number into `[min, max]`. */
export function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** A tiny logger gated on `debug`. */
export function makeLogger(debug: boolean): {
  log: (...args: unknown[]) => void;
  warn: (...args: unknown[]) => void;
} {
  const noop = (): void => {};
  if (!debug || typeof console === 'undefined') {
    return { log: noop, warn: noop };
  }
  return {
    log: (...args: unknown[]) => console.log('[sauron]', ...args),
    warn: (...args: unknown[]) => console.warn('[sauron]', ...args),
  };
}

/**
 * Largest serialized `extra` a single transaction may carry, in bytes.
 *
 * Transactions are the highest-volume signal and they ship in BATCHED
 * envelopes, so one oversized payload does not fail alone — ingest rejects the
 * whole envelope past `INGEST_MAX_BODY_BYTES` (1 MiB by default) and every
 * unrelated span batched with it is lost. Since the motivating use of
 * transaction `extra` is request and response bodies, that is not a remote
 * hazard.
 */
export const MAX_TRANSACTION_EXTRA_BYTES = 16 * 1024;

/**
 * Cap a transaction's `extra`, substituting a marker when it is too large.
 *
 * Replaces the WHOLE map rather than trimming keys: a half-written JSON value
 * is worse than an honest marker, and per-key trimming would make the result
 * depend on key iteration order, which differs across the five SDKs. The
 * marker is deliberately readable on the dashboard — `_truncated` says data
 * was dropped rather than silently serving a short object that looks complete.
 *
 * Returns the input unchanged when it fits. A value that cannot be serialized
 * at all (a cycle, a BigInt) is replaced by the same marker with `_bytes: -1`,
 * because the alternative is throwing from inside `trackTransaction` — and an
 * SDK that crashes the app it is measuring is worse than one that drops a
 * payload.
 */
export function capTransactionExtra(
  extra: Record<string, unknown>,
  maxBytes = MAX_TRANSACTION_EXTRA_BYTES,
): Record<string, unknown> {
  let bytes: number;
  try {
    const json = JSON.stringify(extra);
    if (json === undefined) return { _truncated: true, _bytes: -1 };
    // `Blob`/`Buffer` are not available everywhere this SDK runs; UTF-8 byte
    // length is what the wire actually costs, so it is computed rather than
    // approximated by `json.length` (which undercounts every non-ASCII byte —
    // exactly what a response body full of user text contains).
    bytes = utf8Length(json);
  } catch {
    return { _truncated: true, _bytes: -1 };
  }
  if (bytes <= maxBytes) return extra;
  return { _truncated: true, _bytes: bytes };
}

/** UTF-8 byte length of a string, without depending on `TextEncoder`. */
function utf8Length(s: string): number {
  let n = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x80) n += 1;
    else if (c < 0x800) n += 2;
    else if (c >= 0xd800 && c <= 0xdbff) {
      // A surrogate PAIR is one 4-byte code point; advance past its low half
      // so it is not counted twice.
      n += 4;
      i++;
    } else n += 3;
  }
  return n;
}
