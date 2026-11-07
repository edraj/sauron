import { formatNumber, labelFor, t, tn, type MessageKey } from '../i18n';
import type { IngestFailure } from '../api/ingest-failures';

/**
 * Presentation logic for the ingest-failure page, kept out of the component so
 * the rules that matter — above all, how recoverability is described — can be
 * tested without rendering.
 */

/**
 * Catalog keys for the backend's `error_kind` slugs (classify.rs's `kind`).
 * Keys rather than strings so the label follows the active language.
 */
export const KIND_LABELS: Record<string, MessageKey> = {
  decode: 'failures.kind.decode',
  db_contention: 'failures.kind.dbContention',
  db_unavailable: 'failures.kind.dbUnavailable',
  db_fk_violation: 'failures.kind.dbFkViolation',
  db_constraint: 'failures.kind.dbConstraint',
  redis: 'failures.kind.redis',
  unknown: 'failures.kind.unknown',
};

/** Human label for a kind; an unknown slug prints as sent. Call at render time. */
export function describeKind(kind: string): string {
  return labelFor(KIND_LABELS, kind);
}

/**
 * Whether a kind is one the pipeline would have retried automatically.
 *
 * Mirrors `classify.rs`. Used only to explain to the operator why a group has
 * `attempts: 0` — it is not a second copy of the policy, and the page never
 * decides retry behaviour from it.
 */
export function wasAutoRetried(kind: string): boolean {
  return kind === 'db_contention' || kind === 'db_unavailable' || kind === 'redis';
}

export type RecoveryLevel = 'full' | 'partial' | 'none';

export interface Recovery {
  level: RecoveryLevel;
  /** Sentence shown next to the Retry control. Never omits a loss. */
  summary: string;
}

/**
 * How much of a group Retry can actually bring back.
 *
 * This is the most important function on the page. A group whose payload cap
 * was exceeded can replay only what was retained, and an operator who reads
 * "Retry" as "recover everything" will believe a mass failure was resolved when
 * most of it is permanently gone. So the loss is stated in the summary itself,
 * in whole numbers, rather than being implied by two counts sitting in adjacent
 * columns.
 */
export function describeRecovery(f: Pick<IngestFailure, 'occurrences' | 'retained' | 'dropped'>): Recovery {
  // Counts go through `tn`, so "1 event" / "3 events" agree in number in
  // English and take the right one of Arabic's six forms.
  if (f.retained === 0) {
    return {
      level: 'none',
      summary:
        f.occurrences === 0
          ? t('failures.recovery.nothing')
          : t('failures.recovery.none', {
              occurrences: tn('common.plural.occurrence', f.occurrences),
            }),
    };
  }
  if (f.dropped <= 0) {
    return {
      level: 'full',
      summary: t('failures.recovery.full', { events: tn('common.plural.event', f.retained) }),
    };
  }
  return {
    level: 'partial',
    summary: t('failures.recovery.partial', {
      retained: fmt(f.retained),
      occurrences: fmt(f.occurrences),
      dropped: tn('common.plural.event', f.dropped),
    }),
  };
}

/**
 * Badge tone for a group's status.
 *
 * Returns the house `Badge` vocabulary (`error`, not `danger`) — the component
 * silently renders an unknown tone as unstyled text, so a near-miss name is a
 * defect that looks like a working page.
 */
export function statusTone(status: string): 'error' | 'warning' | 'success' | 'neutral' {
  switch (status) {
    case 'failed':
      return 'error';
    case 'requeued':
      return 'warning';
    case 'resolved':
      return 'success';
    default:
      return 'neutral';
  }
}

/** Thousands separators, so six-figure occurrence counts stay readable. */
export function fmt(n: number): string {
  return formatNumber(n);
}

/**
 * Collapse a long error message to one line for the table.
 *
 * The full text stays available in the drill-down; a serde error can run to
 * kilobytes and would otherwise blow the row height apart.
 */
export function shortMessage(msg: string, max = 120): string {
  const oneLine = msg.replace(/\s+/g, ' ').trim();
  return oneLine.length > max ? `${oneLine.slice(0, max - 1)}…` : oneLine;
}

/** First 8 chars of the group key — enough to correlate with a log line. */
export function shortFingerprint(fp: string): string {
  return fp.slice(0, 8);
}
