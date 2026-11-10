import type { AuditEntry, AuditFilters } from '../api/audit';
import { t, type MessageKey } from '../i18n';

/**
 * Presentation rules for the Wall of Shame.
 *
 * Split out of the page so the parts worth pinning — the verb table, the
 * URL round trip, the diff renderer's handling of nulls and objects — are
 * testable without mounting a component.
 */

/** How prominently an action should read in the feed. */
export type ActionTone = 'destructive' | 'credential' | 'neutral';

/**
 * `entity.verb` → catalog key for the phrase, with a `{noun}` slot the entity
 * half fills. A template per verb, not a verb glued to a noun, so each
 * language places the noun where its grammar wants it.
 *
 * Keys rather than strings throughout this file: these are module-level
 * tables, and a `t()` call here would freeze the language at import time.
 */
const VERBS: Record<string, MessageKey> = {
  create: 'audit.verb.create',
  update: 'audit.verb.update',
  delete: 'audit.verb.delete',
  retire: 'audit.verb.retire',
  activate: 'audit.verb.activate',
  deactivate: 'audit.verb.deactivate',
  reset_password: 'audit.verb.resetPassword',
  email_change_request: 'audit.verb.emailChangeRequest',
  email_change_approved: 'audit.verb.emailChangeApproved',
  email_change_cancelled: 'audit.verb.emailChangeCancelled',
  revoke_sessions: 'audit.verb.revokeSessions',
  rotate_key: 'audit.verb.rotateKey',
  enrollment_update: 'audit.verb.enrollmentUpdate',
  upload: 'audit.verb.upload',
  upsert: 'audit.verb.upsert',
  sync: 'audit.verb.sync',
  test: 'audit.verb.test',
  reveal: 'audit.verb.reveal',
  mask: 'audit.verb.mask',
  mask_preview: 'audit.verb.maskPreview',
  release: 'audit.verb.release',
  extend: 'audit.verb.extend',
  revoke: 'audit.verb.revoke',
  settings_update: 'audit.verb.settingsUpdate',
};

/**
 * `auth.verb` → complete phrase. Auth actions are about the actor, not about a
 * thing they acted on, so they carry no noun — "Signed in", not "Signed in
 * auth".
 */
const AUTH_VERBS: Record<string, MessageKey> = {
  login: 'audit.auth.login',
  login_failed: 'audit.auth.loginFailed',
  logout: 'audit.auth.logout',
  password_change: 'audit.auth.passwordChange',
};

/** `entity_type`/entity half → the noun shown in the verb's `{noun}` slot. */
const NOUNS: Record<string, MessageKey> = {
  org: 'audit.noun.org',
  project: 'audit.noun.project',
  app: 'audit.noun.app',
  environment: 'audit.noun.environment',
  member: 'audit.noun.member',
  role: 'audit.noun.role',
  grant: 'audit.noun.grant',
  alert_rule: 'audit.noun.alertRule',
  alert_channel: 'audit.noun.alertChannel',
  monitor: 'audit.noun.monitor',
  artifact: 'audit.noun.artifact',
  store: 'audit.noun.store',
  inspector_policy: 'audit.noun.inspectorPolicy',
  tier_policy: 'audit.noun.tierPolicy',
  tier_restore: 'audit.noun.tierRestore',
  tier_pin: 'audit.noun.tierPin',
  pii: 'audit.noun.pii',
  access_key: 'audit.noun.accessKey',
};

/** Own-property lookup, so an action like `constructor.toString` cannot hit `Object.prototype`. */
function lookup(table: Record<string, MessageKey>, key: string): MessageKey | undefined {
  return Object.prototype.hasOwnProperty.call(table, key) ? table[key] : undefined;
}

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
  // All three move, or try to move, a login identity — `users.email` is what a
  // person signs in with. `email_change_cancelled` earns the emphasis most of
  // the three: with `cancelled_reason: 'user'` it is a member refusing an
  // admin's attempt, which is precisely the row worth spotting in a wall of two
  // hundred.
  'email_change_request',
  'email_change_approved',
  'email_change_cancelled',
  'revoke_sessions',
  'rotate_key',
  'reveal',
  'mask',
  // A burst of these against one account is the thing worth spotting in a
  // wall of two hundred rows.
  'login_failed',
  'password_change',
]);
/**
 * Whole actions, for credential moves whose verb alone is too common to
 * emphasise: `create` is usually a project or a role, but `access_key.create`
 * mints a personal API key that can read the org through MCP, and its revoke
 * is the row that says that access ended.
 */
const CREDENTIAL_ACTIONS = new Set(['access_key.create', 'access_key.revoke']);

export interface DescribedAction {
  label: string;
  tone: ActionTone;
}

/**
 * Turn `environment.rotate_key` into "Rotated ingest key for environment".
 * Translated on each call — call it from markup or a `$derived`.
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
    : CREDENTIAL.has(verb) || CREDENTIAL_ACTIONS.has(action)
      ? 'credential'
      : 'neutral';

  if (entity === 'auth') {
    const phrase = lookup(AUTH_VERBS, verb);
    return { label: phrase ? t(phrase) : action, tone };
  }
  const verbKey = lookup(VERBS, verb);
  const nounKey = lookup(NOUNS, entity);
  if (!verbKey || !nounKey) return { label: action, tone };
  return { label: t(verbKey, { noun: t(nounKey) }), tone };
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
  if (typeof value === 'boolean') return value ? t('audit.value.yes') : t('audit.value.no');
  if (typeof value === 'string') return value === '' ? t('audit.value.empty') : value;
  if (Array.isArray(value)) return value.length === 0 ? t('audit.value.none') : value.join(', ');
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
