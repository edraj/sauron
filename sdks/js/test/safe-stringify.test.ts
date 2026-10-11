import { describe, expect, it } from 'vitest';
import { safeStringify } from '../src/utils.js';

describe('safeStringify', () => {
  it('serialises an object shared by two siblings in full (not "[Circular]")', () => {
    // Two errors batched in one envelope share breadcrumb objects, because
    // Scope.getBreadcrumbs() is a shallow copy.
    const crumb = { type: 'default', message: 'click' };
    const out = JSON.parse(
      safeStringify({ items: [{ breadcrumbs: [crumb] }, { breadcrumbs: [crumb] }] }),
    );
    expect(out.items[1].breadcrumbs[0]).toEqual(crumb);
  });

  it('still replaces true cycles', () => {
    const a: Record<string, unknown> = { name: 'a' };
    a.self = a;
    expect(JSON.parse(safeStringify(a))).toEqual({ name: 'a', self: '[Circular]' });
  });

  it('replaces a cycle that goes through a shared object', () => {
    const shared: Record<string, unknown> = { k: 1 };
    shared.back = shared;
    const out = JSON.parse(safeStringify([shared, shared]));
    expect(out).toEqual([
      { k: 1, back: '[Circular]' },
      { k: 1, back: '[Circular]' },
    ]);
  });
});
