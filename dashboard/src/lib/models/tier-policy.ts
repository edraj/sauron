/**
 * Parsing for the two numeric fields on the Storage page — the cold-tier
 * rotation age and a restore's pin lifetime.
 *
 * WHY THESE FIELDS ARE TEXT INPUTS, NOT `type="number"`
 *
 * They were `<input type="number">` and it crashed the card. Svelte's
 * `bind:value` special-cases numberlike inputs and writes back the *coerced*
 * value — `to_number()` in
 * `svelte/src/internal/client/dom/elements/bindings/input.js`, which is
 * `value === '' ? null : +value`. So the state is a string until the first
 * keystroke and a `number` (or `null`) after it, and `.trim()` on that throws
 * `TypeError: ... .trim is not a function` — from the render effect that
 * computes the Apply button's `disabled`, which freezes the button enabled.
 *
 * Switching the validator to work on numbers instead would have been worse
 * than the crash. `+value` erases the distinction between the text the
 * operator typed and the number it rounds to: `"3.0"`, `"3."` and — in a
 * locale that groups with dots — `"1.000"` all coerce to a small integer that
 * passes an `isSafeInteger` check, so a mis-typed rotation age would save
 * cleanly as 3 or 1 days. The next tier cycle then exports and drops every
 * partition newer than that, and per the warning on the page itself that is
 * not undone by raising the number back. A regex over the raw text rejects all
 * of them, which is precisely why the original author wrote one.
 *
 * Hence: text inputs, and the strict text validator below.
 *
 * The `string | number | null` signature is deliberate defence in depth. It is
 * the set of shapes `bind:value` can produce across input types, so re-adding
 * `type="number"` to one of these fields can never resurrect the TypeError —
 * it would only relax strictness, not crash the page.
 */

/** Bounds for a restore pin's lifetime, matching the server's allowed range. */
export const RESTORE_MIN_DAYS = 1;
export const RESTORE_MAX_DAYS = 365;

/**
 * A whole, non-negative number of days, or null if the text is not one.
 *
 * Rejects decimals, signs, exponent notation and whitespace-only input;
 * tolerates surrounding whitespace so a pasted value works.
 */
export function parseWholeDays(raw: string | number | null | undefined): number | null {
  // A number only reaches here if some input is numberlike, in which case the
  // original text is already gone and the strictness above cannot be enforced.
  // Normalising it keeps the page alive; it does not make it as safe as text.
  const text = typeof raw === 'number' ? String(raw) : (raw ?? '');
  const t = text.trim();
  if (!/^\d+$/.test(t)) return null;
  const n = Number(t);
  return Number.isSafeInteger(n) ? n : null;
}

/**
 * Whether a parsed rotation age may be submitted.
 *
 * `min` is the server's floor: below it the cutoff lands at or after now and
 * the tier worker would export partitions that are still being written to.
 */
export function isHotDaysValid(parsed: number | null, minHotDays: number): boolean {
  return parsed !== null && parsed >= minHotDays;
}

/**
 * Whether the rotation-age field holds an edit the server has not seen.
 *
 * `seeded` is the last value written INTO the field from a server response.
 * Two separate defects turn on this one question:
 *
 *  - The card polls `loadPolicy()` every 3s while a restore job is active, and
 *    that reseed used to be unconditional. Svelte declines to rewrite a focused
 *    input only when the change came from that input's own batch — `batches` in
 *    `bind_value` is populated solely by its `input` listener — so a poll-driven
 *    assignment sails past the guard and overwrites what is being typed. The
 *    field was therefore un-editable while any job was queued or running, and a
 *    stuck job made that permanent.
 *  - "Saved. Takes effect on the tier worker's next cycle." must not stay on
 *    screen once the field says something else, or the page asserts that an
 *    unsaved number is in force.
 *
 * Compared as text, not as parsed days: any keystroke makes the field dirty and
 * the poll leaves it alone. Comparing parsed values would let a reseed fire
 * while someone is mid-edit on a change that happens not to alter the number
 * yet — moving their cursor for no reason.
 */
export function isHotDaysDirty(current: string, seeded: string): boolean {
  return current !== seeded;
}

/** The two ages the revert guard compares. Structurally satisfied by `TierPolicy`. */
export interface RotationAges {
  /** TIER_HOT_DAYS in the API process — what clearing the override falls back to. */
  configured_hot_days: number;
  /** What the tier worker will use on its next cycle. */
  effective_hot_days: number;
}

