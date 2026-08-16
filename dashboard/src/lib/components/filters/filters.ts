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

// Bounds of the `i64` the server parses numeric filter values into.
const I64_MIN = BigInt('-9223372036854775808');
const I64_MAX = BigInt('9223372036854775807');

/**
 * Whether `value` is acceptable for a `type: 'number'` field.
 *
 * Mirrors `FieldType::Num => value.parse::<i64>()` in
 * `backend/crates/sauron-db/src/filter.rs`: an optionally-signed run of
 * decimal digits that fits in an i64, and nothing else. Anything this rejects
 * comes back from the API as `FilterError::BadValue`, which fails the whole
 * request — so a chip that reaches the server must already satisfy this.
 *
 * Deliberately not a `Number()` check. `Number` accepts `1e3`, `3.5`, `0x10`
 * and `' 7 '`, every one of which Rust's `from_str` refuses, and it silently
 * loses precision past 2^53 where an i64 is still exact — so it would be
 * wrong in both directions. The comparison is done in BigInt for the same
 * reason.
 *
 * Signs are allowed because the server allows them. `times_seen`/`users_seen`
 * are counts where a negative is merely pointless rather than malformed, and
 * being stricter here than the API would reject a query the API would answer.
 */
export function isNumericFilterValue(value: string): boolean {
  // The `string` in the signature is not enforceable at the call site: these
  // values arrive from a `bind:value`, which svelte types as `any` on a raw
  // `<input>`, so TypeScript cannot see a number arriving here. `RegExp.test`
  // would coerce one to its digits and quietly pass it, which is how a
  // non-string ends up in `Filter.value` — check the type, not just the shape.
  if (typeof value !== 'string') return false;
  if (!/^[+-]?\d+$/.test(value)) return false;
  const n = BigInt(value);
  return n >= I64_MIN && n <= I64_MAX;
}

/**
 * Whether a draft value may be committed as a filter on `def`.
 *
 * This replaces a bare `value === ''` guard in FilterBar. That guard read as
 * "reject the empty field", but it only ever rejected the empty *string*:
 * `bind:value` on `<input type="number">` writes back `null` once the field is
 * cleared (Svelte coerces numberlike inputs — see `to_number` in
 * `svelte/src/internal/client/dom/elements/bindings/input.js`), and
 * `null === ''` is false, so a cleared field committed a filter whose value
 * was `null`. That encodes to `times_seen:eq:null` and fails the request for
 * the whole list until the chip is removed. The input is plain text now, but
 * the guard checks the value it is actually about to send either way.
 *
 * `tag` is absent on purpose: its value is composed from two separate fields
 * and FilterBar validates those halves before composing them.
 */
export function isFilterValueValid(def: FieldDef | undefined, value: string): boolean {
  // `typeof` rather than `value === ''`: the whole defect this replaces was a
  // comparison that assumed the runtime type matched the declared one.
  if (!def || typeof value !== 'string' || value === '') return false;
  if (def.type === 'number') return isNumericFilterValue(value);
  // Mirrors the server's `FieldType::Enum` check. The `<select>` cannot
  // produce anything else today; this keeps that from being load-bearing.
  if (def.type === 'enum') return (def.options ?? []).includes(value);
  return true;
}

/**
 * The value a draft filter should store, given the field it is for.
 *
 * Number values are trimmed: the server parses them with Rust's
 * `i64::from_str`, which refuses surrounding whitespace, so a pasted `" 7 "`
 * would fail the request. Everything else is stored verbatim — for a
 * `contains` search the spaces may well be the point.
 *
 * Takes the non-string shapes for the same reason `isNumericFilterValue`
 * checks `typeof`: this sits directly on a `bind:value`, so putting a bare
 * `.trim()` at the call site is what re-adding `type="number"` to the input
 * would turn back into a TypeError.
 */
export function normalizeFilterValue(
  def: FieldDef | undefined,
  raw: string | number | null | undefined,
): string {
  const text = typeof raw === 'number' ? String(raw) : (raw ?? '');
  return def?.type === 'number' ? text.trim() : text;
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

/**
 * The searched transactions list.
 *
 * Mirrors the dimensions the query catalog declares for
 * `Resource::Transactions` — `op`, `name`, `url`, `http.method`,
 * `http.status`, `duration`, `extra` and `tag`. A chip for a field the
 * resource does not carry resolves to an "unknown field" 400, so this list is
 * derived from `catalog.rs` rather than from what looks useful.
 *
 * `duration` is deliberately absent as a CHIP: the catalog types it as
 * `ValueType::Duration`, which accepts `2s`/`500ms` spellings that the
 * numeric-chip validator (`i64` digits only) would reject before they ever
 * reached the wire. It is reachable through the query language, where the
 * parser that owns that grammar is the one doing the parsing.
 */
export const TRANSACTION_FIELDS: FieldDef[] = [
  { key: 'name', label: 'Name', type: 'string', ops: OPS_STR },
  {
    key: 'op',
    label: 'Op',
    type: 'enum',
    ops: OPS_ENUM,
    options: ['navigation', 'http', 'resource', 'screen_load', 'custom'],
  },
  { key: 'url', label: 'URL', type: 'string', ops: OPS_STR },
  { key: 'http.method', label: 'Method', type: 'string', ops: OPS_ENUM },
  { key: 'http.status', label: 'Status code', type: 'number', ops: OPS_NUM },
  { key: 'tag', label: 'Tag', type: 'tag', ops: OPS_TAG },
];
