/**
 * Scope — global process-wide defaults plus per-request/per-task isolation.
 *
 * A single global {@link Scope} holds process-wide user/tags/context/breadcrumbs.
 * {@link withScope} (or {@link runWithAsyncScope}) layers an isolated child scope
 * over the current one for the duration of a callback, backed by
 * {@link AsyncLocalStorage} so concurrent requests never leak state into each
 * other. Reads merge child-over-parent because a child is a *snapshot* of its
 * parent taken at entry.
 */

import { AsyncLocalStorage } from 'node:async_hooks';

import type { Breadcrumb, BreadcrumbInput, ErrorUser, ScopeData, User } from './types.js';

const DEFAULT_MAX_BREADCRUMBS = 100;

/** Stamp a breadcrumb, defaulting missing fields; preserves an existing timestamp. */
export function normalizeBreadcrumb(input: BreadcrumbInput | Breadcrumb): Breadcrumb {
  const existing = (input as { timestamp?: unknown }).timestamp;
  return {
    type: input.type ?? 'default',
    category: input.category ?? null,
    message: input.message ?? null,
    level: input.level ?? null,
    timestamp: typeof existing === 'string' ? existing : new Date().toISOString(),
    data: input.data ?? {},
  };
}

function toErrorUser(user: User | null): ErrorUser | null {
  if (!user) return null;
  return {
    id: user.id ?? null,
    email: user.email ?? null,
    username: user.username ?? null,
  };
}

/** The mutable per-scope state and the operations over it. */
export class Scope {
  readonly data: ScopeData = {
    user: null,
    tags: {},
    contexts: {},
    extra: {},
    breadcrumbs: [],
  };
  private maxBreadcrumbs: number;

  constructor(maxBreadcrumbs = DEFAULT_MAX_BREADCRUMBS) {
    this.maxBreadcrumbs = Math.max(0, maxBreadcrumbs);
  }

  setUser(user: User | null): this {
    this.data.user = user;
    return this;
  }

  setTag(key: string, value: string): this {
    this.data.tags[key] = value;
    return this;
  }

  setTags(tags: Record<string, string>): this {
    Object.assign(this.data.tags, tags);
    return this;
  }

  setContext(key: string, context: Record<string, unknown> | unknown): this {
    this.data.contexts[key] = context;
    return this;
  }

  setExtra(key: string, value: unknown): this {
    this.data.extra[key] = value;
    return this;
  }

  addBreadcrumb(crumb: BreadcrumbInput | Breadcrumb): this {
    if (this.maxBreadcrumbs <= 0) return this;
    this.data.breadcrumbs.push(normalizeBreadcrumb(crumb));
    const overflow = this.data.breadcrumbs.length - this.maxBreadcrumbs;
    if (overflow > 0) this.data.breadcrumbs.splice(0, overflow);
    return this;
  }

  setMaxBreadcrumbs(max: number): void {
    this.maxBreadcrumbs = Math.max(0, max);
    const overflow = this.data.breadcrumbs.length - this.maxBreadcrumbs;
    if (overflow > 0) this.data.breadcrumbs.splice(0, overflow);
  }

  /** A deep-enough snapshot; children never share mutable containers with parents. */
  clone(): Scope {
    const copy = new Scope(this.maxBreadcrumbs);
    copy.data.user = this.data.user ? { ...this.data.user } : null;
    copy.data.tags = { ...this.data.tags };
    copy.data.contexts = { ...this.data.contexts };
    copy.data.extra = { ...this.data.extra };
    copy.data.breadcrumbs = this.data.breadcrumbs.slice();
    return copy;
  }

  /**
   * Layer this scope onto an error item: fill breadcrumbs from the trail, merge
   * scope tags *under* any per-call tags, and set the user only when the item
   * has none (per-call values win).
   */
  applyToErrorItem(item: {
    tags?: Record<string, string>;
    contexts?: Record<string, unknown>;
    extra?: Record<string, unknown>;
    user?: ErrorUser | null;
    breadcrumbs?: Breadcrumb[];
  }): void {
    item.tags = { ...this.data.tags, ...(item.tags ?? {}) };
    const contexts = { ...this.data.contexts, ...(item.contexts ?? {}) };
    if (Object.keys(contexts).length > 0) item.contexts = contexts;
    else delete item.contexts;
    const extra = { ...this.data.extra, ...(item.extra ?? {}) };
    if (Object.keys(extra).length > 0) item.extra = extra;
    else delete item.extra;
    if (item.user == null) item.user = toErrorUser(this.data.user);
    item.breadcrumbs = this.data.breadcrumbs.slice();
  }

  /**
   * Merge this scope's metadata *under* per-call overrides for non-error items
   * (analytics `track`). tags & extra merge by shallow key; contexts merge by
   * block name (a per-call block replaces the same-named scope block). Empty
   * maps are omitted from the result per the emit convention.
   */
  mergeMetadata(
    overrides: {
      tags?: Record<string, string>;
      contexts?: Record<string, unknown>;
      extra?: Record<string, unknown>;
    } = {},
  ): { tags?: Record<string, string>; contexts?: Record<string, unknown>; extra?: Record<string, unknown> } {
    const out: {
      tags?: Record<string, string>;
      contexts?: Record<string, unknown>;
      extra?: Record<string, unknown>;
    } = {};
    const tags = { ...this.data.tags, ...(overrides.tags ?? {}) };
    if (Object.keys(tags).length > 0) out.tags = tags;
    const contexts = { ...this.data.contexts, ...(overrides.contexts ?? {}) };
    if (Object.keys(contexts).length > 0) out.contexts = contexts;
    const extra = { ...this.data.extra, ...(overrides.extra ?? {}) };
    if (Object.keys(extra).length > 0) out.extra = extra;
    return out;
  }
}

const globalScope = new Scope();
const als = new AsyncLocalStorage<Scope>();

/** The process-wide scope holding defaults set at/after init. */
export function getGlobalScope(): Scope {
  return globalScope;
}

/** The scope in effect right now: the async-local child if inside a scoped block, else global. */
export function getCurrentScope(): Scope {
  return als.getStore() ?? globalScope;
}

/**
 * Run `cb` with an isolated child scope (a snapshot of the current one). The
 * child is passed to `cb` and is what {@link getCurrentScope} returns for the
 * duration — including across `await`s inside `cb`. Returns whatever `cb`
 * returns (awaitable if `cb` is async).
 */
export function withScope<T>(cb: (scope: Scope) => T): T {
  const child = getCurrentScope().clone();
  return als.run(child, () => cb(child));
}

/** Like {@link withScope}, but the callback takes no scope argument. */
export function runWithAsyncScope<T>(cb: () => T): T {
  const child = getCurrentScope().clone();
  return als.run(child, cb);
}

/** Mutate the active scope (the global scope outside any {@link withScope}). */
export function configureScope(cb: (scope: Scope) => void): void {
  cb(getCurrentScope());
}
