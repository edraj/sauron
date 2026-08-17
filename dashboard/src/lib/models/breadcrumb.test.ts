import { describe, it, expect } from 'vitest';
import type { Breadcrumb } from './index';
import { breadcrumbSummary, navigationOperation } from './breadcrumb';

function crumb(partial: Partial<Breadcrumb>): Breadcrumb {
  return { type: 'default', timestamp: '2026-08-17T10:00:00Z', ...partial };
}

describe('navigationOperation', () => {
  it('reads the operation off a navigation breadcrumb', () => {
    expect(navigationOperation(crumb({ type: 'navigation', data: { operation: 'push' } }))).toBe(
      'push',
    );
    expect(navigationOperation(crumb({ type: 'navigation', data: { operation: 'pop' } }))).toBe(
      'pop',
    );
  });

  it('accepts all four operations the SDKs emit', () => {
    for (const op of ['push', 'pop', 'replace', 'remove']) {
      expect(navigationOperation(crumb({ type: 'navigation', data: { operation: op } }))).toBe(op);
    }
  });

  // The direction is a recent addition. An older SDK — or any hand-built
  // breadcrumb — sends none, and those rows must keep the level-coloured dot
  // rather than render a blank node.
  it('returns null for a navigation breadcrumb with no operation', () => {
    expect(navigationOperation(crumb({ type: 'navigation', data: { from: '/', to: '/x' } }))).toBe(
      null,
    );
    expect(navigationOperation(crumb({ type: 'navigation', data: null }))).toBe(null);
  });

  it('returns null for an unrecognised operation value', () => {
    expect(navigationOperation(crumb({ type: 'navigation', data: { operation: 'teleport' } }))).toBe(
      null,
    );
    expect(navigationOperation(crumb({ type: 'navigation', data: { operation: 7 } }))).toBe(null);
  });

  // `operation` on a non-navigation crumb is somebody else's key, not ours.
  it('ignores an operation on a non-navigation breadcrumb', () => {
    expect(navigationOperation(crumb({ type: 'http', data: { operation: 'push' } }))).toBe(null);
  });
});

describe('breadcrumbSummary', () => {
  it('prefers the message when there is one', () => {
    expect(breadcrumbSummary(crumb({ type: 'navigation', message: '/checkout' }))).toBe('/checkout');
  });

  // The node renders the direction now, so repeating it as text is a duplicate.
  it('omits operation from the flattened data', () => {
    const summary = breadcrumbSummary(
      crumb({ type: 'navigation', data: { from: '/', to: '/settings', operation: 'push' } }),
    );
    expect(summary).toBe('from: /, to: /settings');
    expect(summary).not.toContain('operation');
  });

  // A Flutter crumb carries operation and nothing else; dropping it must not
  // leave an empty line where the category used to show.
  it('falls back to the category when operation was the only data', () => {
    expect(
      breadcrumbSummary(crumb({ type: 'navigation', category: 'route', data: { operation: 'pop' } })),
    ).toBe('route');
  });

  it('falls back to the type when there is no category either', () => {
    expect(breadcrumbSummary(crumb({ type: 'navigation', data: { operation: 'pop' } }))).toBe(
      'navigation',
    );
  });

  it('leaves non-navigation breadcrumbs flattened as before', () => {
    expect(breadcrumbSummary(crumb({ type: 'http', data: { url: '/api/users', status: 200 } }))).toBe(
      'url: /api/users, status: 200',
    );
  });
});
