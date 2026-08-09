export type Op = 'eq' | 'neq' | 'contains' | 'gt' | 'lt';
export type FieldType = 'enum' | 'string' | 'number' | 'tag';

export interface FieldDef {
  key: string;
  label: string;
  type: FieldType;
  ops: Op[];
  options?: string[]; // for type 'enum'
}

export interface Filter { field: string; op: Op; value: string; }

export const OP_LABEL: Record<Op, string> = {
  eq: '=', neq: '≠', contains: 'contains', gt: '>', lt: '<',
};

/** field:op:value — value is URL-encoded so ':' and other chars survive. */
export function encodeFilters(filters: Filter[]): string[] {
  return filters.map((f) => `${f.field}:${f.op}:${encodeURIComponent(f.value)}`);
}

/** Inverse of encodeFilters; drops any filter whose field/op is not in `fields`. */
export function parseFilters(raw: string[], fields: FieldDef[]): Filter[] {
  const out: Filter[] = [];
  for (const item of raw) {
    const i1 = item.indexOf(':');
    const i2 = item.indexOf(':', i1 + 1);
    if (i1 < 0 || i2 < 0) continue;
    const field = item.slice(0, i1);
    const op = item.slice(i1 + 1, i2) as Op;
    let value: string;
    try {
      value = decodeURIComponent(item.slice(i2 + 1));
    } catch {
      continue;
    }
    const def = fields.find((d) => d.key === field);
    if (!def || !def.ops.includes(op)) continue;
    out.push({ field, op, value });
  }
  return out;
}

/** Compose a tag key + value into the single `key=value` filter value slot. */
export function composeTag(key: string, value: string): string {
  return `${key}=${value}`;
}

/** Split a `key=value` tag filter value on the first `=` (inverse of composeTag). */
export function splitTag(v: string): { key: string; value: string } {
  const i = v.indexOf('=');
  if (i <= 0 || i === v.length - 1) return { key: '', value: '' };
  return { key: v.slice(0, i), value: v.slice(i + 1) };
}

const OPS_STR: Op[] = ['eq', 'neq', 'contains'];
const OPS_ENUM: Op[] = ['eq', 'neq'];
const OPS_NUM: Op[] = ['eq', 'gt', 'lt'];
// `contains` is first so it's the default op the FilterBar selects: "search by
// tag" is expected to be a forgiving, case-insensitive substring match. `eq`
// (exact JSONB containment) stays available for precise filtering.
const OPS_TAG: Op[] = ['contains', 'eq'];

export const ISSUE_FIELDS: FieldDef[] = [
  { key: 'level', label: 'Level', type: 'enum', ops: OPS_ENUM, options: ['debug', 'info', 'warning', 'error', 'fatal'] },
  { key: 'status', label: 'Status', type: 'enum', ops: OPS_ENUM, options: ['unresolved', 'resolved', 'ignored'] },
  { key: 'type', label: 'Type', type: 'string', ops: OPS_STR },
  { key: 'culprit', label: 'Culprit', type: 'string', ops: OPS_STR },
  { key: 'times_seen', label: 'Events', type: 'number', ops: OPS_NUM },
  { key: 'users_seen', label: 'Users', type: 'number', ops: OPS_NUM },
  { key: 'tag', label: 'Tag', type: 'tag', ops: OPS_TAG },
];

/**
 * Filter fields the API refuses outright without `event:read`, rather than
 * quietly narrowing.
 *
 * Mirrors `reject_body_filters` in `backend/bins/sauron-api/src/routes/issues.rs`.
 * Both entries are predicates over a column this caller may not read, so
 * answering them at all would turn the filter into an oracle — the backend
 * returns 403 with the reason instead.
 *
 * Needed on the client for one reason: that 403 is **permanent**, so a page
 * showing the standard Retry button offers a recovery that cannot work. With
 * this list a page can offer to drop the offending chip instead.
 *
 * `workflow` is listed even though `ISSUE_FIELDS` has no workflow chip and the
 * FilterBar therefore cannot produce one — the filter is still reachable through
 * a hand-written URL or a saved view, and a list that only covers what the UI
 * happens to offer today is the kind that goes stale silently.
 */
export const PERMISSION_GATED_FILTER_FIELDS = ['tag', 'workflow'] as const;

/** The gated fields present in `filters`, in the order they appear. */
export function gatedFilterFields(filters: Filter[]): string[] {
  const gated = new Set<string>(PERMISSION_GATED_FILTER_FIELDS);
  return [...new Set(filters.filter((f) => gated.has(f.field)).map((f) => f.field))];
}

// `environment` used to live here as a chip whose options were injected at
// runtime (loaded from the environments API). It's now scoped globally via
// the topbar environment switcher instead — see `sessionStore.currentEnvId`
// — so it's no longer a per-page filter field. The backend's legacy
// `filter=environment:eq:<name>` handling (`EVENT_FILTERS`) stays for API
// back-compatibility; this registry only drives the dashboard's FilterBar.
export const EVENT_FIELDS: FieldDef[] = [
  { key: 'name', label: 'Event', type: 'string', ops: OPS_STR },
  { key: 'distinct_id', label: 'User', type: 'string', ops: OPS_STR },
  { key: 'session_id', label: 'Session', type: 'string', ops: OPS_STR },
  { key: 'release', label: 'Release', type: 'string', ops: OPS_STR },
  { key: 'tag', label: 'Tag', type: 'tag', ops: OPS_TAG },
];

// Issue-detail occurrences: only the per-event `tag` is filterable.
export const OCCURRENCE_FIELDS: FieldDef[] = [
  { key: 'tag', label: 'Tag', type: 'tag', ops: OPS_TAG },
];
