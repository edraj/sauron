import type { Breadcrumb, UserContext, UserInput } from './types.js';

/** An empty user context (all fields null, no traits). */
export function emptyUser(): UserContext {
  return { id: null, email: null, traits: {} };
}

/**
 * Shallow-merge a per-call override map over a base map. The override wins per
 * top-level key — tags/extra merge by key, contexts merge by block name (a
 * per-call block replaces the same-named base block). Returns a fresh object;
 * callers OMIT the field entirely when the result is empty (emit convention).
 */
export function mergeMeta(
  base: Record<string, unknown>,
  override?: Record<string, unknown>,
): Record<string, unknown> {
  return override ? { ...base, ...override } : { ...base };
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
  readonly contexts: Record<string, unknown> = {};
  readonly extra: Record<string, unknown> = {};

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

  /** Merge a batch of tags into the scope (last-write-wins per key). */
  setTags(tags: Record<string, string>): void {
    Object.assign(this.tags, tags);
  }

  /** Set (replace) a named context block on the scope. */
  setContext(name: string, block: Record<string, unknown>): void {
    this.contexts[name] = block;
  }

  /** Set a single freeform extra value on the scope. */
  setExtra(key: string, value: unknown): void {
    this.extra[key] = value;
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
