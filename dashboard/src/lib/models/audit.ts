import type { AuditEntry, AuditFilters } from '../api/audit';

/**
 * Presentation rules for the Wall of Shame.
 *
 * Split out of the page so the parts worth pinning — the verb table, the
 * URL round trip, the diff renderer's handling of nulls and objects — are
 * testable without mounting a component.
 */

/** How prominently an action should read in the feed. */
export type ActionTone = 'destructive' | 'credential' | 'neutral';

/** `entity.verb` → human phrase. Verb half only; the entity half is derived. */
const VERBS: Record<string, string> = {
  create: 'Created',
  update: 'Updated',
  delete: 'Deleted',
  retire: 'Retired',
  activate: 'Activated',
  deactivate: 'Deactivated',
  reset_password: 'Reset password for',
  revoke_sessions: 'Revoked sessions for',
  rotate_key: 'Rotated ingest key for',
  enrollment_update: 'Updated enrollment for',
  upload: 'Uploaded',
  upsert: 'Configured',
  sync: 'Queued sync for',
  test: 'Sent test through',
  reveal: 'Revealed PII in',
  mask: 'Masked PII in',
  mask_preview: 'Previewed PII mask for',
  release: 'Released',
  extend: 'Extended',
  // Auth. These read as complete phrases on their own — see the `auth`
  // special case in `describeAction`.
  login: 'Signed in',
  login_failed: 'Failed sign-in',
  logout: 'Signed out',
  password_change: 'Changed own password',
};

/** `entity_type`/entity half → the noun shown after the verb. */
const NOUNS: Record<string, string> = {
  org: 'organization',
  project: 'project',
  app: 'app',
  environment: 'environment',
  member: 'member',
  role: 'role',
  grant: 'access grant',
  alert_rule: 'alert rule',
  alert_channel: 'alert channel',
  monitor: 'monitor',
  artifact: 'artifact',
  store: 'store connection',
  inspector_policy: 'privacy policy',
  tier_policy: 'tier policy',
  tier_restore: 'cold-data restore',
  tier_pin: 'restore pin',
  pii: 'data',
};

/**
 * Actions that destroy something or move a credential.
 *
 * Drives nothing but emphasis — but that emphasis is the point of the page:
 * a deletion and a rename must not look identical in a list of two hundred
 * rows.
 */
const DESTRUCTIVE = new Set(['delete', 'retire', 'release']);
const CREDENTIAL = new Set([
  'reset_password',
  'revoke_sessions',
  'rotate_key',
  'reveal',
  'mask',
  // A burst of these against one account is the thing worth spotting in a
  // wall of two hundred rows.
  'login_failed',
  'password_change',
]);

export interface DescribedAction {
  label: string;
  tone: ActionTone;
}

/**
 * Turn `environment.rotate_key` into "Rotated ingest key for environment".
 *
 * Falls back to the raw action string rather than throwing or rendering
 * "undefined": a backend that adds an action before the dashboard learns
 * about it should degrade to something readable, not to a blank cell.
 */
export function describeAction(action: string): DescribedAction {
  const idx = action.indexOf('.');
  if (idx <= 0 || idx === action.length - 1) {
    return { label: action, tone: 'neutral' };
  }
  const entity = action.slice(0, idx);
  const verb = action.slice(idx + 1);

  const tone: ActionTone = DESTRUCTIVE.has(verb)
    ? 'destructive'
    : CREDENTIAL.has(verb)
      ? 'credential'
      : 'neutral';

  const verbLabel = VERBS[verb];
  // Auth actions are about the actor, not about a thing they acted on, so they
  // carry no noun — "Signed in", not "Signed in auth".
  if (entity === 'auth') return { label: verbLabel ?? action, tone };
  const noun = NOUNS[entity];
  if (!verbLabel || !noun) return { label: action, tone };
  return { label: `${verbLabel} ${noun}`, tone };
}

