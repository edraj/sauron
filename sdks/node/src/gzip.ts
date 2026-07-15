/**
 * Optional gzip of the request body.
 *
 * The ingest accepts `Content-Encoding: gzip` (see the module docs in
 * `sauron-core/src/envelope.rs`). We only pay the CPU cost when the payload is
 * large enough to matter — bodies at or below `threshold` bytes go out
 * uncompressed. Node >= 18 ships `zlib`, so this adds no runtime dependency.
 */

import { gzipSync } from 'node:zlib';

/** The (possibly compressed) body plus the headers to merge into the request. */
export interface MaybeGzipResult {
  body: Buffer | string;
  headers: Record<string, string>;
}

/**
 * Gzip `body` when its byte length exceeds `threshold`; otherwise pass it
 * through untouched. Returns the encoding header only when compression happened.
 * A negative threshold disables compression entirely.
 */
export function maybeGzip(body: string, threshold: number): MaybeGzipResult {
  if (threshold < 0 || Buffer.byteLength(body) <= threshold) {
    return { body, headers: {} };
  }
  return { body: gzipSync(body), headers: { 'Content-Encoding': 'gzip' } };
}
