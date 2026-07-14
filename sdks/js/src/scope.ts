import type { Breadcrumb, UserContext, UserInput } from './types.js';

/** An empty user context (all fields null, no traits). */
export function emptyUser(): UserContext {
  return { id: null, email: null, traits: {} };
}

/**
 * Mutable per-client state: the current user, a ring buffer of breadcrumbs and
 * free-form tags. The breadcrumb buffer is capped at `maxBreadcrumbs`; the
 * oldest entries fall off the front (FIFO).
 */
export class Scope {
  private user: UserContext | null = null;
  private breadcrumbs: Breadcrumb[] = [];
  private maxBreadcrumbs: number;
  readonly tags: Record<string, string> = {};

  constructor(maxBreadcrumbs = 50) {
    this.maxBreadcrumbs = Math.max(0, maxBreadcrumbs);
  }

  setMaxBreadcrumbs(max: number): void {
    this.maxBreadcrumbs = Math.max(0, max);
    this.trim();
  }

  setUser(user: UserInput): void {
    if (user === null) {
      this.user = null;
      return;
    }
    this.user = {
      id: user.id ?? null,
      email: user.email ?? null,
      traits: user.traits ?? {},
    };
  }

  /** The user context for an envelope. Never null — defaults to an empty user. */
  getUser(): UserContext {
    return this.user ? { ...this.user, traits: { ...this.user.traits } } : emptyUser();
  }

  /** True when an identifiable user has been set. */
  hasUser(): boolean {
    return this.user !== null;
  }

  setTag(key: string, value: string): void {
    this.tags[key] = value;
  }

  addBreadcrumb(breadcrumb: Breadcrumb): void {
    if (this.maxBreadcrumbs <= 0) return;
    this.breadcrumbs.push(breadcrumb);
    this.trim();
  }

  /** A defensive copy of the current breadcrumb trail. */
  getBreadcrumbs(): Breadcrumb[] {
    return this.breadcrumbs.slice();
  }

  clearBreadcrumbs(): void {
    this.breadcrumbs = [];
  }

  private trim(): void {
    const overflow = this.breadcrumbs.length - this.maxBreadcrumbs;
    if (overflow > 0) {
      this.breadcrumbs.splice(0, overflow);
    }
  }
}
