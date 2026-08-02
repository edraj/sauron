import { api, bareClient, normalizeError } from './client';
import type {
  AuthSession,
  LoginPayload,
  RefreshResponse,
  RegisterPayload,
  User,
} from '../models';

// The auth token endpoints use the bare client (no interceptors) so they never
// carry a stale bearer and never trigger the 401 refresh loop.

/**
 * Normalizes its own rejection, for the same reason the two reset endpoints at
 * the bottom of this file do: `bareClient` deliberately has no response
 * interceptor, so it rejects with a raw AxiosError, and `isNormalizedError`
 * answers false for one (an AxiosError carries `status`, `code` and `message`
 * but never `isNetwork`). The login page branches on the 403
 * `password_reset_required` to replace the form with the emailed-link panel;
 * without this that branch can never fire, and the target of an admin-forced
 * reset is told "Request failed with status code 403" in a red box on the very
 * screen they have just been locked out of.
 */
export async function login(payload: LoginPayload): Promise<AuthSession> {
  try {
    const { data } = await bareClient.post<AuthSession>('/v1/auth/login', payload);
    return data;
  } catch (err) {
    throw normalizeError(err);
  }
}

export async function register(payload: RegisterPayload): Promise<AuthSession> {
  const { data } = await bareClient.post<AuthSession>('/v1/auth/register', payload);
  return data;
}

export async function refresh(refreshToken: string): Promise<RefreshResponse> {
  const { data } = await bareClient.post<RefreshResponse>('/v1/auth/refresh', {
    refresh_token: refreshToken,
  });
  return data;
}

export async function logout(refreshToken: string): Promise<void> {
  await bareClient.post('/v1/auth/logout', { refresh_token: refreshToken });
}

// /me goes through the main client so it carries the bearer.
export async function getMe(): Promise<User> {
  const { data } = await api.get<User>('/v1/me');
  return data;
}

/**
 * Goes through the main client, not bareClient: it needs the bearer token, and
 * it is one of only two endpoints the API allows while a password change is
 * outstanding.
 */
export async function changePassword(
  currentPassword: string,
  newPassword: string,
): Promise<AuthSession> {
  const { data } = await api.post<AuthSession>('/v1/auth/password', {
    current_password: currentPassword,
    new_password: newPassword,
  });
  return data;
}

/**
 * Both reset endpoints go through `bareClient`, not `api`: they are
 * unauthenticated, must never carry a stale bearer, and must never enter the
 * single-flight 401 refresh-and-replay loop — the same reason
 * login/register/refresh/logout use it.
 *
 * They must still normalize their own rejections. `bareClient` deliberately has
 * no response interceptor, so it rejects with a raw AxiosError, and
 * `isNormalizedError` answers false for one (an AxiosError carries `status`,
 * `code` and `message` but never `isNetwork`). Both callers branch on the
 * status — 404/429 on the request page, 401 on the consume page — so without
 * this the "server not upgraded" panel, the rate-limit toast and the dead-link
 * panel are all unreachable, and the reset page shows the raw "Request failed
 * with status code 401" instead. Confirmed at runtime, not inferred.
 */
export async function forgotPassword(email: string): Promise<void> {
  try {
    await bareClient.post('/v1/auth/forgot-password', { email });
  } catch (err) {
    throw normalizeError(err);
  }
}

export async function resetPassword(token: string, newPassword: string): Promise<void> {
  try {
    await bareClient.post('/v1/auth/reset-password', { token, new_password: newPassword });
  } catch (err) {
    throw normalizeError(err);
  }
}
