import { beforeEach, describe, expect, it, vi } from 'vitest';
import { authStore, isPasswordChangeRequired } from './auth.svelte';
import { viewCache } from './view-cache';

vi.mock('../api/auth', () => ({
  changePassword: vi.fn(),
  getMe: vi.fn(),
  login: vi.fn().mockResolvedValue({
    access_token: 'tok',
    refresh_token: 'refresh',
    user: { id: 'u1', email: 'b@example.test', must_change_password: false },
  }),
  logout: vi.fn(),
  refresh: vi.fn(),
  register: vi.fn(),
}));

describe('isPasswordChangeRequired', () => {
  it('is true for a normalized 403 password_change_required error', () => {
    expect(
      isPasswordChangeRequired({
        status: 403,
        code: 'password_change_required',
        message: '',
        isNetwork: false,
      }),
    ).toBe(true);
  });

  it('is false when the code does not match, even at 403', () => {
    expect(
      isPasswordChangeRequired({
        status: 403,
        code: 'http_error',
        message: '',
        isNetwork: false,
      }),
    ).toBe(false);
  });

  it('is false when the code matches but the status is not 403', () => {
    expect(
      isPasswordChangeRequired({
        status: 401,
        code: 'password_change_required',
        message: '',
        isNetwork: false,
      }),
    ).toBe(false);
  });

  it('is false for a plain Error', () => {
    expect(isPasswordChangeRequired(new Error('x'))).toBe(false);
  });

  it('is false for null, undefined, and empty object', () => {
    expect(isPasswordChangeRequired(null)).toBe(false);
    expect(isPasswordChangeRequired(undefined)).toBe(false);
    expect(isPasswordChangeRequired({})).toBe(false);
  });
});

// The identity boundary on the view cache. Cached payloads are one user's
// RBAC-filtered rows, so ending or changing a session has to drop them — and it
// has to do so centrally, not by asking each page to remember. These assert the
// two hooks in auth.svelte.ts that make it unconditional.
describe('session end clears the view cache', () => {
  beforeEach(() => {
    viewCache.clear();
  });

  it('logout drops every cached payload', async () => {
    viewCache.set('issues.list app-1', [{ id: 'issue-from-user-a' }]);
    viewCache.set('events.list app-1', [{ id: 'event-from-user-a' }]);
    expect(viewCache.size).toBe(2);
    await authStore.logout();
    expect(viewCache.size).toBe(0);
  });

  it('signing in drops anything a previous session left behind', async () => {
    // Belt-and-braces path: even if a session were replaced without an
    // intervening logout, the new identity must not inherit cached rows.
    viewCache.set('issues.list app-1', [{ id: 'issue-from-user-a' }]);
    await authStore.login({ email: 'b@example.test', password: 'x' });
    expect(viewCache.size).toBe(0);
  });
});
