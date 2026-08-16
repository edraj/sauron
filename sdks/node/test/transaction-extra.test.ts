import { describe, expect, it } from 'vitest';
import {
  capTransactionExtra,
  MAX_TRANSACTION_EXTRA_BYTES,
} from '../src/transaction-extra.js';

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
    const capped = capTransactionExtra({
      response: 'x'.repeat(MAX_TRANSACTION_EXTRA_BYTES + 1),
    });
    expect(capped._truncated).toBe(true);
    expect(capped._bytes as number).toBeGreaterThan(MAX_TRANSACTION_EXTRA_BYTES);
    // The whole map goes, not just the offending key.
    expect(capped.response).toBeUndefined();
  });

  it('measures UTF-8 BYTES, not string length', () => {
    // Under the cap by character count, over it by bytes. Measured wrong, the
    // envelope is ~2x the size the SDK believed it was sending.
    const capped = capTransactionExtra({
      body: 'é'.repeat(MAX_TRANSACTION_EXTRA_BYTES - 100),
    });
    expect(capped._truncated).toBe(true);
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

  it('uses the same limit as every other SDK', () => {
    expect(MAX_TRANSACTION_EXTRA_BYTES).toBe(16 * 1024);
  });
});
