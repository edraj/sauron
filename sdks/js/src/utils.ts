/** Small dependency-free helpers shared across the SDK. */

/** SDK identity, embedded in every envelope header. */
export const SDK_NAME = 'sauron.javascript';
export const SDK_VERSION = '0.3.0';

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
