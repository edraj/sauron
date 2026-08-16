import { describe, expect, it } from 'vitest';
import { trackTransaction } from '../src/index.js';
import { MAX_TRANSACTION_EXTRA_BYTES, capTransactionExtra } from '../src/index.js';

/**
 * Compile-checks the exact API shapes the README documents. See the browser
 * SDK's twin for why this exists: a README example is the one piece of the SDK
 * nothing else compiles.
 */
describe('README shapes', () => {
  it('trackTransaction accepts the documented tags/extra shape', () => {
    // No init(): trackTransaction is a no-op without an active client, which
    // is exactly what makes this a pure compile check.
    trackTransaction({
      name: 'POST /orders',
      op: 'http',
      duration_ms: 842.5,
      status: 'ok',
      http_method: 'POST',
      http_status: 201,
      url: '/orders',
      distinct_id: 'u_1',
      tags: { route: 'POST /orders', tier: 'premium' },
      extra: { request: {}, response: {}, query: {}, request_headers: ['content-type'] },
    });
    trackTransaction({
      name: 'SELECT orders',
      op: 'db',
      duration_ms: 12,
      status: 'ok',
      tags: { db: 'postgres', table: 'orders' },
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
