import { describe, it, expect } from 'vitest';
import {
  parseWholeDays,
  isHotDaysValid,
  parseRestoreDays,
  isHotDaysDirty,
  revertWouldLower,
  describeRevert,
  RESTORE_MIN_DAYS,
  RESTORE_MAX_DAYS,
  isRetentionValid,
  retentionWouldDelete,
  retentionRevertWouldDelete,
  describeRetentionRevert,
} from './tier-policy';

describe('parseWholeDays', () => {
  it('accepts a whole number of days', () => {
    expect(parseWholeDays('30')).toBe(30);
    expect(parseWholeDays('1')).toBe(1);
    expect(parseWholeDays('0')).toBe(0);
  });

  it('tolerates surrounding whitespace, which is what a paste produces', () => {
    expect(parseWholeDays(' 45 ')).toBe(45);
    expect(parseWholeDays('\t7\n')).toBe(7);
  });

  it('rejects the empty and blank field rather than guessing', () => {
    expect(parseWholeDays('')).toBeNull();
    expect(parseWholeDays('   ')).toBeNull();
  });

  // These are the cases a `type="number"` input would have silently accepted
  // by coercing them to a SMALLER integer. Lowering the rotation age is not
  // reversible — the next tier cycle exports and drops the newly-eligible
  // partitions — so each of these has to stay a rejection, not a round.
  it('rejects decimals that would coerce to a smaller whole number', () => {
    expect(parseWholeDays('3.0')).toBeNull();
    expect(parseWholeDays('3.')).toBeNull();
    expect(parseWholeDays('3.5')).toBeNull();
    // Dot-grouped thousands in a de-DE/fr-FR style entry: `+'1.000'` is 1.
    expect(parseWholeDays('1.000')).toBeNull();
    expect(parseWholeDays('3,5')).toBeNull();
  });

  it('rejects exponent notation', () => {
    expect(parseWholeDays('1e3')).toBeNull();
    expect(parseWholeDays('1e')).toBeNull();
  });

  it('rejects signs and non-numeric text', () => {
    expect(parseWholeDays('-5')).toBeNull();
    expect(parseWholeDays('+5')).toBeNull();
    expect(parseWholeDays('abc')).toBeNull();
    expect(parseWholeDays('30d')).toBeNull();
  });

  it('rejects integers beyond exact float representation', () => {
    expect(parseWholeDays('9007199254740993')).toBeNull();
  });

  // Regression guard for the reported crash: `bind:value` on a numberlike
  // input hands over a number (or null once the field is emptied), and the
  // old `raw.trim()` threw `TypeError: ... .trim is not a function` on it.
  // Parsing must survive every shape a binding can produce.
  it('does not throw on the shapes a numberlike bind:value produces', () => {
    expect(parseWholeDays(45)).toBe(45);
    expect(parseWholeDays(null)).toBeNull();
    expect(parseWholeDays(undefined)).toBeNull();
    expect(parseWholeDays(3.5)).toBeNull();
    expect(parseWholeDays(-5)).toBeNull();
  });
});

describe('isHotDaysValid', () => {
  it('requires a parsed value at or above the server floor', () => {
    expect(isHotDaysValid(30, 7)).toBe(true);
    expect(isHotDaysValid(7, 7)).toBe(true);
    expect(isHotDaysValid(6, 7)).toBe(false);
  });

  // The Apply button sends `parsedHotDays`, and `null` on that endpoint means
  // "clear the override and revert to the configured default" — a different,
  // destructive action. An unparseable field must never be submittable.
  it('rejects null so the Apply path can never send a revert', () => {
    expect(isHotDaysValid(null, 7)).toBe(false);
    expect(isHotDaysValid(null, 1)).toBe(false);
  });

  it('does not let 0 through a floor of 1', () => {
    expect(isHotDaysValid(0, 1)).toBe(false);
  });
});

describe('parseRestoreDays', () => {
  it('accepts a lifetime inside the server bounds', () => {
    expect(parseRestoreDays('30')).toBe(30);
    expect(parseRestoreDays(String(RESTORE_MIN_DAYS))).toBe(RESTORE_MIN_DAYS);
    expect(parseRestoreDays(String(RESTORE_MAX_DAYS))).toBe(RESTORE_MAX_DAYS);
  });

  it('rejects values outside the bounds', () => {
    expect(parseRestoreDays('0')).toBeNull();
    expect(parseRestoreDays('366')).toBeNull();
  });

  // A non-integer here reaches the API as `expires_in_days`, typed i64 server
  // side, and comes back as an opaque 422 instead of a clean local rejection.
  it('rejects non-integers rather than deferring to the server', () => {
    expect(parseRestoreDays('3.5')).toBeNull();
    expect(parseRestoreDays('30.0')).toBeNull();
  });

  it('survives the shapes a numberlike bind:value produces', () => {
    expect(parseRestoreDays(30)).toBe(30);
    expect(parseRestoreDays(null)).toBeNull();
  });
});

