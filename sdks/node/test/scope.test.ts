import { describe, it, expect } from 'vitest';
import { getGlobalScope, withScope, getCurrentScope, Scope } from '../src/scope.js';

describe('scope', () => {
  it('merges global tags under a child scope', () => {
    getGlobalScope().setTag('env', 'prod');
    withScope((s) => {
      s.setTag('req', '42');
      const item: any = { type: 'error', tags: {} };
      getCurrentScope().applyToErrorItem(item);
      expect(item.tags).toEqual({ env: 'prod', req: '42' });
    });
  });

  it('isolates concurrent scopes (no leak)', async () => {
    const seen: string[] = [];
    await Promise.all([
      withScope(async (s) => {
        s.setTag('id', 'A');
        await tick();
        seen.push(getCurrentScope().data.tags.id);
      }),
      withScope(async (s) => {
        s.setTag('id', 'B');
        await tick();
        seen.push(getCurrentScope().data.tags.id);
      }),
    ]);
    expect(seen.sort()).toEqual(['A', 'B']);
  });

  it('bounds breadcrumbs at maxBreadcrumbs', () => {
    const s = new Scope(3);
    for (let i = 0; i < 5; i++) s.addBreadcrumb({ message: String(i) });
    expect(s.data.breadcrumbs.map((b) => b.message)).toEqual(['2', '3', '4']);
  });

  it('sets user, context and extra on the scope data', () => {
    const s = new Scope();
    s.setUser({ id: 'u1', email: 'a@b.co' });
    s.setContext('page', { route: '/home' });
    s.setExtra('trace', 'abc');
    s.setTags({ a: '1', b: '2' });
    expect(s.data.user).toEqual({ id: 'u1', email: 'a@b.co' });
    expect(s.data.contexts.page).toEqual({ route: '/home' });
    expect(s.data.extra.trace).toBe('abc');
    expect(s.data.tags).toEqual({ a: '1', b: '2' });
  });

  it('fills user and breadcrumbs on an error item from the scope', () => {
    const s = new Scope();
    s.setUser({ id: 'u7' });
    s.addBreadcrumb({ message: 'clicked' });
    const item: any = { type: 'error', tags: {}, user: null, breadcrumbs: [] };
    s.applyToErrorItem(item);
    expect(item.user).toEqual({ id: 'u7', email: null, username: null });
    expect(item.breadcrumbs).toHaveLength(1);
    expect(item.breadcrumbs[0].message).toBe('clicked');
  });
});

const tick = () => new Promise((r) => setTimeout(r, 5));
