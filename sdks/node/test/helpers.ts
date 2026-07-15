import { gunzipSync } from 'node:zlib';

/**
 * Decode a request body captured by a fake fetch, transparently un-gzipping when
 * the transport compressed it (`Content-Encoding: gzip`). Keeps the fake fetches
 * agnostic to whether a given envelope crossed the gzip threshold.
 */
export function bodyToString(init: {
  headers: Record<string, string>;
  body: string | Uint8Array;
}): string {
  if (init.headers['Content-Encoding'] === 'gzip') {
    return gunzipSync(init.body as Uint8Array).toString();
  }
  return init.body as string;
}
