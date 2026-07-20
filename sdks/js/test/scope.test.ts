import { describe, expect, it } from 'vitest';
import { Scope, mergeMeta } from '../src/scope';

describe('Scope metadata scopes (tags/contexts/extra)', () => {
  it('starts with empty tags/contexts/extra', () => {
    const s = new Scope();
    expect(s.tags).toEqual({});
    expect(s.contexts).toEqual({});
    expect(s.extra).toEqual({});
  });

  it('setTag / setTags merge by key (last-write-wins)', () => {
    const s = new Scope();
    s.setTag('a', '1');
    s.setTags({ b: '2', a: '3' });
    expect(s.tags).toEqual({ a: '3', b: '2' });
  });

  it('setContext replaces a whole block by name', () => {
    const s = new Scope();
    s.setContext('order', { id: 1 });
    s.setContext('order', { id: 2, total: 9 });
    s.setContext('page', { path: '/cart' });
    expect(s.contexts).toEqual({ order: { id: 2, total: 9 }, page: { path: '/cart' } });
  });

  it('setExtra sets freeform values by key', () => {
    const s = new Scope();
    s.setExtra('build', 'ci-42');
    s.setExtra('flag', true);
    expect(s.extra).toEqual({ build: 'ci-42', flag: true });
  });
});

describe('mergeMeta', () => {
  it('returns a fresh copy of base when no override', () => {
    const base = { a: 1 };
    const out = mergeMeta(base);
    expect(out).toEqual({ a: 1 });
    expect(out).not.toBe(base);
  });

  it('lets the override win per top-level key', () => {
    expect(mergeMeta({ a: 1, b: 2 }, { b: 3, c: 4 })).toEqual({ a: 1, b: 3, c: 4 });
  });
});
