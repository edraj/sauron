import { api } from './client';
import { searchParams, type SearchEnvelope, type SearchParams } from './search';
import type { Transaction } from '../models';

/**
 * The searched per-span transactions list.
 *
 * Distinct from `api/performance.ts`, which serves the two AGGREGATES
 * (`/performance/summary` groups by operation, `/performance/series` is a time
 * series). This route returns individual rows — the only place a developer's
 * per-call `tags`/`extra` is searchable.
 *
 * `sort` accepts `occurred_at` (the default), `duration_ms`, `name` or `op`,
 * `-`-prefixed for ascending. Anything else is a 400 naming what is allowed.
 * `limit` is clamped server-side to 1..200.
 *
 * There is no `offset`: `duration_ms` ties constantly (every cached response is
 * `0.0`), so an OFFSET page boundary landing inside a tied group repeats or
 * skips rows. Follow `next_cursor`.
 */
export type ListTransactionsParams = SearchParams;

/**
 * The window columns this route accepts. An unlisted `time_field` is a 400
 * that names the allowed set — mirrors `routes/transactions.rs:TIME_FIELDS`.
 */
export const TRANSACTION_TIME_FIELDS = ['occurred_at', 'received_at'] as const;

export async function listTransactions(
  appId: string,
  opts: ListTransactionsParams = {},
): Promise<SearchEnvelope<Transaction>> {
  const p = searchParams(opts);
  const { data } = await api.get<SearchEnvelope<Transaction>>(
    `/v1/apps/${appId}/transactions?${p.toString()}`,
  );
  return data;
}
