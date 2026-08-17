import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { init, type SauronClient } from '../src/client.js';
import { addNavigationBreadcrumb } from '../src/api/breadcrumbs.js';
import type { Breadcrumb } from '../src/types.js';

/**
 * `installHistory` patches `history.pushState`/`replaceState` and listens for
 * `popstate`. The suite runs on Node (no jsdom), so the three globals it reaches
 * for are stubbed here — which also lets a test drive a *forward* navigation,
 * something jsdom would not make any easier to distinguish.
 */

let crumbs: Breadcrumb[] = [];
let client: SauronClient;
let popstateHandlers: Array<() => void> = [];

/** The stubbed `location.href`, mutated by the fake history and by popstate. */
let href = 'https://app.example.com/';

function installGlobals(): void {
  const g = globalThis as Record<string, unknown>;
  g.location = {
    get href() {
      return href;
    },
  };
  g.history = {
    pushState(_state: unknown, _title: string, url?: string) {
      if (url) href = new URL(url, href).href;
    },
    replaceState(_state: unknown, _title: string, url?: string) {
      if (url) href = new URL(url, href).href;
    },
  };
  g.addEventListener = (type: string, handler: () => void) => {
    if (type === 'popstate') popstateHandlers.push(handler);
  };
  g.removeEventListener = (type: string, handler: () => void) => {
    if (type === 'popstate') popstateHandlers = popstateHandlers.filter((h) => h !== handler);
  };
}

/** Simulate a back/forward step: the URL changes, then `popstate` fires. */
function popTo(path: string): void {
  href = new URL(path, href).href;
  for (const h of popstateHandlers) h();
}

function navigationCrumbs(): Breadcrumb[] {
  return crumbs.filter((c) => c.type === 'navigation');
}

function lastOperation(): unknown {
  const nav = navigationCrumbs();
  return nav[nav.length - 1]?.data?.operation;
}

describe('history navigation breadcrumbs', () => {
  beforeEach(() => {
    crumbs = [];
    popstateHandlers = [];
    href = 'https://app.example.com/';
    installGlobals();
    client = init({
      dsn: 'https://pk_test@localhost:9/1',
      beforeBreadcrumb: (b) => {
        crumbs.push(b);
        return b;
      },
    });
  });

  afterEach(() => {
    client.teardown();
  });

  it('records pushState as a push', () => {
    globalThis.history.pushState({}, '', '/settings');
    expect(lastOperation()).toBe('push');
  });

  it('records replaceState as a replace', () => {
    globalThis.history.replaceState({}, '', '/settings');
    expect(lastOperation()).toBe('replace');
  });

  it('records popstate as a pop', () => {
    globalThis.history.pushState({}, '', '/settings');
    popTo('/');
    expect(lastOperation()).toBe('pop');
  });

  // A forward navigation fires the same `popstate` as a back navigation and
  // carries nothing to tell them apart. We deliberately do NOT try: labelling
  // both `pop` is the decision, so it is pinned here rather than left to a
  // comment a later "fix" would not have to argue with.
  it('records a forward navigation as a pop too', () => {
    globalThis.history.pushState({}, '', '/settings');
    popTo('/'); // back
    popTo('/settings'); // forward
    expect(lastOperation()).toBe('pop');
    expect(navigationCrumbs().map((c) => c.data?.operation)).toEqual(['push', 'pop', 'pop']);
  });

  it('keeps carrying from and to alongside the operation', () => {
    globalThis.history.pushState({}, '', '/settings');
    expect(navigationCrumbs()[0]?.data).toMatchObject({
      from: '/',
      to: '/settings',
      operation: 'push',
    });
  });

  // The same-path guard runs before direction is recorded, so a no-op
  // `replaceState` to the current URL must not produce a `replace` crumb.
  it('suppresses a replaceState to the current path', () => {
    globalThis.history.replaceState({}, '', '/');
    expect(navigationCrumbs()).toHaveLength(0);
  });

  it('defaults to push when the helper is called without an operation', () => {
    addNavigationBreadcrumb('/a', '/b');
    expect(lastOperation()).toBe('push');
  });
});
