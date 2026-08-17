import axios, {
  AxiosError,
  type AxiosInstance,
  type InternalAxiosRequestConfig,
} from 'axios';
import { apiBaseUrl } from '../config/env';
import type { ApiErrorEnvelope, NormalizedError } from '../models';
import { computeScopeParams, currentEnvironmentId } from './scope';

// ---------------------------------------------------------------------------
// Auth bridge
//
// The axios instance must not import the auth store directly (that would create
// an import cycle: store -> api -> client -> store). Instead the store wires
// itself in via configureAuthBridge() once at startup. The client only ever
// touches these callbacks at request time, never at module-evaluation time.
// ---------------------------------------------------------------------------

export interface AuthBridge {
  /** Current in-memory access token, or null if unauthenticated. */
  getAccessToken(): string | null;
  /** Perform a token refresh (rotating the refresh token) and resolve with the new access token. */
  refresh(): Promise<string>;
  /** Called when a refresh attempt fails — the store should log out + redirect. */
  onRefreshFailure(): void;
}

const noopBridge: AuthBridge = {
  getAccessToken: () => null,
  refresh: async () => {
    throw new Error('auth bridge not configured');
  },
  onRefreshFailure: () => {},
};

let bridge: AuthBridge = noopBridge;

export function configureAuthBridge(next: AuthBridge): void {
  bridge = next;
}

/**
 * The current access token, for the one caller that cannot go through axios.
 *
 * `overview-stream.ts` reads an SSE response with `fetch()` because the
 * browser's native `EventSource` cannot set an `Authorization` header, and the
 * two usual ways around that are both worse: a token in the query string writes
 * a live JWT into every access log and `Referer`, and cookie auth would open a
 * CSRF surface this API does not currently have.
 *
 * Deliberately narrow — it returns the token and nothing else, so this does not
 * become a general back door around the interceptors. Anything that CAN use
 * axios must.
 */
export function currentAccessToken(): string | null {
  return bridge.getAccessToken();
}

/** Refresh once, shared with in-flight refreshes. See {@link runRefreshOnce}. */
export function refreshAccessToken(): Promise<string> {
  return runRefreshOnce();
}

// ---------------------------------------------------------------------------
// Axios instances
// ---------------------------------------------------------------------------

const baseConfig = {
  baseURL: apiBaseUrl,
  headers: { 'Content-Type': 'application/json' },
};

/**
 * Bare instance with NO interceptors. Used for the auth endpoints
 * (login / register / refresh / logout) so the refresh call can never recurse
 * back through the 401 handler.
 */
export const bareClient: AxiosInstance = axios.create(baseConfig);

/** Main instance used by every authenticated request. */
export const api: AxiosInstance = axios.create(baseConfig);

// ---------------------------------------------------------------------------
// Request interceptor — attach the bearer token when present.
// ---------------------------------------------------------------------------

api.interceptors.request.use((config: InternalAxiosRequestConfig) => {
  const token = bridge.getAccessToken();
  if (token) {
    config.headers.set('Authorization', `Bearer ${token}`);
  }
  return config;
});

// ---------------------------------------------------------------------------
// Request interceptor — attach `environment_id` from the session store to
// every environment-scoped read (see `./scope.ts` for the opt-out list and
// the wire-contract rule that a `null` environment omits the parameter
// entirely rather than sending it empty).
//
// Imports the predicate from `scope.ts` rather than the store directly —
// same reasoning as the auth bridge above: a module-level `import
// { sessionStore }` here would create a `store -> api -> client -> store`
// cycle (the store's own load path imports `./orgs`, `./apps`, etc., which
// import this module). `scope.ts` takes the same bridge approach as
// `configureAuthBridge` to sidestep that.
// ---------------------------------------------------------------------------

api.interceptors.request.use((config: InternalAxiosRequestConfig) => {
  const scopeParams = computeScopeParams(config.url, currentEnvironmentId());
  if (scopeParams) {
    config.params = { ...(config.params as Record<string, unknown> | undefined), ...scopeParams };
  }
  return config;
});

// ---------------------------------------------------------------------------
// Single-flight refresh
//
// If several requests fail with 401 at the same time, only ONE refresh runs;
// the others park on the same promise and replay once the new token lands.
// ---------------------------------------------------------------------------

let refreshPromise: Promise<string> | null = null;

function runRefreshOnce(): Promise<string> {
  if (!refreshPromise) {
    refreshPromise = bridge.refresh().finally(() => {
      refreshPromise = null;
    });
  }
  return refreshPromise;
}

type RetriableConfig = InternalAxiosRequestConfig & { _retry?: boolean; _retry_429_count?: number };

