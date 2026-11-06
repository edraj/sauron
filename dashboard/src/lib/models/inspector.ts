// Pure decision logic for the PII inspector. No Svelte, no DOM — vitest is
// node-only in this repo, so anything that needs a test lives here.

import type { InspectorMaskAction, InspectorScan, InspectorTrackedKey } from './index';
import { labelFor, t, type MessageKey } from '../i18n';

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
  /** Catalog keys, translated at render time with `t()`. */
  whatKey: MessageKey;
  whyKey: MessageKey;
  boundedKey: MessageKey;
}

/**
 * What "permanently masked" does NOT mean.
 *
 * ONE data array, rendered verbatim in the MaskDialog, in the Audit tab detail
 * and in the wiki, so support answers and the product cannot diverge. The
 * product must never claim a mask is permanent: in twelve named places the
 * promise does not hold — eleven where the bytes survive, and one where the
 * mask silently takes something else away with it.
 *
 * The text lives in the catalog (`inspector.unreachable.*`) so the Arabic UI
 * renders it too; each row carries keys, never pre-translated strings, so a
 * language switch re-renders it.
 */
export const UNREACHABLE_COPY: UnreachableRow[] = [
  {
    headline: true,
    whatKey: 'inspector.unreachable.headline.what',
    whyKey: 'inspector.unreachable.headline.why',
    boundedKey: 'inspector.unreachable.headline.bounded',
  },
  {
    whatKey: 'inspector.unreachable.coldParquet.what',
    whyKey: 'inspector.unreachable.coldParquet.why',
    boundedKey: 'inspector.unreachable.coldParquet.bounded',
  },
  {
    whatKey: 'inspector.unreachable.beyondHot.what',
    whyKey: 'inspector.unreachable.beyondHot.why',
    boundedKey: 'inspector.unreachable.beyondHot.bounded',
  },
  {
    whatKey: 'inspector.unreachable.ingestStream.what',
    whyKey: 'inspector.unreachable.ingestStream.why',
    boundedKey: 'inspector.unreachable.ingestStream.bounded',
  },
  {
    // Every clause of the previous text became false once the bounded DLQ, its
    // reaper, and the ingest_failures table landed. A privacy page that reports
    // a closed hazard as open is worse than one that omits it: it spends the
    // reader's attention on a problem that no longer exists.
    whatKey: 'inspector.unreachable.failedIngest.what',
    whyKey: 'inspector.unreachable.failedIngest.why',
    boundedKey: 'inspector.unreachable.failedIngest.bounded',
  },
  {
    whatKey: 'inspector.unreachable.breadcrumbs.what',
    whyKey: 'inspector.unreachable.breadcrumbs.why',
    boundedKey: 'inspector.unreachable.breadcrumbs.bounded',
  },
  {
    whatKey: 'inspector.unreachable.alertEvents.what',
    whyKey: 'inspector.unreachable.alertEvents.why',
    boundedKey: 'inspector.unreachable.alertEvents.bounded',
  },
  {
    whatKey: 'inspector.unreachable.deliveredAlerts.what',
    whyKey: 'inspector.unreachable.deliveredAlerts.why',
    boundedKey: 'inspector.unreachable.deliveredAlerts.bounded',
  },
  {
    whatKey: 'inspector.unreachable.eventUsers.what',
    whyKey: 'inspector.unreachable.eventUsers.why',
    boundedKey: 'inspector.unreachable.eventUsers.bounded',
  },
  {
    whatKey: 'inspector.unreachable.devices.what',
    whyKey: 'inspector.unreachable.devices.why',
    boundedKey: 'inspector.unreachable.devices.bounded',
  },
  {
    whatKey: 'inspector.unreachable.sourceLines.what',
    whyKey: 'inspector.unreachable.sourceLines.why',
    boundedKey: 'inspector.unreachable.sourceLines.bounded',
  },
  {
    whatKey: 'inspector.unreachable.backups.what',
    whyKey: 'inspector.unreachable.backups.why',
    boundedKey: 'inspector.unreachable.backups.bounded',
  },
  {
    readFirst: true,
    whatKey: 'inspector.unreachable.activeUsers.what',
    whyKey: 'inspector.unreachable.activeUsers.why',
    boundedKey: 'inspector.unreachable.activeUsers.bounded',
  },
];

