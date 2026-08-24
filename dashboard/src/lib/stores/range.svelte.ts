/**
 * The date range every analytics page shares, plus the named ranges a user
 * keeps around.
 *
 * # Why the selection is shared rather than per-page
 *
 * Before this, each page held its own `let sinceDays = $state(…)`, so picking
 * `90d` on Overview and clicking through to Performance silently landed you
 * back on that page's own default — the control looked global and behaved
 * local.
 *
 * # Why it starts EMPTY
 *
 * Pages do not share a default: Overview windows 30 days, Issues its widest
 * setting so its list shows every issue. Seeding this store with any one of
 * those would change what another page shows on first load, as a side effect of
 * a filter nobody touched. So `value` is `null` until the user picks something,
 * and [`effective`] falls back to whatever the page passes. Once a choice
 * exists it applies everywhere, which is the whole point of a global range.
 */

import {
  isValidRange,
  lastDays,
  type AbsoluteRange,
  type DateRangeValue,
} from '../models/date-range';

export const CURRENT_KEY = 'sauron.dateRange';
export const SAVED_KEY = 'sauron.dateRange.saved';

/**
 * Ceiling on the saved list.
 *
 * Unbounded growth in `localStorage` is a quota error waiting to happen, and it
 * lands on whatever unrelated write happens to cross the limit first — so the
 * symptom appears nowhere near the cause.
 */
export const SAVED_MAX = 20;

/** A named absolute window, as stored. */
export interface SavedRange extends AbsoluteRange {
  readonly id: string;
  readonly name: string;
}

/**
 * Read a JSON value, treating every failure as absent.
 *
 * Both directions are guarded: private-mode Safari throws from `getItem` as
 * well as `setItem`, and these values outlive the code that wrote them. A bad
 * preference must not break every page in the app.
 */
function read(key: string): unknown {
  if (typeof window === 'undefined') return null;
  try {
    const raw = window.localStorage.getItem(key);
    return raw === null ? null : JSON.parse(raw);
  } catch {
    return null;
  }
}

function write(key: string, value: unknown): void {
  if (typeof window === 'undefined') return;
  try {
    if (value === null) window.localStorage.removeItem(key);
    else window.localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* keep the in-memory value — losing persistence must not lose the click */
  }
}

function isSaved(v: unknown): v is SavedRange {
  if (typeof v !== 'object' || v === null) return false;
  const r = v as Partial<SavedRange>;
  if (typeof r.id !== 'string' || r.id === '') return false;
  if (typeof r.name !== 'string' || r.name.trim() === '') return false;
  // The window itself goes through the same validator every other reader uses,
  // so a saved range cannot be laxer than a selected one.
  return r.kind === 'absolute' && isValidRange(v);
}

/**
 * `crypto.randomUUID` where it exists, else a counter-and-clock fallback.
 *
 * The id only has to be unique within one list of at most [`SAVED_MAX`]
 * entries in one browser, so a weak fallback is genuinely sufficient — it is
 * never a security boundary and never leaves the device.
 */
let seq = 0;
function newId(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === 'function') return c.randomUUID();
  seq += 1;
  return `r${Date.now().toString(36)}-${seq}`;
}

export class RangeStore {
  /** `null` means "the user has not chosen" — see the module doc. */
  value = $state<DateRangeValue | null>(null);
  saved = $state<SavedRange[]>([]);

  constructor() {
    const stored = read(CURRENT_KEY);
    this.value = isValidRange(stored) ? stored : null;

    const list = read(SAVED_KEY);
    // Filtered per-entry rather than all-or-nothing: one corrupt row should
    // not silently delete a list the user built up over months.
    this.saved = Array.isArray(list) ? list.filter(isSaved) : [];
  }

  /** The window a page should use, given its own fallback in days. */
  effective(fallbackDays: number): DateRangeValue {
    return this.value ?? lastDays(fallbackDays);
  }

  set(next: DateRangeValue): void {
    this.value = next;
    write(CURRENT_KEY, next);
  }

  /** Return every page to its own default. */
  clear(): void {
    this.value = null;
    write(CURRENT_KEY, null);
  }

  /**
   * Name and keep an absolute window.
   *
   * Relative windows are refused rather than silently dropped-in: `24h` and
   * `7d` are already one click away on the chip strip, so a saved copy would
   * be a second control for the same thing.
   */
  save(name: string, range: DateRangeValue): void {
    const trimmed = name.trim();
    if (trimmed === '' || range.kind !== 'absolute') return;
    const entry: SavedRange = { ...range, id: newId(), name: trimmed };
    // Newest first, oldest dropped past the cap.
    this.saved = [entry, ...this.saved].slice(0, SAVED_MAX);
    write(SAVED_KEY, this.saved);
  }

  remove(id: string): void {
    this.saved = this.saved.filter((r) => r.id !== id);
    write(SAVED_KEY, this.saved);
  }
}

export const rangeStore = new RangeStore();
