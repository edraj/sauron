import { api, bareClient } from './client';
import type {
  AuthSession,
  LoginPayload,
  RefreshResponse,
  RegisterPayload,
  User,
} from '../models';

// The auth token endpoints use the bare client (no interceptors) so they never
// carry a stale bearer and never trigger the 401 refresh loop.

export async function login(payload: LoginPayload): Promise<AuthSession> {
  const { data } = await bareClient.post<AuthSession>('/v1/auth/login', payload);
  return data;
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
