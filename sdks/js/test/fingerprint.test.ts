import { describe, expect, it } from 'vitest';
import { buildErrorItem, normalizeFingerprint } from '../src/api/capture';

describe('fingerprint normalisation', () => {
  it('wraps a bare string so the envelope still parses server-side', () => {
    expect(normalizeFingerprint('/en/products')).toEqual(['/en/products']);
    expect(buildErrorItem(new Error('x'), [], { fingerprint: '/en/products' }).fingerprint).toEqual(['/en/products']);
  });

  it('keeps string arrays and stringifies non-string parts', () => {
    expect(normalizeFingerprint(['a', 'b'])).toEqual(['a', 'b']);
    expect(normalizeFingerprint(['a', 42, { k: 1 }])).toEqual(['a', '42', '{"k":1}']);
  });

  it('drops anything else to null', () => {
    for (const raw of [undefined, null, 42, { k: 1 }]) expect(normalizeFingerprint(raw)).toBeNull();
  });
});
