import { describe, expect, it } from 'vitest';
import { capTransactionExtra, MAX_TRANSACTION_EXTRA_BYTES } from '../src/utils.js';
import { buildTransactionItem } from '../src/api/product.js';

/**
 * The cap on a transaction's `extra`.
 *
 * Worth its own suite because the failure it prevents is invisible from the
 * outside: transactions ship in BATCHED envelopes, and ingest rejects the whole
 * envelope past `INGEST_MAX_BODY_BYTES`. One oversized response body does not
 * lose one span — it loses every unrelated span batched alongside it, with a
 * 400 the transport drops without retrying.
 */
describe('capTransactionExtra', () => {
  it('passes a small payload through unchanged', () => {
    const extra = { request: '{"page":1}', retries: 2 };
    expect(capTransactionExtra(extra)).toBe(extra);
  });

  it('replaces an oversized payload with a truncation marker', () => {
    const body = 'x'.repeat(MAX_TRANSACTION_EXTRA_BYTES + 1);
    const capped = capTransactionExtra({ response: body });
    expect(capped._truncated).toBe(true);
    expect(capped._bytes as number).toBeGreaterThan(MAX_TRANSACTION_EXTRA_BYTES);
    // The whole map goes, not just the offending key — a half-written payload
    // is worse than an honest marker.
    expect(capped.response).toBeUndefined();
  });

  it('measures UTF-8 BYTES, not string length', () => {
    // Just under the cap in JS string length, well over it in UTF-8 bytes.
    // `json.length` would wave this through and the envelope would be ~3x the
    // size the SDK believed it was sending.
    const multibyte = 'é'.repeat(MAX_TRANSACTION_EXTRA_BYTES - 100);
    const capped = capTransactionExtra({ body: multibyte });
    expect(capped._truncated).toBe(true);
  });

  it('counts a surrogate pair as one 4-byte code point', () => {
    // Two UTF-16 units, four UTF-8 bytes. Counted as two 3-byte characters the
    // measurement drifts 50% high on emoji-heavy payloads and truncates
    // requests that would have fit.
    const emoji = '\u{1F600}'.repeat(1000); // 4000 UTF-8 bytes
    const capped = capTransactionExtra({ body: emoji });
    expect(capped._truncated).toBeUndefined();
  });

  it('marks an unserializable payload rather than throwing', () => {
    // An SDK that crashes the app it is measuring is worse than one that drops
    // a payload.
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;
    const capped = capTransactionExtra(cyclic);
    expect(capped._truncated).toBe(true);
    expect(capped._bytes).toBe(-1);
  });
});

describe('trackTransaction metadata', () => {
  it('omits tags and extra entirely when not supplied', () => {
    const item = buildTransactionItem({ name: '/x', durationMs: 1 }, null, null);
    expect('tags' in item).toBe(false);
    expect('extra' in item).toBe(false);
  });

  it('omits them when supplied but empty', () => {
    const item = buildTransactionItem(
      { name: '/x', durationMs: 1, tags: {}, extra: {} },
      null,
      null,
    );
    expect('tags' in item).toBe(false);
    expect('extra' in item).toBe(false);
  });

  it('caps extra on the way onto the item', () => {
    const item = buildTransactionItem(
      { name: '/x', durationMs: 1, extra: { b: 'y'.repeat(MAX_TRANSACTION_EXTRA_BYTES) } },
      null,
      null,
    );
    expect(item.extra?._truncated).toBe(true);
  });

  it('copies the caller maps rather than aliasing them', () => {
    // The item is QUEUED, not sent inline, so a caller mutating their own map
    // after the call would otherwise change what ships.
    const tags = { tier: 'free' };
    const item = buildTransactionItem({ name: '/x', durationMs: 1, tags }, null, null);
    tags.tier = 'premium';
    expect(item.tags).toEqual({ tier: 'free' });
  });
});
