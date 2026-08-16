import { describe, expect, it } from 'vitest';
import * as Sauron from '../src/index.js';
import { MAX_TRANSACTION_EXTRA_BYTES, capTransactionExtra } from '../src/index.js';

/**
 * Compile-checks the exact API shapes the README documents.
 *
 * Not a behaviour test — it exists because a README example is the one piece of
 * this SDK that nothing else compiles. Signatures drift, the docs keep telling
 * people to write code that no longer builds, and no suite notices.
 */
describe('README shapes', () => {
  it('trackTransaction accepts the documented tags/extra shape', () => {
    // No init(): trackTransaction is a no-op without a client, which is
    // exactly what makes this a pure compile check.
    Sauron.trackTransaction({
      name: 'POST /api/orders',
      op: 'http',
      durationMs: 842.5,
      status: 'ok',
      httpMethod: 'POST',
      httpStatus: 201,
      url: '/api/orders',
      tags: { api: 'orders', tier: 'premium' },
      extra: { request: '{}', response: '{}', response_bytes: 2 },
    });
    Sauron.trackTransaction({
      name: 'SELECT orders',
      op: 'custom',
      durationMs: 12,
      status: 'ok',
      tags: { db: 'sqlite', table: 'orders' },
      extra: { statement: 'SELECT 1', row_count: 20, params: ['u_1'] },
    });
  });

  it('exposes the documented cap constant from the package entrypoint', () => {
    expect(MAX_TRANSACTION_EXTRA_BYTES).toBe(16 * 1024);
    expect(
      capTransactionExtra({ a: 'x'.repeat(MAX_TRANSACTION_EXTRA_BYTES + 1) })._truncated,
    ).toBe(true);
  });
});
