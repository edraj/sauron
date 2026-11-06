/**
 * The human name of a purge kind, by its wire slug (sauron-purge's
 * `PurgeKind::slug`). Call from markup or `$derived` so a language switch
 * reaches it.
 */
import { labelFor, type MessageKey } from '../i18n';

const KIND_NAMES: Record<string, MessageKey> = {
  error_events: 'purge.kind.error_events',
  analytics_events: 'purge.kind.analytics_events',
  transactions: 'purge.kind.transactions',
  inspector: 'purge.kind.inspector',
  issues: 'purge.kind.issues',
  sessions: 'purge.kind.sessions',
  devices: 'purge.kind.devices',
  persons: 'purge.kind.persons',
  workflows: 'purge.kind.workflows',
};

/**
 * `error_events` → "Error events". A kind the server adds before this list
 * learns it falls back to the slug, spaced, rather than vanishing.
 */
export function purgeKindName(slug: string): string {
  const label = labelFor(KIND_NAMES, slug);
  return label === slug ? slug.replace(/_/g, ' ') : label;
}
