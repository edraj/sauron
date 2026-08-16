/**
 * The time window a list is filtered by: which timestamp column, and which
 * shape of bound on it.
 *
 * Replaces the four-button `since_days` picker on the lists that browse signal
 * data. Two things it can express that the picker could not: an ABSOLUTE window
 * (a range, a lower bound, or an upper bound), and a CHOICE OF COLUMN — "users
 * whose first_seen is in the last 7 days" (new users) is a different question
 * from "users whose last_seen is", and only the second was previously askable.
 *
 * Pure: no Svelte imports, no network, no clock reads outside the functions
 * that document one. Mirrors `resolve_time_filter` in
 * `backend/bins/sauron-api/src/routes/search.rs`, which is the authority on
 * every rule here — this module's job is to avoid sending a request the server
 * would reject, not to re-decide the semantics.
 */

/** The route's ceiling on a window's total span. Mirrors `max_days`. */
export const MAX_DAYS = 365;

export type TimeMode = 'last' | 'after' | 'before' | 'between';

/** Which end of the interval a raw input value is being read for. */
export type Bound = 'from' | 'to';

export interface TimeField {
  readonly key: string;
  readonly label: string;
}

/**
 * Every field is `readonly`, not merely the container that holds one.
 *
 * Svelte 5 `$state` deep-proxies this object, so `tf.mode = 'after'` is a
 * *reactive* mutation that reaches straight past a `readonly TimeFilterState`
 * annotation on the holder — the write succeeds, the UI updates, and no
 * reducer runs to reset the page position that the changed predicate
 * invalidated. This is the same door `SortState` closes in `sort.ts`, and for
 * the same reason: the coarse `readonly` blocks REPLACING the object, which is
 * the mutation nobody was going to make by accident.
 */
export interface TimeFilterState {
  readonly field: string;
  readonly mode: TimeMode;
  /** Days, for `last`. Integer in `[1, MAX_DAYS]`. */
  readonly lastDays?: number;
  /** RFC3339 UTC. INCLUSIVE lower bound. */
  readonly from?: string;
  /**
   * RFC3339 UTC. **EXCLUSIVE** upper bound.
   *
   * An inclusive bound would have to be spelled as the last representable
   * instant of the period, and `timestamptz` stores microseconds — so
   * `23:59:59.999` silently drops the final millisecond of every window. A
   * half-open interval has no such gap.
   */
  readonly to?: string;
}

/** The plain `last N days` filter a page starts on. */
export function defaultFilter(field: string, days: number): TimeFilterState {
  return { field, mode: 'last', lastDays: days };
}

// ---------------------------------------------------------------------------
// Local time in, UTC out
// ---------------------------------------------------------------------------

const DATE_ONLY = /^(\d{4})-(\d{2})-(\d{2})$/;
const DATE_TIME = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/;

/**
 * Read one `<input type="date">` / `<input type="datetime-local">` value as an
 * instant, interpreting it in the BROWSER'S OWN ZONE, and return RFC3339 UTC.
 *
 * A bare date carries a convention, which is where `bound` comes in: `from`
 * takes the start of that local day, `to` takes the start of the FOLLOWING
 * one. Against the half-open interval that makes "between 1 Aug and 3 Aug"
 * cover all of 3 August, which is what the phrase means to the person typing
 * it. Truncating `to` to the start of its own day instead would drop the whole
 * final day — a boundary convention that reads as a data bug.
 *
 * A value that carries an explicit TIME is taken exactly, for either bound. A
 * convention is only needed where the user did not say.
 *
 * The next-day step is calendar arithmetic via the `Date` constructor's own
 * overflow handling (`d + 1` past the end of a month rolls over), NOT
 * `+ 86_400_000`. A day is not reliably 24 hours: across a spring-forward
 * transition it is 23, and the millisecond form would land an hour into the
 * next day and quietly widen the window.
 *
 * Returns `null` rather than an invented instant when the value cannot be
 * parsed — an empty input mid-edit is the common case, and defaulting it to
 * "now" would apply a filter the user never asked for.
 */
