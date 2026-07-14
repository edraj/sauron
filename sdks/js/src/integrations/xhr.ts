import { addBreadcrumb } from '../api/breadcrumbs.js';
import { nowIso } from '../utils.js';
import { isDsnRequest, isInternal, isWrapped, markWrapped, registerPatch, withInternal } from './instrument.js';

interface XhrMeta {
  method: string;
  url: string;
  startedAt: string;
}

type PatchedXhr = XMLHttpRequest & { __sauron_xhr__?: XhrMeta };

function addXhrBreadcrumb(meta: XhrMeta, status: number): void {
  withInternal(() => {
    addBreadcrumb({
      type: 'default',
      category: 'xhr',
      level: status >= 400 ? 'warning' : 'info',
      message: `${meta.method} ${meta.url}`,
      timestamp: meta.startedAt,
      data: { method: meta.method, url: meta.url, status_code: status || null },
    });
  });
}

/** Wrap `XMLHttpRequest.open`/`send` to leave a breadcrumb per request. */
export function installXhr(): void {
  const XHR = (globalThis as { XMLHttpRequest?: typeof XMLHttpRequest }).XMLHttpRequest;
  if (typeof XHR !== 'function' || !XHR.prototype) return;

  const proto = XHR.prototype;
  const originalOpen = proto.open;
  const originalSend = proto.send;
  if (isWrapped(originalOpen) || isWrapped(originalSend)) return;

  proto.open = markWrapped(function sauronXhrOpen(
    this: PatchedXhr,
    method: string,
    url: string | URL,
    ...rest: unknown[]
  ) {
    this.__sauron_xhr__ = {
      method: String(method ?? 'GET').toUpperCase(),
      url: String(url),
      startedAt: nowIso(),
    };
    return (originalOpen as (...a: unknown[]) => unknown).apply(this, [method, url, ...rest]);
  }) as typeof proto.open;

  proto.send = markWrapped(function sauronXhrSend(this: PatchedXhr, ...args: unknown[]) {
    const meta = this.__sauron_xhr__;
    if (meta && !isInternal() && !isDsnRequest(meta.url)) {
      const onLoadEnd = (): void => {
        try {
          addXhrBreadcrumb(meta, this.status);
        } catch {
          /* ignore */
        } finally {
          try {
            this.removeEventListener('loadend', onLoadEnd);
          } catch {
            /* ignore */
          }
        }
      };
      try {
        this.addEventListener('loadend', onLoadEnd);
      } catch {
        /* ignore */
      }
    }
    return (originalSend as (...a: unknown[]) => unknown).apply(this, args);
  }) as typeof proto.send;

  registerPatch('xhr', () => {
    proto.open = originalOpen;
    proto.send = originalSend;
  });
}
