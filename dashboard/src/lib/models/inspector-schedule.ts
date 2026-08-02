import { WEEKDAYS } from '../constants/inspectorSchedules';

// The SERVER's `next_run_at` is authoritative. Everything here is DISPLAY
// ONLY: the backend resolves DST with Postgres's `AT TIME ZONE`, and this
// module cannot, so a preview that disagrees by an hour on a transition day is
// expected and is not a bug to chase.

/** Bit N = day N, Sunday first, matching Postgres's EXTRACT(DOW). */
export function weekdayMaskToArray(mask: number): boolean[] {
  return WEEKDAYS.map((_, i) => ((mask >> i) & 1) === 1);
}

export function weekdayArrayToMask(days: boolean[]): number {
  return days.reduce((acc, on, i) => (on ? acc | (1 << i) : acc), 0);
}

export function describeSchedule(mask: number, time: string, tz: string): string {
  if (mask === 0) return 'No scheduled runs';
  if (mask === 127) return `Every day at ${time} (${tz})`;
  const names = weekdayMaskToArray(mask)
    .map((on, i) => (on ? WEEKDAYS[i] : null))
    .filter((n): n is string => n !== null);
  return `Every ${names.join(', ')} at ${time} (${tz})`;
}

/**
 * The next three instants, for a preview under the picker.
 *
 * Plain UTC arithmetic rather than a tz library — the dashboard has no date
 * library and this is display only. `_tz` is accepted and deliberately unused:
 * every call site has the policy's zone, and dropping it from the signature
 * would invite someone to later re-add it and quietly start resolving weekdays
 * in the browser's zone, which is the one thing the body below refuses to do.
 */
export function nextRuns(mask: number, time: string, _tz: string, now: Date = new Date()): Date[] {
  if (mask === 0) return [];
  const [hh, mm] = time.split(':').map((n) => Number.parseInt(n, 10));
  const out: Date[] = [];
  // 21 days, not 14. A weekly (single-bit) schedule has to yield THREE
  // candidates, and three weekly runs span up to 21 days from an arbitrary
  // starting weekday — from a Saturday, the third Sunday is offset 15. A
  // 14-day bound silently returns two, and the preview under the picker
  // quietly shows one fewer run than it promises.
  for (let offset = 0; offset <= 21 && out.length < 3; offset += 1) {
    const day = new Date(now.getTime() + offset * 86400_000);
    const candidate = new Date(
      Date.UTC(day.getUTCFullYear(), day.getUTCMonth(), day.getUTCDate(), hh, mm, 0),
    );
    // Deliberately UTC day-of-week: the server resolves the real local
    // weekday with Postgres's AT TIME ZONE, and duplicating that here without
    // a tz library would produce a preview that is confidently wrong near
    // midnight. The Policy tab labels this list "approximate — the server
    // decides", and `_tz` is carried only so that label can name the zone.
    const dow = candidate.getUTCDay();
    if (((mask >> dow) & 1) === 1 && candidate.getTime() > now.getTime()) {
      out.push(candidate);
    }
  }
  return out;
}