export function localInputToUtc(value: string, bound: Bound): string | null {
  if (typeof value !== 'string') return null;
  const trimmed = value.trim();

  const dateOnly = DATE_ONLY.exec(trimmed);
  if (dateOnly) {
    const [, y, m, d] = dateOnly;
    const day = Number(d) + (bound === 'to' ? 1 : 0);
    const dt = new Date(Number(y), Number(m) - 1, day, 0, 0, 0, 0);
    // Guards a value like `2026-13-45`, which the constructor happily rolls
    // forward into a real (wrong) date rather than rejecting.
    if (Number(m) < 1 || Number(m) > 12 || Number(d) < 1 || Number(d) > 31) return null;
    return Number.isNaN(dt.getTime()) ? null : dt.toISOString();
  }

  const dateTime = DATE_TIME.exec(trimmed);
  if (dateTime) {
    const [, y, m, d, hh, mm] = dateTime;
    if (Number(m) < 1 || Number(m) > 12 || Number(d) < 1 || Number(d) > 31) return null;
    if (Number(hh) > 23 || Number(mm) > 59) return null;
    const dt = new Date(Number(y), Number(m) - 1, Number(d), Number(hh), Number(mm), 0, 0);
    return Number.isNaN(dt.getTime()) ? null : dt.toISOString();
  }

  return null;
}

