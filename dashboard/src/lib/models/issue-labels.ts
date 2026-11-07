/**
 * The words for an issue's level and status, by their wire values.
 *
 * One table each, shared by every place that shows them — the level and
 * status badges, the filter bar's chips and value picker, the subscription
 * dialog's level choice — so a new level cannot read as a word in one place
 * and as `critical` in another. Translate with `labelFor`.
 */
import type { MessageKey } from '../i18n';

export const LEVEL_KEYS: Record<string, MessageKey> = {
  fatal: 'level.fatal',
  error: 'level.error',
  warning: 'level.warning',
  info: 'level.info',
  debug: 'level.debug',
};

export const STATUS_KEYS: Record<string, MessageKey> = {
  unresolved: 'status.unresolved',
  resolved: 'status.resolved',
  ignored: 'status.ignored',
};
