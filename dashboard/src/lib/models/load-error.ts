/**
 * Turning a failed load into a sentence a person can act on.
 *
 * Pages used to print whatever the failure carried — axios's "Request failed
 * with status code 504", or the backend's English text in an Arabic UI. The
 * reason is now one of a few human sentences picked by the KIND of failure,
 * and the raw text survives as `details` for whoever has to report it.
 *
 * Failures whose text is the point keep it (`reason: null`): a 400 names what
 * was wrong with the request, and a coded 503 like `release_rollup_pending` is
 * a state the page explains in its own words.
 */
import { isNormalizedError } from '../api/client';
import { t, type MessageKey } from '../i18n';

export interface LoadErrorDescription {
  /** Catalog key of the human sentence, or `null` to show `details` as-is. */
  reason: MessageKey | null;
  /** The raw message, for the muted "Details" line. */
  details: string;
}

/** Server codes a page explains in its own words; never paraphrased here. */
const SELF_EXPLAINED_CODES = new Set(['release_rollup_pending']);

/**
 * A request that gave up waiting: axios's own codes, and the coded 503 the API
 * sends when a query runs past its budget (`query_timeout`, error.rs) — the
 * most common real failure on a large dataset.
 */
const TIMEOUT_CODES = new Set(['ECONNABORTED', 'ETIMEDOUT', 'timeout', 'query_timeout']);

/** axios's codes for a request that never reached a server. */
const NETWORK_CODES = new Set(['ERR_NETWORK', 'network_error', 'ECONNREFUSED', 'ENOTFOUND', 'ECONNRESET']);

interface Facts {
  status: number | null;
  code: string | null;
  message: string;
  isNetwork: boolean;
}

function factsOf(err: unknown): Facts | string {
  if (typeof err === 'string') return err;
  if (isNormalizedError(err)) {
    return { status: err.status || null, code: err.code || null, message: err.message, isNetwork: err.isNetwork };
  }
  // A CachedView (or anything shaped like one): error / errorStatus / errorCode.
  if (err && typeof err === 'object' && 'errorStatus' in err) {
    const v = err as { error?: unknown; errorStatus?: unknown; errorCode?: unknown };
    const status = typeof v.errorStatus === 'number' && v.errorStatus > 0 ? v.errorStatus : null;
    const code = typeof v.errorCode === 'string' ? v.errorCode : null;
    // Only a known transport code means "never reached the server"; a loader
    // that threw a plain JS error also has no status, but it is our fault.
    return { status, code, message: String(v.error ?? ''), isNetwork: status === null && !!code && NETWORK_CODES.has(code) };
  }
  if (err instanceof Error) return { status: null, code: null, message: err.message, isNetwork: false };
  return { status: null, code: null, message: String(err ?? ''), isNetwork: false };
}

export function describeLoadError(err: unknown): LoadErrorDescription {
  const f = factsOf(err);
  if (typeof f === 'string') return { reason: null, details: f };
  const details = f.message;
  if (f.code && SELF_EXPLAINED_CODES.has(f.code)) return { reason: null, details };
  // An EMPTY 503 is the API's TimeoutLayer giving up on the request budget.
  // Every handler-made 503 carries a coded envelope, and a proxy's 503 carries
  // its own page (code `http_error`): that one is an outage, not slowness.
  const bareTimeout = f.status === 503 && f.code === 'empty_response';
  if (f.status === 504 || bareTimeout || (f.code && TIMEOUT_CODES.has(f.code))) {
    return { reason: 'common.loadError.timeout', details };
  }
  if (f.isNetwork) return { reason: 'common.loadError.network', details };
  if (f.status === 429) return { reason: 'common.loadError.rateLimited', details };
  if (f.status === 403) return { reason: 'common.loadError.forbidden', details };
  if (f.status === 404) return { reason: 'common.loadError.notFound', details };
  if (f.status !== null && f.status >= 400 && f.status < 500) return { reason: null, details };
  return { reason: 'common.loadError.server', details };
}

/**
 * The failure as one line of text: the human sentence when there is one, the
 * raw message when the failure keeps its own words. For places that fold the
 * failure into a sentence of their own (the "showing older results" notices)
 * rather than rendering an ErrorState. Call from markup or `$derived`.
 */
export function loadErrorText(err: unknown): string {
  const d = describeLoadError(err);
  return d.reason ? t(d.reason) : d.details;
}
