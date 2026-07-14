import { addBreadcrumb } from '../api/breadcrumbs.js';
import { nowIso } from '../utils.js';
import { isDsnRequest, isInternal, isWrapped, markWrapped, registerPatch, withInternal } from './instrument.js';

function resolveUrl(input: unknown): string {
  if (typeof input === 'string') return input;
  if (input && typeof input === 'object') {
    const withUrl = input as { url?: string; href?: string };
    if (typeof withUrl.url === 'string') return withUrl.url;
    if (typeof withUrl.href === 'string') return withUrl.href;
  }
  return String(input);
}

function resolveMethod(input: unknown, init: unknown): string {
  const fromInit = (init as { method?: string } | undefined)?.method;
  const fromReq = (input as { method?: string } | null)?.method;
  return String(fromInit ?? fromReq ?? 'GET').toUpperCase();
}

function addFetchBreadcrumb(
  method: string,
  url: string,
  status: number | null,
  startedAt: string,
): void {
  withInternal(() => {
    addBreadcrumb({
      type: 'default',
      category: 'fetch',
      level: status !== null && status >= 400 ? 'warning' : 'info',
      message: `${method} ${url}`,
      timestamp: startedAt,
      data: { method, url, status_code: status },
    });
  });
}

/**
 * Wrap `fetch` to leave a breadcrumb per request. Requests to the DSN host (our
 * own ingest traffic) and requests made while SDK code runs are skipped — this
 * is what prevents an infinite send→breadcrumb→send loop.
 */
export function installFetch(): void {
  const g = globalThis as { fetch?: typeof fetch };
  const original = g.fetch;
  if (typeof original !== 'function' || isWrapped(original)) return;

  const wrapped = markWrapped(function sauronFetch(
    this: unknown,
    input: RequestInfo | URL,
    init?: RequestInit,
  ): Promise<Response> {
    const url = resolveUrl(input);
    const shouldRecord = !isInternal() && !isDsnRequest(url);
    const method = resolveMethod(input, init);
    const startedAt = nowIso();

    const promise = original.call(this, input as RequestInfo, init);

    if (shouldRecord) {
      promise.then(
        (res) => addFetchBreadcrumb(method, url, res?.status ?? null, startedAt),
        () => addFetchBreadcrumb(method, url, null, startedAt),
      );
    }
    return promise;
  });

  g.fetch = wrapped;
  registerPatch('fetch', () => {
    g.fetch = original;
  });
}
