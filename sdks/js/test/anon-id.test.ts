import { beforeEach, describe, expect, it } from 'vitest';
import {
  ANON_ID_KEY,
  getAnonymousId,
  resetAnonymousId,
  resetIdentity,
} from '../src/identity.js';

/** Minimal writable localStorage stand-in; the SDK probes before using one. */
function installStorage(): Map<string, string> {
  const map = new Map<string, string>();
  (globalThis as Record<string, unknown>).localStorage = {
    getItem: (k: string) => (map.has(k) ? (map.get(k) as string) : null),
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
  };
  return map;
}

describe('anonymous id', () => {
  let store: Map<string, string>;

  beforeEach(() => {
    store = installStorage();
    resetIdentity();
  });

  it('persists across page loads instead of being re-minted in memory', () => {
    const first = getAnonymousId();
    expect(store.get(ANON_ID_KEY)).toBe(first);
    // A fresh page load: the in-memory cache is gone, storage is not.
    resetIdentity();
    expect(getAnonymousId()).toBe(first);
  });

  it('keeps the anon_ prefix so existing data stays recognisable', () => {
    expect(getAnonymousId()).toMatch(/^anon_/);
  });

  it('resetAnonymousId mints a new one and persists it', () => {
    const first = getAnonymousId();
    const second = resetAnonymousId();
    expect(second).not.toBe(first);
    expect(store.get(ANON_ID_KEY)).toBe(second);
    expect(getAnonymousId()).toBe(second);
  });

  it('degrades to a per-process id with no writable storage', () => {
    delete (globalThis as Record<string, unknown>).localStorage;
    resetIdentity();
    const a = getAnonymousId();
    expect(a).toMatch(/^anon_/);
    expect(getAnonymousId()).toBe(a);
  });
});