// ---------------------------------------------------------------------------
// Response interceptor — normalize errors, refresh-and-replay on 401.
// ---------------------------------------------------------------------------

/**
 * With `responseType: 'blob'` (the CSV export) an ERROR body is a Blob too, so
 * `normalizeError`'s `response.data as ApiErrorEnvelope` read yields
 * `undefined` and the message degrades to axios's generic "Request failed with
 * status code 403".
 *
 * This belongs in the interceptor, not in the caller: every branch of the
 * handler below ends in `Promise.reject(normalizeError(error))`, so by the time
 * a caller's `catch` runs there is no `error.response` left to re-read. Doing
 * it here also means every future blob-returning endpoint gets the fix free.
 */
export async function unwrapBlobErrorBody(error: AxiosError): Promise<void> {
  const data = error.response?.data as unknown;
  if (!(data instanceof Blob)) return;
  try {
    const text = await data.text();
    (error.response as { data: unknown }).data = JSON.parse(text);
  } catch {
    /* not JSON — leave the Blob in place */
  }
}

api.interceptors.response.use(
  (response) => response,
  async (error: AxiosError<ApiErrorEnvelope>) => {
    const original = error.config as RetriableConfig | undefined;

    // No response at all → treat as a network error, don't attempt refresh.
    if (!error.response) {
      return Promise.reject(normalizeError(error));
    }

    // Must run before the branching below: see `unwrapBlobErrorBody`.
    await unwrapBlobErrorBody(error);

    const status = error.response.status;
    const url = original?.url ?? '';
    const isAuthEndpoint = url.includes('/v1/auth/');

    if (status === 401 && original && !original._retry && !isAuthEndpoint) {
      original._retry = true;
      try {
        const newToken = await runRefreshOnce();
        original.headers.set('Authorization', `Bearer ${newToken}`);
        return api(original);
      } catch {
        bridge.onRefreshFailure();
        return Promise.reject(normalizeError(error));
      }
    }

    if (status === 429 && original) {
      const retryCount = original._retry_429_count ?? 0;
      if (retryCount < 3) {
        original._retry_429_count = retryCount + 1;
        const retryAfter = error.response.headers['retry-after'];
        const delaySeconds = retryAfter && !isNaN(Number(retryAfter))
          ? Number(retryAfter)
          : Math.pow(2, retryCount);
        
        await new Promise((resolve) => setTimeout(resolve, delaySeconds * 1000));
        return api(original);
      }
    }

    return Promise.reject(normalizeError(error));
  },
);

// ---------------------------------------------------------------------------
// Error normalization — collapse everything to a stable shape and read the
// { error: { code, message } } envelope the backend returns.
// ---------------------------------------------------------------------------

export function normalizeError(error: unknown): NormalizedError {
  if (axios.isAxiosError(error)) {
    const response = error.response;
    if (!response) {
      return {
        status: 0,
        code: error.code ?? 'network_error',
        message: error.message || 'Network error — is the API reachable?',
        isNetwork: true,
      };
    }
    const envelope = response.data as ApiErrorEnvelope | undefined;
    return {
      status: response.status,
      code: envelope?.error?.code ?? 'http_error',
      message: envelope?.error?.message ?? error.message ?? 'Request failed',
      isNetwork: false,
    };
  }
  if (error instanceof Error) {
    return { status: 0, code: 'error', message: error.message, isNetwork: false };
  }
  return { status: 0, code: 'error', message: 'Unknown error', isNetwork: false };
}

/** Type guard so callers can render a friendly message. */
export function isNormalizedError(value: unknown): value is NormalizedError {
  return (
    typeof value === 'object' &&
    value !== null &&
    'status' in value &&
    'code' in value &&
    'message' in value &&
    'isNetwork' in value
  );
}

export function errorMessage(value: unknown): string {
  if (isNormalizedError(value)) return value.message;
  if (value instanceof Error) return value.message;
  return 'Something went wrong';
}

/**
 * The HTTP status behind a caught error, or `null` when there is not one.
 *
 * The companion to {@link errorMessage}, for pages that hand-roll their request
 * state instead of going through `CachedView` (which exposes `errorStatus` of
 * its own). Callers that must distinguish "the query was rejected" from "the
 * server broke" need the code, not the prose: a 400 belongs on the search
 * input, a 500 belongs on the page's error card, and the message text reads
 * much the same either way.
 *
 * `null` rather than `0` for a non-HTTP failure. `0` is what the normalizer
 * uses for a network drop, and a caller comparing `status === 400` would treat
 * either as "not a query error" — but a caller doing arithmetic or truthiness
 * on it would not, and `null` makes the absence explicit.
 */
export function errorStatus(value: unknown): number | null {
  if (isNormalizedError(value)) return value.status || null;
  return null;
}
