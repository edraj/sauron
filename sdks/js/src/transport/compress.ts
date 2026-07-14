/**
 * Payload compression.
 *
 * Prefers the native `CompressionStream('gzip')`; lazily imports `fflate` only
 * as a fallback. Payloads under {@link MIN_COMPRESS_BYTES} are left as plain
 * strings so the caller omits the `Content-Encoding` header entirely.
 */

/** Below this size, compression overhead isn't worth it. */
export const MIN_COMPRESS_BYTES = 1024;

export interface CompressResult {
  body: Uint8Array | string;
  /** `'gzip'` only when the body was actually compressed. */
  encoding: 'gzip' | null;
}

interface CompressionStreamCtor {
  new (format: string): {
    readable: ReadableStream<Uint8Array>;
    writable: WritableStream<Uint8Array>;
  };
}

function getCompressionStream(): CompressionStreamCtor | undefined {
  const g = globalThis as { CompressionStream?: CompressionStreamCtor };
  return typeof g.CompressionStream === 'function' ? g.CompressionStream : undefined;
}

async function gzipViaStream(bytes: Uint8Array, CS: CompressionStreamCtor): Promise<Uint8Array> {
  const cs = new CS('gzip');
  const writer = cs.writable.getWriter();
  void writer.write(bytes);
  void writer.close();

  const reader = cs.readable.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (value) {
      chunks.push(value);
      total += value.length;
    }
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}

/** Compress a JSON string to gzip when large enough; otherwise pass it through. */
export async function maybeCompress(json: string): Promise<CompressResult> {
  const raw = new TextEncoder().encode(json);
  if (raw.length < MIN_COMPRESS_BYTES) {
    return { body: json, encoding: null };
  }

  const CS = getCompressionStream();
  if (CS) {
    try {
      const gz = await gzipViaStream(raw, CS);
      return { body: gz, encoding: 'gzip' };
    } catch {
      /* fall through to fflate */
    }
  }

  try {
    const { gzipSync } = await import('fflate');
    return { body: gzipSync(raw), encoding: 'gzip' };
  } catch {
    // No compression available — send uncompressed rather than drop the event.
    return { body: json, encoding: null };
  }
}
