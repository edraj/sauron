// Pure decision logic for the PII inspector. No Svelte, no DOM — vitest is
// node-only in this repo, so anything that needs a test lives here.

import type { InspectorTrackedKey } from './index';

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
    what: 'Failed ingest',
    // Every clause of the previous text became false once the bounded DLQ, its
    // reaper, and the ingest_failures table landed. A privacy page that reports
    // a closed hazard as open is worse than one that omits it: it spends the
    // reader's attention on a problem that no longer exists.
    why: 'Events that fail to persist are retained as masked copies — in ingest_failures, and in sauron:ingest:dlq when even that write fails.',
    bounded: 'INGEST_FAILURE_RETENTION_DAYS (30d) and INGEST_DLQ_RETENTION_HOURS (7d).',
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

/**
 * Turn a free-typed key list into the wire shape.
 *
 * Separators are commas AND any whitespace, because the realistic input is a
 * paste from a spec document rather than a tidy CSV.
 *
 * Lowercasing here is not cosmetic: the backend's `normalize_key` is trim +
 * lowercase applied at policy write AND at match time, so sending `Email`
 * stores `email` and the chip list would render a key the user never typed.
 * Doing it up front keeps the optimistic UI honest.
 *
 * Deduping is ours alone — `parse_tracked_keys` happily stores the same key
 * twice, which does not error but does double that key's reported match count.
 */
export function parseKeyInput(raw: string): InspectorTrackedKey[] {
  const seen = new Set<string>();
  const out: InspectorTrackedKey[] = [];
  for (const piece of raw.split(/[\s,]+/)) {
    const key = piece.trim().toLowerCase();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    // `scope: 'any'` matches the key at any depth, which is what the existing
    // add-a-key form on this tab sends. `top` is not offered at creation
    // time: narrowing to top level is an optimisation you make after a scan
    // shows where the key actually lives, not a first guess.
    out.push({ key, scope: 'any' });
  }
  return out;
}

/**
 * The enrollment the environment picker should start on.
 *
 * A `<select>` whose bound value matches no `<option>` renders with
 * `selectedIndex === -1` — visually BLANK. Pairing that with a "just use the
 * first one" fallback in the submit path is the silent-mismatch bug this
 * codebase keeps re-learning: the control shows nothing, the request carries
 * production, and the policy lands on an environment the operator never saw
 * named. Seeding the bound value instead keeps the visible choice and the
 * submitted id the same fact.
 *
 * Returns `null` for an app with no live enrollments, which is a real state —
 * every environment retired — and must disable the scope rather than guess.
 */
export function defaultEnvEnrollmentId(
  envs: { id: string; is_default: boolean }[],
): string | null {
  if (envs.length === 0) return null;
  return (envs.find((e) => e.is_default) ?? envs[0]).id;
}

/**
 * Why the create button is disabled, or `null` when it may be pressed.
 *
 * Mirrors the two hard 400s in `create_policy`/`normalize_matchers` so the
 * form explains itself instead of round-tripping to an error toast. The
 * matcher-less message repeats the BACKEND's reasoning rather than saying
 * "required": a policy with neither keys nor detectors is not merely invalid,
 * it scans nothing and finishes `succeeded` with `coverage='full'` and zero
 * findings — a confident false negative, which is the worst thing a privacy
 * scan can emit.
 */
export function createPolicyBlockedReason(
  targetId: string | null,
  keys: InspectorTrackedKey[],
  detectors: string[],
): string | null {
  // Names the field's own visible label ("Target"), not an internal concept —
  // a reason that points at a control the user cannot find is not a reason.
  if (!targetId) return 'Choose the target this policy covers.';
  if (keys.length === 0 && detectors.length === 0) {
    return 'Add at least one tracked key or enable one detector — a policy with neither scans nothing and reports no findings, which reads as "clean".';
  }
  return null;
}

export function csvFilename(
  kind: 'findings' | 'mask-actions',
  scope: string,
  from: string,
  to: string,
): string {
  return `sauron-inspector-${kind}_${scope}_${from}_${to}.csv`;
}
