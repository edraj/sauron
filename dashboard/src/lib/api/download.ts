import { api } from './client';

/**
 * Pull `filename` out of a `Content-Disposition` header value.
 *
 * Exported and pure so it can be tested: the download itself touches the DOM,
 * and there is no DOM test environment here.
 */
export function filenameFromDisposition(value: string): string | null {
  const quoted = /filename="([^"]+)"/.exec(value);
  if (quoted) return quoted[1];
  const bare = /filename=([^;]+)/.exec(value);
  if (bare) return bare[1].trim();
  return null;
}

/**
 * Fetch `url` as a Blob and hand it to the browser as a download.
 *
 * Goes through the SHARED `api` instance, not a bare axios call, so it keeps
 * the bearer header and the 401 refresh-and-replay — the replay path does
 * `api(original)` with the original config, so `responseType` survives it.
 *
 * `paramsSerializer: { indexes: null }` is load-bearing: axios 1.x's default
 * serializer renders an array as `key[]=a&key[]=b`, which `serde_html_form`'s
 * `Vec<String>` on the server does not accept. `indexes: null` produces the
 * repeated `key=a&key=b` form the backend actually parses.
 *
 * `fallbackFilename` is used when `Content-Disposition` is unreadable — in both
 * shipped topologies the dashboard origin is not the API origin, so the header
 * only reaches JS because the API's CORS layer exposes it. Callers build the
 * fallback from the same ids and effective dates the server uses, so the file
 * is correctly named even if that ever regresses.
 *
 * Error handling is deliberately absent: `client.ts` unwraps the Blob error
 * body before normalizing, so the caller's `errorMessage(err)` already reads
 * the real message.
 */
export async function downloadCsv(
  url: string,
  params: Record<string, unknown>,
  fallbackFilename: string,
): Promise<void> {
  const res = await api.get(url, {
    params,
    responseType: 'blob',
    paramsSerializer: { indexes: null },
  });
  const disposition = String(res.headers['content-disposition'] ?? '');
  const filename = filenameFromDisposition(disposition) ?? fallbackFilename;
  const href = URL.createObjectURL(res.data as Blob);
  try {
    const a = document.createElement('a');
    a.href = href;
    a.download = filename;
    a.rel = 'noopener';
    document.body.appendChild(a);
    a.click();
    a.remove();
  } finally {
    // The click starts the download synchronously, so revoking here is safe
    // and is the only place that runs on both the success and the throw path.
    URL.revokeObjectURL(href);
  }
}