/**
 * "error_events.extra → customer.email". Table, column and path are wire
 * values; only the empty-path phrase is translated, so call this from markup.
 */
export function describeTarget(target: MaskTargetView): string {
  const path = target.path === '' ? t('inspector.describeTarget.wholeValue') : target.path;
  return `${target.table}.${target.column} → ${path}`;
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
 *
 * Returns translated text: call it inside `$derived` or markup so the reason
 * follows a language switch.
 */
export function createPolicyBlockedReason(
  targetId: string | null,
  keys: InspectorTrackedKey[],
  detectors: string[],
): string | null {
  // Names the field's own visible label ("Target"), not an internal concept —
  // a reason that points at a control the user cannot find is not a reason.
  if (!targetId) return t('inspector.blocked.noTarget');
  if (keys.length === 0 && detectors.length === 0) {
    return t('inspector.blocked.noMatchers');
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

/**
 * The privacy inspector's four sections, and the URL that addresses each.
 *
 * The tab used to be plain component state, which meant it could not be linked
 * to, Back could not undo it, and every reload — including the one after an
 * action that jumps you to Audit — landed back on Findings. Reading it out of
 * the query string instead makes the URL the single source of truth, so there
 * is no second copy to keep in sync.
 */
export const INSPECTOR_TABS = ['findings', 'policy', 'scans', 'audit'] as const;
export type InspectorTab = (typeof INSPECTOR_TABS)[number];

/**
 * `?tab=` → a tab, defaulting to `findings`.
 *
 * Unknown values fall back rather than erroring: this reads a URL anyone can
 * hand-edit, and a stale bookmark from before a tab was renamed should open the
 * page, not break it.
 */
export function inspectorTabFromQuery(qs: string | null | undefined): InspectorTab {
  const raw = new URLSearchParams(qs ?? '').get('tab');
  return (INSPECTOR_TABS as readonly string[]).includes(raw ?? '')
    ? (raw as InspectorTab)
    : 'findings';
}

/**
 * The route to navigate to for `tab`, preserving any other query params.
 *
 * `findings` drops the parameter instead of writing `?tab=findings`: it is the
 * default, and a canonical URL for the page's initial state keeps the entry
 * someone copies out of the address bar as short as what they arrived with.
 */
export function inspectorTabRoute(tab: InspectorTab, qs: string | null | undefined): string {
  const params = new URLSearchParams(qs ?? '');
  if (tab === 'findings') {
    params.delete('tab');
  } else {
    params.set('tab', tab);
  }
  const query = params.toString();
  return query ? `/admin/privacy?${query}` : '/admin/privacy';
}

// --- human words for API values ---------------------------------------------
// Every status below reaches the screen in a badge; the raw lowercase API value
// ("succeeded", "app_env") is the server's vocabulary, not the reader's. A value
// the server adds before these tables learn it prints as sent.

const SCAN_STATUS_KEYS: Record<InspectorScan['status'], MessageKey> = {
  queued: 'inspector.scanStatus.queued',
  running: 'inspector.scanStatus.running',
  succeeded: 'inspector.scanStatus.succeeded',
  failed: 'inspector.scanStatus.failed',
  cancelled: 'inspector.scanStatus.cancelled',
};

const MASK_STATUS_KEYS: Record<InspectorMaskAction['status'], MessageKey> = {
  preview: 'inspector.maskStatus.preview',
  previewed: 'inspector.maskStatus.previewed',
  pending: 'inspector.maskStatus.pending',
  running: 'inspector.maskStatus.running',
  cancelling: 'inspector.maskStatus.cancelling',
  done: 'inspector.maskStatus.done',
  failed: 'inspector.maskStatus.failed',
  cancelled: 'inspector.maskStatus.cancelled',
};

const TARGET_TYPE_KEYS: Record<string, MessageKey> = {
  project: 'inspector.targetType.project',
  app: 'inspector.targetType.app',
  app_env: 'inspector.targetType.appEnv',
};

export function scanStatusLabel(s: InspectorScan['status']): string {
  return labelFor(SCAN_STATUS_KEYS, s);
}

export function maskStatusLabel(s: InspectorMaskAction['status']): string {
  return labelFor(MASK_STATUS_KEYS, s);
}

export function targetTypeLabel(tt: string): string {
  return labelFor(TARGET_TYPE_KEYS, tt);
}
