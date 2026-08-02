/**
 * Pure decision logic for personal notification subscriptions.
 *
 * DOM-free on purpose: there is no DOM test environment in this project, so
 * anything a `.svelte` file decides is untestable. The `.svelte` files render;
 * this file decides.
 */
import type {
  NotificationSubscription,
  SubscriptionConditions,
  SubscriptionDelivery,
  SubscriptionKind,
} from './index';
import type { ScopeSelection } from './scope-tree';

/** These duplicate the backend clamps exactly; a mismatch is drift, not style. */
export const COND_DEFAULTS = {
  window_seconds: 900,
  factor: 3,
  min_count: 10,
} as const;
export const COND_CLAMPS = {
  window_seconds: [300, 86400],
  factor: [1.5, 100],
  min_count: [1, 100000],
} as const;
export const MAX_THROTTLE_SECONDS = 604800;

export type ScopeResult =
  | { ok: true; scope_type: 'project' | 'app'; scope_id: string }
  | { ok: false; reason: string };

/**
 * Collapse a `ScopeTree` selection into the single scope a subscription can
 * carry.
 *
 * A subscription is one row per scope, not a collapsed grant set, so
 * `grant-plan.ts`'s coverage-diff machinery is deliberately not reused — a
 * multi-node selection is refused rather than merged.
 */
export function selectionToSubscriptionScope(sel: ScopeSelection): ScopeResult {
  if (sel.org) {
    return { ok: false, reason: 'Pick one project or one app.' };
  }
  if (sel.envs.length > 0) {
    // ScopeTree's env rows are `AppEnvironment.id` — ENROLLMENT ids — while a
    // subscription stores CATALOGUE ids in a separate chip row. Rejecting
    // rather than ignoring is what makes a regression that re-enables the env
    // level fail loudly instead of storing the wrong id space.
    return { ok: false, reason: 'Pick one project or one app.' };
  }
  const picked = sel.projects.length + sel.apps.length;
  if (picked !== 1) {
    return { ok: false, reason: 'Pick one project or one app.' };
  }
  if (sel.projects.length === 1) {
    return { ok: true, scope_type: 'project', scope_id: sel.projects[0] };
  }
  return { ok: true, scope_type: 'app', scope_id: sel.apps[0] };
}

/** `monitors` carries only `project_id`, so uptime has nothing to narrow on. */
export function kindSupportsEnvFilter(kind: SubscriptionKind): boolean {
  return kind !== 'uptime';
}

export function kindScopeTypes(kind: SubscriptionKind): ('project' | 'app')[] {
  return kind === 'uptime' ? ['project'] : ['project', 'app'];
}

function clampNumber(value: number | undefined, fallback: number, lo: number, hi: number): number {
  if (value === undefined || value === null || !Number.isFinite(value)) return fallback;
  return Math.min(hi, Math.max(lo, value));
}

export function clampConditions(
  kind: SubscriptionKind,
  raw: Partial<SubscriptionConditions>,
): SubscriptionConditions {
  const defaultLevel =
    kind === 'error_new_issue' || kind === 'error_regression' ? 'error' : null;
  return {
    window_seconds: clampNumber(
      raw.window_seconds,
      COND_DEFAULTS.window_seconds,
      COND_CLAMPS.window_seconds[0],
      COND_CLAMPS.window_seconds[1],
    ),
    factor: clampNumber(
      raw.factor,
      COND_DEFAULTS.factor,
      COND_CLAMPS.factor[0],
      COND_CLAMPS.factor[1],
    ),
    min_count: clampNumber(
      raw.min_count,
      COND_DEFAULTS.min_count,
      COND_CLAMPS.min_count[0],
      COND_CLAMPS.min_count[1],
    ),
    level: raw.level === undefined ? defaultLevel : raw.level,
  };
}

export function describeSubscription(s: NotificationSubscription): string {
  const noun = s.scope_type === 'project' ? 'Project' : 'App';
  // `scope_id` has no foreign key, so the target can be gone. Say so instead of
  // rendering a bare uuid nobody can act on.
  return s.scope_name ? `${noun} “${s.scope_name}”` : `${noun} (deleted)`;
}

function hhmm(minuteOfDay: number): string {
  const h = Math.floor(minuteOfDay / 60);
  const m = minuteOfDay % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
}

/**
 * Renders the EFFECTIVE zone, which is what the enqueue actually used — a zone
 * the server does not know falls back to UTC there, and this is where a user
 * would notice.
 */
export function quietHoursLabel(
  start: number | null,
  end: number | null,
  tz: string,
): string {
  if (start === null || end === null) return 'Always on';
  return `${hhmm(start)} – ${hhmm(end)} (${tz})`;
}

export interface SubscriptionDraft {
  kind: SubscriptionKind;
  selection: ScopeSelection;
  environmentIds: string[];
  conditions: Partial<SubscriptionConditions>;
  delivery: SubscriptionDelivery;
  throttleSeconds: number;
  quietStartMin: number | null;
  quietEndMin: number | null;
  quietTz: string;
}

/** Every reason the save button is disabled, in the order they are shown. */
export function validateSubscription(input: SubscriptionDraft): string[] {
  const reasons: string[] = [];
  const scope = selectionToSubscriptionScope(input.selection);
  if (!scope.ok) {
    reasons.push(scope.reason);
  } else if (!kindScopeTypes(input.kind).includes(scope.scope_type)) {
    reasons.push('Uptime subscriptions are project-scoped.');
  }
  if ((input.quietStartMin === null) !== (input.quietEndMin === null)) {
    reasons.push('Set both a quiet-hours start and end, or neither.');
  }
  for (const v of [input.quietStartMin, input.quietEndMin]) {
    if (v !== null && (v < 0 || v > 1439)) {
      reasons.push('Quiet hours must be times of day.');
      break;
    }
  }
  if (
    !Number.isFinite(input.throttleSeconds) ||
    input.throttleSeconds < 0 ||
    input.throttleSeconds > MAX_THROTTLE_SECONDS
  ) {
    reasons.push(`Throttle must be between 0 and ${MAX_THROTTLE_SECONDS} seconds.`);
  }
  if (!input.quietTz.trim()) {
    reasons.push('Pick a timezone for quiet hours.');
  }
  return reasons;
}