describe('revertWouldLower', () => {
  // "Revert to default" reads like an undo. It only is one when the override
  // sits below the configured value; above it, it destroys data.
  it('is true when the override is above the configured default', () => {
    expect(revertWouldLower({ configured_hot_days: 30, effective_hot_days: 180 })).toBe(true);
  });

  it('is false when reverting would raise the rotation age', () => {
    expect(revertWouldLower({ configured_hot_days: 90, effective_hot_days: 30 })).toBe(false);
  });

  it('is false when the override matches the default, so nothing is dropped', () => {
    expect(revertWouldLower({ configured_hot_days: 30, effective_hot_days: 30 })).toBe(false);
  });

  it('treats a one-day gap as lowering', () => {
    expect(revertWouldLower({ configured_hot_days: 29, effective_hot_days: 30 })).toBe(true);
  });
});

describe('describeRevert', () => {
  const msg = describeRevert({ configured_hot_days: 30, effective_hot_days: 180 });

  it('names both ages and the range that will be dropped', () => {
    // The button reads only "Revert to default (30d)", so the 180 and the span
    // between the two are exactly what the user cannot otherwise see.
    expect(msg).toContain('180-day override');
    expect(msg).toContain('30 days in force');
    expect(msg).toContain('between 30 and 180 days old');
  });

  it('closes with the same sentence as the typed-value warning', () => {
    // Both paths cause the identical irreversible export-and-drop, so they say
    // so identically; drift here is how one of them starts sounding survivable.
    expect(msg).toContain(
      'Raising the number afterwards does not bring it back into Postgres — that needs a restore from cold.',
    );
  });
});

describe('isHotDaysDirty', () => {
  it('is false when the field still holds what the server seeded', () => {
    expect(isHotDaysDirty('30', '30')).toBe(false);
  });

  // The poll reseeds only when this is false, so a true here is what keeps the
  // field typeable while a restore job is running.
  it('is true once the field has been edited away', () => {
    expect(isHotDaysDirty('3', '30')).toBe(true);
    expect(isHotDaysDirty('', '30')).toBe(true);
  });

  it('treats a half-typed value as dirty', () => {
    // Mid-edit of 30 -> 300: the poll must not step in between keystrokes.
    expect(isHotDaysDirty('30 ', '30')).toBe(true);
  });

  it('goes clean again when the field is typed back to the seeded value', () => {
    // Both consumers want this: the poll may resume, and "Saved" is true again
    // because the number on screen really is the one in force.
    expect(isHotDaysDirty('30', '30')).toBe(false);
  });

  it('is false at mount, when neither has been set', () => {
    // Guards the first load: were this true, the initial seed would be skipped
    // and the field would stay empty forever.
    expect(isHotDaysDirty('', '')).toBe(false);
  });
});

describe('isRetentionValid', () => {
  it('accepts 0 as an explicit off', () => {
    expect(isRetentionValid(0, 7)).toBe(true);
  });

  it('accepts values at or above the floor', () => {
    expect(isRetentionValid(7, 7)).toBe(true);
    expect(isRetentionValid(365, 7)).toBe(true);
  });

  // 1..6 is the band where a typo would over-delete; the server refuses it
  // and the button must too.
  it('rejects null and the below-floor band', () => {
    expect(isRetentionValid(null, 7)).toBe(false);
    expect(isRetentionValid(1, 7)).toBe(false);
    expect(isRetentionValid(6, 7)).toBe(false);
  });
});

describe('retentionWouldDelete', () => {
  it('enabling retention while off deletes', () => {
    expect(retentionWouldDelete(0, 30)).toBe(true);
  });

  it('lowering while on deletes', () => {
    expect(retentionWouldDelete(90, 30)).toBe(true);
  });

  it('raising, keeping, or turning off deletes nothing', () => {
    expect(retentionWouldDelete(30, 90)).toBe(false);
    expect(retentionWouldDelete(30, 30)).toBe(false);
    expect(retentionWouldDelete(30, 0)).toBe(false);
    expect(retentionWouldDelete(0, 0)).toBe(false);
  });
});

describe('retention revert', () => {
  it('flags a revert to a tighter configured value and names both ages', () => {
    const p = { configured_session_retention_days: 30, effective_session_retention_days: 90 };
    expect(retentionRevertWouldDelete(p)).toBe(true);
    expect(describeRetentionRevert(p)).toContain('30 days');
    expect(describeRetentionRevert(p)).toContain('90 days');
  });

  it('a revert back to off is safe', () => {
    expect(
      retentionRevertWouldDelete({
        configured_session_retention_days: 0,
        effective_session_retention_days: 30,
      }),
    ).toBe(false);
  });
});
