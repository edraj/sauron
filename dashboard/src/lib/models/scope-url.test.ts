import { describe, expect, it } from 'vitest';
import { parseScopeSearch, scopeSearch, unmetScope, type ScopeSelection } from './scope-url';

const FULL: ScopeSelection = {
  org: 'o1',
  project: 'p1',
  app: 'a1',
  release: 'v2.0.71+2071',
  env: 'e1',
};

describe('parseScopeSearch', () => {
  it('returns null when the link names no scope', () => {
    expect(parseScopeSearch('')).toBeNull();
    expect(parseScopeSearch('?utm_source=slack')).toBeNull();
  });

  it('reads every level', () => {
    expect(parseScopeSearch('?org=o1&project=p1&app=a1&release=v2&env=e1')).toEqual({
      org: 'o1',
      project: 'p1',
      app: 'a1',
      release: 'v2',
      env: 'e1',
    });
  });

  it('reads an absent env / release as "all" once the link names an app', () => {
    expect(parseScopeSearch('?org=o1&project=p1&app=a1')).toEqual({
      org: 'o1',
      project: 'p1',
      app: 'a1',
      release: null,
      env: null,
    });
  });

  it('leaves env / release unsaid when the link names no app', () => {
    expect(parseScopeSearch('?org=o1')).toEqual({ org: 'o1' });
  });

  it('passes the "none" sentinels straight through', () => {
    expect(parseScopeSearch('?app=a1&release=none&env=none')).toEqual({
      app: 'a1',
      release: 'none',
      env: 'none',
    });
  });

  it('treats an empty value as absent', () => {
    expect(parseScopeSearch('?org=&app=a1&env=')).toEqual({ app: 'a1', release: null, env: null });
  });
});

describe('scopeSearch', () => {
  it('writes the scope in breadcrumb order', () => {
    expect(scopeSearch('', FULL)).toBe('?org=o1&project=p1&app=a1&release=v2.0.71%2B2071&env=e1');
  });

  it('omits "all" levels', () => {
    expect(scopeSearch('', { ...FULL, release: null, env: null })).toBe('?org=o1&project=p1&app=a1');
  });

  it('returns an empty string for an empty scope', () => {
    expect(
      scopeSearch('', { org: null, project: null, app: null, release: null, env: null }),
    ).toBe('');
  });

  it('replaces stale scope params and keeps foreign ones after them', () => {
    expect(scopeSearch('?app=old&utm_source=slack&env=old', FULL)).toBe(
      '?org=o1&project=p1&app=a1&release=v2.0.71%2B2071&env=e1&utm_source=slack',
    );
  });

  it('round-trips through parseScopeSearch, including awkward release names', () => {
    for (const release of ['v2.0.71.2071-release-PROD', 'a b&c=d', '1.0+build#7', 'none', null]) {
      const scope = { ...FULL, release };
      const seed = parseScopeSearch(scopeSearch('', scope));
      expect(seed).toEqual(scope);
    }
  });

  it('is a fixed point: re-writing the same scope changes nothing', () => {
    const once = scopeSearch('?x=1', FULL);
    expect(scopeSearch(once, FULL)).toBe(once);
  });
});

describe('unmetScope', () => {
  it('is empty when every named level landed', () => {
    expect(unmetScope({ org: 'o1', app: 'a1', env: 'e1', release: null }, { ...FULL, release: null })).toEqual([]);
  });

  it('ignores levels the link did not name', () => {
    expect(unmetScope({ org: 'o1' }, FULL)).toEqual([]);
  });

  it('names each level that fell back', () => {
    expect(unmetScope({ org: 'o1', app: 'other', release: 'v9', env: null }, FULL)).toEqual([
      'app',
      'release',
      'env',
    ]);
  });
});
