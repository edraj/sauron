import axios, {
  AxiosError,
  type AxiosInstance,
  type InternalAxiosRequestConfig,
} from 'axios';
import { apiBaseUrl } from '../config/env';
import type { ApiErrorEnvelope, NormalizedError } from '../models';

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

type RetriableConfig = InternalAxiosRequestConfig & { _retry?: boolean };

// ---------------------------------------------------------------------------
// Response interceptor — normalize errors, refresh-and-replay on 401.
// ---------------------------------------------------------------------------

api.interceptors.response.use(
  (response) => response,
  async (error: AxiosError<ApiErrorEnvelope>) => {
    const original = error.config as RetriableConfig | undefined;

    // No response at all → treat as a network error, don't attempt refresh.
    if (!error.response) {
      return Promise.reject(normalizeError(error));
    }

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
