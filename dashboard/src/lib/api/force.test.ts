import { afterEach, describe, expect, it } from 'vitest';
import { beginForcing, endForcing, isForcing } from './force';
import { isForceableUrl } from './scope';

afterEach(() => endForcing());

describe('isForceableUrl', () => {
  it('matches every server-cached endpoint, including the POST one', () => {
    expect(isForceableUrl('/v1/apps/abc/overview/totals')).toBe(true);
    expect(isForceableUrl('/v1/apps/abc/overview/series')).toBe(true);
    expect(isForceableUrl('/v1/apps/abc/overview/top-issues')).toBe(true);
    expect(isForceableUrl('/v1/apps/abc/overview/top-events')).toBe(true);
    expect(isForceableUrl('/v1/apps/abc/analytics/active-users')).toBe(true);
    expect(isForceableUrl('/v1/projects/p1/active-users')).toBe(true);
    expect(isForceableUrl('/v1/admin/storage')).toBe(true);
    // POST only because its query is a body. A method-based rule would skip
    // the most expensive cached endpoint in the app.
    expect(isForceableUrl('/v1/apps/abc/funnel')).toBe(true);
  });

  it('ignores a query string already on the url', () => {
    expect(isForceableUrl('/v1/apps/abc/overview/totals?since_days=7')).toBe(true);
  });

  it('does not match uncached endpoints', () => {
    expect(isForceableUrl('/v1/issues')).toBe(false);
    expect(isForceableUrl('/v1/orgs/o1/members')).toBe(false);
    expect(isForceableUrl(undefined)).toBe(false);
    expect(isForceableUrl('')).toBe(false);
  });

  it('does not match the neighbours a prefix rule would catch', () => {
    // `/overview` is the un-sectioned legacy read; `/overview/stream` is SSE,
    // where an unknown query param is least welcome; `/funnels` is the saved
    // funnel CRUD list, not the cached compute.
    expect(isForceableUrl('/v1/apps/abc/overview')).toBe(false);
    expect(isForceableUrl('/v1/apps/abc/overview/stream')).toBe(false);
    expect(isForceableUrl('/v1/apps/abc/overview/refresh')).toBe(false);
    expect(isForceableUrl('/v1/apps/abc/funnels')).toBe(false);
    expect(isForceableUrl('/v1/apps/abc/funnels/f1')).toBe(false);
  });

  it('excludes the CSV export', () => {
    // An export the user explicitly asked for is not a section on screen.
    expect(isForceableUrl('/v1/projects/p1/active-users.csv')).toBe(false);
  });
});

describe('the forcing flag', () => {
  it('is off by default and toggles', () => {
    expect(isForcing()).toBe(false);
    beginForcing();
    expect(isForcing()).toBe(true);
    endForcing();
    expect(isForcing()).toBe(false);
  });
});
