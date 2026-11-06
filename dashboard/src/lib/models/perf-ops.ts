/**
 * Words for the two free-form span fields the performance views show: the
 * operation (`op`) and the span status.
 *
 * One place, because three views showed them (Performance, Transactions and
 * the per-operation drill-down) and their three copies had already drifted —
 * the drill-down printed `screen load` in English while the page it opened
 * from said it in the reader's language.
 *
 * Both fields are developer-supplied strings: the values every SDK sends get
 * words, anything else is shown as written. Call from markup or `$derived` so
 * a language switch reaches them.
 */
import { labelFor, type MessageKey } from '../i18n';

const OP_KEYS: Record<string, MessageKey> = {
  // The Performance page's "every op" filter value, not a wire op.
  All: 'perf.op.all',
  navigation: 'perf.op.navigation',
  screen_load: 'perf.op.screenLoad',
  resource: 'perf.op.resource',
  custom: 'perf.op.custom',
};

const STATUS_KEYS: Record<string, MessageKey> = {
  ok: 'transactions.status.ok',
  error: 'transactions.status.error',
};

/** `screen_load` → "Screen load"; an op the SDKs never send passes through, spaced. */
export function opLabel(op: string): string {
  // A protocol name, the same in every locale.
  if (op === 'http') return 'HTTP';
  const label = labelFor(OP_KEYS, op);
  return label === op ? op.replace(/_/g, ' ') : label;
}

/** `ok` / `error` in words; a developer's own status is shown exactly as sent. */
export function spanStatusLabel(status: string): string {
  return labelFor(STATUS_KEYS, status);
}
