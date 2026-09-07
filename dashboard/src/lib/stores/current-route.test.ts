import { describe, expect, it } from 'vitest';
import { currentRoute, setCurrentRoute } from './current-route';

describe('currentRoute', () => {
  it('is a string before the router reports anything', () => {
    // Registrations made before the first navigation are tagged '' and simply
    // never match a real route — inert, not wrong.
    expect(typeof currentRoute()).toBe('string');
  });

  it('round-trips what the router last reported', () => {
    setCurrentRoute('/issues');
    expect(currentRoute()).toBe('/issues');
    setCurrentRoute('/overview');
    expect(currentRoute()).toBe('/overview');
  });

  it('strips a query string if one is passed', () => {
    // svelte-spa-router's `$location` already excludes it, but a future caller
    // reaching for `window.location.hash` would otherwise mint a separate tag
    // per filter combination and make Refresh reload almost nothing.
    setCurrentRoute('/issues?status=unresolved&q=foo');
    expect(currentRoute()).toBe('/issues');
  });

  it('handles a bare path with no query', () => {
    setCurrentRoute('/members');
    expect(currentRoute()).toBe('/members');
  });
});