/** The inverse, for populating a `datetime-local` input from stored state. */
export function utcToLocalInput(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** The viewer's UTC offset, e.g. `UTC+03`, for labelling the control. */
export function localZoneLabel(now: Date = new Date()): string {
  // `getTimezoneOffset` is minutes WEST of UTC, so its sign is inverted
  // relative to the `UTC+hh` people read.
  const mins = -now.getTimezoneOffset();
  const sign = mins < 0 ? '-' : '+';
  const abs = Math.abs(mins);
  const hh = String(Math.floor(abs / 60)).padStart(2, '0');
  const mm = abs % 60;
  return mm === 0 ? `UTC${sign}${hh}` : `UTC${sign}${hh}:${String(mm).padStart(2, '0')}`;
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/**
 * Why this filter cannot be sent, or `null` if it can.
 *
 * Returns prose rather than a boolean because the control shows it: a disabled
 * Apply button with no stated reason is the same dead end as no validation.
 */
export function validate(tf: TimeFilterState): string | null {
  switch (tf.mode) {
    case 'last': {
      const n = tf.lastDays;
      // `typeof` first, not just `Number.isInteger`. A `bind:value` on a raw
      // input is typed `any` by svelte, so a string reaches here despite the
      // `number` annotation, and `'30' > 0` is `true` — the comparison alone
      // would wave it through and the server would then reject the request.
      if (typeof n !== 'number' || !Number.isInteger(n)) return 'Enter a whole number of days';
      if (n < 1) return 'The window must be at least 1 day';
      if (n > MAX_DAYS) return `This list looks back at most ${MAX_DAYS} days`;
      return null;
    }
    case 'after':
      return tf.from ? null : 'Choose a start date';
    case 'before':
      return tf.to ? null : 'Choose an end date';
    case 'between': {
      if (!tf.from) return 'Choose a start date';
      if (!tf.to) return 'Choose an end date';
      // `>=`, not `>`: the interval is half-open, so equal bounds select
      // nothing at all. Rejecting it beats returning a confidently empty table.
      if (new Date(tf.from).getTime() >= new Date(tf.to).getTime()) {
        return 'The start must be earlier than the end';
      }
      return null;
    }
  }
}

// ---------------------------------------------------------------------------
// The wire, and the URL — the same encoding for both
// ---------------------------------------------------------------------------

/**
 * Encode a filter as query parameters.
 *
 * `time_field` is omitted when it matches the page's default, so an untouched
 * page produces an untouched URL. `since_days` is sent ONLY in `last` mode:
 * the server ignores it whenever a bound is present, and sending it anyway
 * would put a request on the wire that reads as two conflicting windows.
 */
export function toParams(tf: TimeFilterState, defaultField: string): URLSearchParams {
  const p = new URLSearchParams();
  if (tf.field !== defaultField) p.set('time_field', tf.field);
  if (tf.mode === 'last') {
    if (tf.lastDays != null) p.set('since_days', String(tf.lastDays));
    return p;
  }
  if (tf.from) p.set('from', tf.from);
  if (tf.to) p.set('to', tf.to);
  return p;
}

/**
 * The same encoding as {@link toParams}, shaped for the axios clients that pass
 * a plain object as `params`.
 *
 * Derived FROM `toParams` rather than written alongside it. Two encoders for
 * one wire format is how a page starts sending `since_days` next to a bound the
 * server will let it override.
 */
export function toRecord(tf: TimeFilterState, defaultField: string): Record<string, string> {
  return Object.fromEntries(toParams(tf, defaultField));
}

/**
 * Decode a filter from query parameters, dropping anything this page cannot
 * honour.
 *
 * Every rejection falls back to `last fallbackDays` rather than surfacing an
 * error. These values arrive from a URL, which is hand-editable and outlives
 * the code that wrote it: a stale bookmark must degrade to a valid view, not
 * produce a 400 on first paint. The mode is INFERRED from which bounds are
 * present rather than carried as its own parameter — a `mode` that disagreed
 * with the bounds beside it would be a third source of truth.
 */
export function fromParams(
  sp: URLSearchParams,
  fields: readonly TimeField[],
  defaultField: string,
  fallbackDays: number,
): TimeFilterState {
  const requested = sp.get('time_field');
  const field = requested && fields.some((f) => f.key === requested) ? requested : defaultField;

  const from = sp.get('from');
  const to = sp.get('to');
  const ok = (s: string | null) => !!s && !Number.isNaN(new Date(s).getTime());

  if (ok(from) || ok(to)) {
    const mode: TimeMode = ok(from) && ok(to) ? 'between' : ok(from) ? 'after' : 'before';
    const candidate: TimeFilterState = {
      field,
      mode,
      ...(ok(from) ? { from: from! } : {}),
      ...(ok(to) ? { to: to! } : {}),
    };
    // Re-validating what we just parsed is not belt-and-braces: an inverted
    // range in a URL is a request the server answers with a 400, and falling
    // back to a valid window shows a table instead of an error page.
    if (validate(candidate) === null) return candidate;
  }

  const raw = sp.get('since_days');
  const n = raw === null ? NaN : Number(raw);
  const days = Number.isInteger(n) && n >= 1 && n <= MAX_DAYS ? n : fallbackDays;
  return { field, mode: 'last', lastDays: days };
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

function labelFor(key: string, fields: readonly TimeField[]): string {
  return fields.find((f) => f.key === key)?.label ?? key;
}

function stamp(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleString(undefined, {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * A one-line caption for the active window, naming the FIELD as well as the
 * bounds — "in the last 7 days" alone is ambiguous the moment a page offers
 * more than one column to apply it to.
 */
export function describeFilter(tf: TimeFilterState, fields: readonly TimeField[]): string {
  const name = labelFor(tf.field, fields);
  switch (tf.mode) {
    case 'last':
      return tf.lastDays === 1
        ? `${name} in the last 24 hours`
        : `${name} in the last ${tf.lastDays} days`;
    case 'after':
      return `${name} after ${stamp(tf.from!)}`;
    case 'before':
      return `${name} before ${stamp(tf.to!)}`;
    case 'between':
      return `${name} between ${stamp(tf.from!)} and ${stamp(tf.to!)}`;
  }
}
