import { configureAuthBridge, isNormalizedError } from '../api/client';
import * as authApi from '../api/auth';
import { viewCache } from './view-cache';
import type { LoginPayload, RegisterPayload, User } from '../models';

export type AuthStatus =
  | 'idle'
  | 'booting'
  | 'authenticated'
  | 'unauthenticated';

const REFRESH_KEY = 'sauron.refresh_token';

/** True for the API's 403 password_change_required. */
export function isPasswordChangeRequired(err: unknown): boolean {
  return isNormalizedError(err) && err.status === 403 && err.code === 'password_change_required';
}

function readRefreshToken(): string | null {
  if (typeof window === 'undefined') return null;
  return window.localStorage.getItem(REFRESH_KEY);
}

function writeRefreshToken(token: string | null): void {
  if (typeof window === 'undefined') return;
  if (token) window.localStorage.setItem(REFRESH_KEY, token);
  else window.localStorage.removeItem(REFRESH_KEY);
}

class AuthStore {
  // Access token lives in memory only — never persisted.
  accessToken = $state<string | null>(null);
  user = $state<User | null>(null);
  status = $state<AuthStatus>('idle');
  /**
   * The session is valid but owes a password change. Derived from the user
   * object when we have one; when /v1/me was blocked we have no user, and the
   * block itself is the signal.
   */
  mustChangePassword = $state(false);

  get isAuthenticated(): boolean {
    return this.status === 'authenticated' && this.accessToken !== null;
  }

  constructor() {
    // Wire this store into the axios client's auth bridge.
    configureAuthBridge({
      getAccessToken: () => this.accessToken,
      refresh: () => this.refresh(),
      onRefreshFailure: () => {
        this.clearLocal();
        this.status = 'unauthenticated';
        if (typeof window !== 'undefined') {
          window.location.hash = '#/login';
        }
      },
    });
  }

  private clearLocal(): void {
    this.accessToken = null;
    this.user = null;
    this.mustChangePassword = false;
    writeRefreshToken(null);
    // Every cached view payload belonged to the identity being cleared. Dropping
    // them here rather than in each page is what makes the guarantee total: this
    // runs on logout, on a failed refresh, and on a failed boot, so there is no
    // path that ends a session and leaves rows behind for whoever signs in next
    // on the same tab.
    viewCache.clear();
  }

  async login(payload: LoginPayload): Promise<void> {
    // Belt and braces alongside `clearLocal`: nothing currently reaches `login`
    // without an intervening `clearLocal`, but a sign-in is by definition an
    // identity change, and this is the one place that is true no matter how the
    // caller got here. Cheap, and it means a future path that swaps sessions
    // without logging out first cannot serve the previous user's rows.
    viewCache.clear();
    const session = await authApi.login(payload);
    this.accessToken = session.access_token;
    this.user = session.user;
    this.mustChangePassword = session.user.must_change_password;
    writeRefreshToken(session.refresh_token);
    this.status = 'authenticated';
  }

  async register(payload: RegisterPayload): Promise<void> {
    const session = await authApi.register(payload);
    this.accessToken = session.access_token;
    this.user = session.user;
    this.mustChangePassword = session.user.must_change_password;
    writeRefreshToken(session.refresh_token);
    this.status = 'authenticated';
  }

  /**
   * Change the password and adopt the fresh session the server returns. The
   * old refresh token is revoked server-side, so the new pair must replace it
   * here or the next refresh fails.
   */
  async applyPasswordChange(currentPassword: string, newPassword: string): Promise<void> {
    const session = await authApi.changePassword(currentPassword, newPassword);
    this.accessToken = session.access_token;
    this.user = session.user;
    writeRefreshToken(session.refresh_token);
    this.mustChangePassword = false;
    this.status = 'authenticated';
  }

  /**
   * Exchange the stored refresh token for a fresh access token.
   * The refresh token ROTATES — the new one is persisted, the old is now dead.
   * Returns the new access token. Throws (and clears state) on failure.
   */
  async refresh(): Promise<string> {
    const token = readRefreshToken();
    if (!token) {
      throw new Error('no refresh token');
    }
    try {
      const res = await authApi.refresh(token);
      this.accessToken = res.access_token;
      writeRefreshToken(res.refresh_token);
      return res.access_token;
    } catch (err) {
      this.clearLocal();
      throw err;
    }
  }

  /**
   * On app start: if a refresh token exists, silently refresh + load the user.
   * Resolves the store into either 'authenticated' or 'unauthenticated'.
   */
  async boot(): Promise<void> {
    this.status = 'booting';
    if (!readRefreshToken()) {
      this.status = 'unauthenticated';
      return;
    }
    try {
      await this.refresh();
    } catch {
      this.clearLocal();
      this.status = 'unauthenticated';
      return;
    }
    try {
      this.user = await authApi.getMe();
      this.mustChangePassword = this.user.must_change_password;
      this.status = 'authenticated';
    } catch (err) {
      // A pending password change blocks /v1/me along with everything else.
      // That is a valid session that owes one action, not a failed one —
      // clearing it here would lock the user out of the only screen that can
      // fix it.
      if (isPasswordChangeRequired(err)) {
        this.user = null;
        this.mustChangePassword = true;
        this.status = 'authenticated';
        return;
      }
      this.clearLocal();
      this.status = 'unauthenticated';
    }
  }

  async logout(): Promise<void> {
    const token = readRefreshToken();
    if (token) {
      // Best effort — revoke server-side, but always clear locally.
      try {
        await authApi.logout(token);
      } catch {
        /* ignore */
      }
    }
    this.clearLocal();
    this.status = 'unauthenticated';
  }
}

export const authStore = new AuthStore();