/**
 * Whether clearing the override would LOWER the rotation age.
 *
 * "Revert to default" reads like an undo, and for an override BELOW the
 * configured value it is one. Above it, it is the same irreversible lowering
 * the typed-value path warns about — an override of 180 against a configured
 * 30 drops five months of partitions on the next cycle — except it arrives
 * from a single click with no number typed and nothing on screen relating the
 * two figures. The server does not backstop it either: the `None` arm of the
 * tier-policy handler deletes the setting without validating anything.
 */
export function revertWouldLower(p: RotationAges): boolean {
  return p.configured_hot_days < p.effective_hot_days;
}

/**
 * Confirmation text for a revert that lowers the rotation age.
 *
 * Deliberately names both ages and the range between them: the button says
 * only "Revert to default (30d)", so the amount of data at stake is the one
 * thing a reader cannot get from the control they are about to click. The
 * closing sentence is the typed-value warning's, verbatim, so the two paths
 * describe the same consequence in the same words.
 *
 * Only meaningful when `revertWouldLower` is true; callers guard on it.
 */
export function describeRevert(p: RotationAges): string {
  return (
    `Reverting drops the ${p.effective_hot_days}-day override and puts ` +
    `${p.configured_hot_days} days in force. On its next cycle the tier worker ` +
    `will export and then drop everything between ${p.configured_hot_days} and ` +
    `${p.effective_hot_days} days old. Raising the number afterwards does not ` +
    `bring it back into Postgres — that needs a restore from cold.`
  );
}

/**
 * Whether a parsed session-retention value may be submitted. `0` is an
 * explicit OFF; anything else must be at least the server's floor — below it
 * the server refuses rather than rounding, because retention deletes data
 * with no cold copy.
 */
export function isRetentionValid(parsed: number | null, minRetentionDays: number): boolean {
  return parsed !== null && (parsed === 0 || parsed >= minRetentionDays);
}

/**
 * Whether moving session retention from `effective` to `next` deletes data on
 * the next daily pass: enabling it while off, or lowering it while on.
 * Turning it OFF (`next === 0`) or raising it deletes nothing. Unlike the
 * rotation age there is no restore path — sessions have no cold copy — so
 * every true here deserves the strongest warning the page has.
 */
export function retentionWouldDelete(effective: number, next: number): boolean {
  return next !== 0 && (effective === 0 || next < effective);
}

/** The two retention ages the revert guard compares. Structurally satisfied by `TierPolicy`. */
export interface RetentionAges {
  /** SESSION_RETENTION_DAYS in the API process — what clearing falls back to. */
  configured_session_retention_days: number;
  /** What the daily pass will use next; 0 = off. */
  effective_session_retention_days: number;
}

/**
 * Whether clearing the retention override would delete data — the same trap
 * as `revertWouldLower`: "Revert to default" reads like an undo, but when the
 * configured value is tighter (or retention was off only by override) the
 * revert IS the destructive change, reached from one click.
 */
export function retentionRevertWouldDelete(p: RetentionAges): boolean {
  return retentionWouldDelete(
    p.effective_session_retention_days,
    p.configured_session_retention_days,
  );
}

/**
 * Confirmation text for a retention revert that deletes data. Names both
 * values and says plainly that there is no restore path; only meaningful when
 * `retentionRevertWouldDelete` is true, callers guard on it.
 */
export function describeRetentionRevert(p: RetentionAges): string {
  const from =
    p.effective_session_retention_days === 0
      ? 'off'
      : `${p.effective_session_retention_days} days`;
  return (
    `Reverting drops the override (currently ${from}) and puts ` +
    `${p.configured_session_retention_days} days in force. On the next daily pass, ` +
    `raw sessions older than ${p.configured_session_retention_days} days are deleted ` +
    `permanently — sessions have no cold copy, so past-retention days survive only ` +
    `as aggregates. Raising the number afterwards does not bring them back.`
  );
}

/** A restore pin lifetime within the server's bounds, or null. */
export function parseRestoreDays(raw: string | number | null | undefined): number | null {
  const n = parseWholeDays(raw);
  if (n === null || n < RESTORE_MIN_DAYS || n > RESTORE_MAX_DAYS) return null;
  return n;
}
