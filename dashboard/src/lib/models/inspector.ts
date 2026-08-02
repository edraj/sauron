// Pure decision logic for the PII inspector. No Svelte, no DOM — vitest is
// node-only in this repo, so anything that needs a test lives here.

export interface MaskTargetView {
  table: string;
  column: string;
  path: string;
}

export interface UnreachableRow {
  /** The first entry is the headline, rendered above the enumerated rows. */
  headline?: boolean;
  /** Rendered in bold before confirm is enabled. */
  readFirst?: boolean;
  what: string;
  why: string;
  bounded: string;
}

/**
 * What "permanently masked" does NOT mean.
 *
 * ONE data array, rendered verbatim in the MaskDialog, in the Audit tab detail
 * and in the wiki, so support answers and the product cannot diverge. The
 * product must never claim a mask is permanent: in twelve named places the
 * promise does not hold — eleven where the bytes survive, and one where the
 * mask silently takes something else away with it.
 */
export const UNREACHABLE_COPY: UnreachableRow[] = [
  {
    headline: true,
    what: 'Masking rewrites rows in hot Postgres only.',
    why: 'Everything below still holds the original bytes, or is outside this product’s reach.',
    bounded: 'Read the rows below before confirming.',
  },
  {
    what: 'Cold Parquet',
    why: 'The partition was exported before the mask ran. Parquet is immutable and, after the drop, the only copy.',
    bounded: 'Nothing. Permanent.',
  },
  {
    what: 'Postgres rows older than TIER_HOT_DAYS',
    why: 'The retro-mask deliberately stops at the hot boundary.',
    bounded: 'The tier drop, which destroys the row entirely.',
  },
  {
    what: 'The Redis ingest stream',
    why: 'sauron:ingest:stream holds the full serialized job.',
    bounded: 'XADD … MAXLEN ~ 1000000.',
  },
  {
    what: 'The Redis DLQ',
    why: 'sauron:ingest:dlq is XADD with no MAXLEN and no TTL, and no reaper exists. A payload that fails to deserialize still dead-letters raw.',
    bounded: 'Nothing. Permanent.',
  },
  {
    what: 'Per-person breadcrumbs in Redis',
    why: 'Up to 100 batches are buffered per person before an error arrives.',
    bounded: 'A 1800 s TTL.',
  },
  {
    what: 'alert_events.title / .body',
    why: 'They embed the issue title verbatim.',
    bounded: 'ALERT_EVENT_RETENTION_DAYS (90).',
  },
  {
    what: 'Already-delivered alerts',
    why: 'Email, Slack, Discord, Matrix, Telegram and webhook messages are gone from our control the moment they send.',
    bounded: 'Nothing.',
  },
  {
    what: 'event_users.properties',
    why: 'The identify() write merges with ||, which never removes keys. An at-rest mask is undone by the next identify().',
    bounded: 'Forward enforcement only, and only for keys in the mask set.',
  },
  {
    what: 'devices.*',
    why: 'Every column is COALESCE(EXCLUDED.x, devices.x) — a non-null incoming value always wins, and there is no wire field to enforce on.',
    bounded: 'Not offered: devices is not maskable at all.',
  },
  {
    what: 'Symbolicated source lines',
    why: 'Frames carry context_line / pre_context / post_context — verbatim customer source. Masking a JSON path never touches them.',
    bounded: 'Redacted from responses only, for callers without source:read.',
  },
  {
    what: 'Backups, WAL, replicas',
    why: 'Out of the product’s reach entirely.',
    bounded: 'Operator policy.',
  },
  {
    readFirst: true,
    what: 'The active-users report stops identifying anyone new through that key',
    why: 'The enforcer runs before the active-users pipeline stamps identified_at, so masking a key an app sends as context.user.id means the equality test never passes again. Nobody already stamped is un-identified, but everyone first seen afterwards arrives as a guest and never merges across apps, so the identified share decays with no discontinuity to notice.',
    bounded: 'Nothing. The bytes are gone, so it cannot be recomputed later.',
  },
];

export function describeTarget(t: MaskTargetView): string {
  return `${t.table}.${t.column} → ${t.path === '' ? 'the whole value' : t.path}`;
}

/**
 * Mirrors the backend's `expand_targets` so the dialog can describe the blast
 * radius before the server answers. The backend map is authoritative;
 * `inspector.test.ts` and the Rust `targets.rs` tests assert the same pairs.
 */
export function expandCompanionTargets(t: MaskTargetView): MaskTargetView[] {
  const out: MaskTargetView[] = [{ ...t }];
  const push = (m: MaskTargetView) => {
    if (!out.some((x) => x.table === m.table && x.column === m.column && x.path === m.path)) {
      out.push(m);
    }
  };
  if (t.table === 'error_events' && t.column === 'title') {
    // error_events.title is derived server-side and has NO wire field, so
    // forward enforcement reaches it only through its inputs.
    push({ table: 'issues', column: 'title', path: '' });
    push({ table: 'error_events', column: 'exception_value', path: '' });
    push({ table: 'error_events', column: 'exception_type', path: '' });
    push({ table: 'error_events', column: 'message', path: '' });
  } else if (t.table === 'error_events' && t.column === 'culprit') {
    push({ table: 'issues', column: 'culprit', path: '' });
  } else if (t.table === 'error_events' && t.column === 'stacktrace') {
    push({ table: 'error_events', column: 'stacktrace_symbolicated', path: t.path });
  } else if (
    (t.table === 'error_events' || t.table === 'analytics_events') &&
    t.column === 'context'
  ) {
    // bump_session snapshots the same enriched jsonb on every event.
    push({ table: 'sessions', column: 'context', path: t.path });
  }
  return out;
}

export interface PreviewState {
  status: string;
  previewed_at: string | null;
  estimated_rows: number;
}

/**
 * Whether the danger button may be enabled.
 *
 * Typing the SLUG is the only confirmation that forces attention onto the
 * thing that actually goes wrong: the realistic failure is masking the WRONG
 * APP, not a mis-click. Case-sensitive, whitespace-trimmed.
 */
export function maskConfirmReady(
  typed: string,
  slug: string,
  preview: PreviewState,
  ttlSecs: number,
  maxRows: number,
): boolean {
  if (typed.trim() !== slug) return false;
  if (preview.status !== 'previewed' || !preview.previewed_at) return false;
  // The TTL runs from the preview COMPLETING, not from the request, or a
  // queued preview expires before it is readable.
  const ageSecs = (Date.now() - Date.parse(preview.previewed_at)) / 1000;
  if (!Number.isFinite(ageSecs) || ageSecs > ttlSecs) return false;
  return preview.estimated_rows <= maxRows;
}

export function csvFilename(
  kind: 'findings' | 'mask-actions',
  scope: string,
  from: string,
  to: string,
): string {
  return `sauron-inspector-${kind}_${scope}_${from}_${to}.csv`;
}
