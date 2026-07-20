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

// `environment` options are injected at runtime (loaded from the environments API).
export const EVENT_FIELDS: FieldDef[] = [
  { key: 'name', label: 'Event', type: 'string', ops: OPS_STR },
  { key: 'distinct_id', label: 'User', type: 'string', ops: OPS_STR },
  { key: 'session_id', label: 'Session', type: 'string', ops: OPS_STR },
  { key: 'environment', label: 'Environment', type: 'enum', ops: OPS_ENUM, options: [] },
  { key: 'release', label: 'Release', type: 'string', ops: OPS_STR },
  { key: 'tag', label: 'Tag', type: 'tag', ops: OPS_TAG },
];

// Issue-detail occurrences: only the per-event `tag` is filterable.
export const OCCURRENCE_FIELDS: FieldDef[] = [
  { key: 'tag', label: 'Tag', type: 'tag', ops: OPS_TAG },
];