/** `interval_seconds` → "Interval seconds". */
export function formatFieldName(field: string): string {
  const spaced = field.replace(/_/g, ' ');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

/**
 * Render one side of a diff for display.
 *
 * `null`/`undefined` become an em dash rather than the strings "null" and
 * "undefined" — on a create the whole `from` column is null, and a column of
 * the literal word "null" reads as data rather than absence.
 */
export function formatValue(value: unknown): string {
  if (value === null || value === undefined) return '—';
  if (typeof value === 'boolean') return value ? 'yes' : 'no';
  if (typeof value === 'string') return value === '' ? '(empty)' : value;
  if (Array.isArray(value)) return value.length === 0 ? '(none)' : value.join(', ');
  if (typeof value === 'object') return JSON.stringify(value);
  return String(value);
}

/** Where the action happened, as a single readable string. */
export function describeScope(entry: AuditEntry): string {
  const parts = [entry.project_name, entry.app_name, entry.environment_name].filter(
    (p) => p && p.length > 0,
  );
  return parts.join(' / ');
}

/** The named date ranges the filter bar offers. */
export const RANGES = [
  { key: '24h', label: 'Last 24 hours', hours: 24 },
  { key: '7d', label: 'Last 7 days', hours: 24 * 7 },
  { key: '30d', label: 'Last 30 days', hours: 24 * 30 },
  { key: '90d', label: 'Last 90 days', hours: 24 * 90 },
  { key: 'all', label: 'All time', hours: null },
] as const;

export type RangeKey = (typeof RANGES)[number]['key'];

/** Default range. Bounds the first paint on an org with a long history. */
export const DEFAULT_RANGE: RangeKey = '7d';

/**
 * `from` for a named range, as RFC3339. `null` for "all time", which the API
 * reads as "no lower bound" rather than as the epoch.
 */
export function rangeStart(key: RangeKey, now: Date = new Date()): string | null {
  const range = RANGES.find((r) => r.key === key);
  if (!range || range.hours === null) return null;
  return new Date(now.getTime() - range.hours * 3600 * 1000).toISOString();
}

export interface FilterState extends AuditFilters {
  range: RangeKey;
  /** Show sign-in activity alongside administrative actions. */
  include_auth: boolean;
}

export const EMPTY_FILTERS: FilterState = {
  range: DEFAULT_RANGE,
  include_auth: false,
  project_id: null,
  app_id: null,
  environment_id: null,
  actor_id: null,
  action: null,
  entity_type: null,
};

/**
 * A facet list guaranteed to contain `selected`.
 *
 * `<select bind:value>` resets its binding to `null` when the bound value is
 * not among its `<option>`s — and the facets that build those options arrive
 * asynchronously, one request AFTER the filters are hydrated from the URL. So
 * a deep link to `?action=role.update` rendered a select that did not yet
 * contain that option, the binding wrote `null` straight back into the filter
 * state, that write retriggered the load, the reply replaced the option list,
 * and the page never settled — an infinite request loop with the spinner
 * stuck on. No deep link to any filter value could work.
 *
 * Pinning the selected value as an option closes that, and is independently
 * right: the trail can name a project or actor that no longer exists, and the
 * dropdown must still be able to show what is currently being filtered on.
 */
export function withSelected(
  options: Array<{ id: string | null; label: string }>,
  selected: string | null | undefined,
  labelFor: (value: string) => string = (v) => v,
): Array<{ id: string | null; label: string }> {
  if (!selected) return options;
  if (options.some((o) => (o.id ?? o.label) === selected)) return options;
  return [{ id: selected, label: labelFor(selected) }, ...options];
}

/**
 * Semantic equality of two filter sets.
 *
 * Compare filter STATES, never their encoded query strings. `filtersToQuery`
 * emits keys in a fixed order, but a URL a user pasted or bookmarked may hold
 * them in any order — so `?action=x&range=30d` and `?range=30d&action=x` are
 * the same view and different strings. Comparing the strings makes the
 * URL→state and state→URL effects permanently disagree: each sees a
 * difference, each rewrites, and the page reloads forever with the spinner
 * stuck on. This is the same trap `DevicesInventory`'s `sameGroupKey`
 * documents one level down.
 */
export function sameFilters(a: FilterState, b: FilterState): boolean {
  return (
    a.range === b.range &&
    !!a.include_auth === !!b.include_auth &&
    (a.project_id ?? null) === (b.project_id ?? null) &&
    (a.app_id ?? null) === (b.app_id ?? null) &&
    (a.environment_id ?? null) === (b.environment_id ?? null) &&
    (a.actor_id ?? null) === (b.actor_id ?? null) &&
    (a.action ?? null) === (b.action ?? null) &&
    (a.entity_type ?? null) === (b.entity_type ?? null)
  );
}

/** True when nothing is narrowing the feed beyond the default range. */
export function isDefaultFilter(f: FilterState): boolean {
  return (
    f.range === DEFAULT_RANGE &&
    !f.include_auth &&
    !f.project_id &&
    !f.app_id &&
    !f.environment_id &&
    !f.actor_id &&
    !f.action &&
    !f.entity_type
  );
}

/**
 * Serialize filters into a URL query string so a filtered view is linkable.
 *
 * Only non-empty values travel, so a default view produces `''` and the
 * address bar stays clean.
 */
export function filtersToQuery(f: FilterState): string {
  const p = new URLSearchParams();
  if (f.range !== DEFAULT_RANGE) p.set('range', f.range);
  if (f.include_auth) p.set('include_auth', '1');
  for (const key of [
    'project_id',
    'app_id',
    'environment_id',
    'actor_id',
    'action',
    'entity_type',
  ] as const) {
    const v = f[key];
    if (v) p.set(key, v);
  }
  return p.toString();
}

/** Inverse of {@link filtersToQuery}. Unknown ranges fall back to the default. */
export function filtersFromQuery(query: string): FilterState {
  const p = new URLSearchParams(query);
  const rawRange = p.get('range');
  const range = RANGES.some((r) => r.key === rawRange) ? (rawRange as RangeKey) : DEFAULT_RANGE;
  return {
    range,
    include_auth: p.get('include_auth') === '1',
    project_id: p.get('project_id'),
    app_id: p.get('app_id'),
    environment_id: p.get('environment_id'),
    actor_id: p.get('actor_id'),
    action: p.get('action'),
    entity_type: p.get('entity_type'),
  };
}

/** The filter payload the API takes, with `from` resolved from the range. */
export function toApiFilters(f: FilterState, now: Date = new Date()): AuditFilters {
  return {
    project_id: f.project_id,
    app_id: f.app_id,
    environment_id: f.environment_id,
    actor_id: f.actor_id,
    action: f.action,
    entity_type: f.entity_type,
    include_auth: f.include_auth,
    from: rangeStart(f.range, now),
  };
}
