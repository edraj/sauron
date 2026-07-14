import { configureAuthBridge } from '../api/client';
import * as authApi from '../api/auth';
import type { LoginPayload, RegisterPayload, User } from '../models';

export type AuthStatus =
  | 'idle'
  | 'booting'
  | 'authenticated'
  | 'unauthenticated';

const REFRESH_KEY = 'sauron.refresh_token';

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
    writeRefreshToken(null);
  }

  async login(payload: LoginPayload): Promise<void> {
    const session = await authApi.login(payload);
    this.accessToken = session.access_token;
    this.user = session.user;
    writeRefreshToken(session.refresh_token);
    this.status = 'authenticated';
  }

  async register(payload: RegisterPayload): Promise<void> {
    const session = await authApi.register(payload);
    this.accessToken = session.access_token;
    this.user = session.user;
    writeRefreshToken(session.refresh_token);
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
      this.user = await authApi.getMe();
      this.status = 'authenticated';
    } catch {
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
