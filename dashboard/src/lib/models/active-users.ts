// Pure decision logic for the Active Users page. Lives here, not in the
// component, because the dashboard has no DOM test environment — this module
// is the only layer of the feature a test can reach.

/** An `AppEnvironment.id`, or the literal `'all'` / `'none'`. */
export type EnvChoice = string;

/** Which environment was chosen for each ticked app. */
export interface AppEnvSelection {
  [appId: string]: EnvChoice;
}

/** Mirrors the backend's `MAX_SELECTED_APPS`; the server 400s past it. */
export const MAX_SELECTED_APPS = 20;

/**
 * Wire tokens for `?selection=`, sorted by app id.
 *
 * Sorting is not cosmetic: the server's Redis cache key hashes the resolved
 * selection, and a stable URL is what makes an export reproducible from the
 * link that produced it. A bare app id means `all`, which keeps the common URL
 * short and round-trips exactly.
 */
export function encodeSelection(sel: AppEnvSelection): string[] {
  return Object.keys(sel)
    .sort()
    .map((appId) => (sel[appId] === 'all' ? appId : `${appId}:${sel[appId]}`));
}

/** Inverse of {@link encodeSelection}. A bare app id decodes to `all`. */
export function decodeSelection(params: string[]): AppEnvSelection {
  const out: AppEnvSelection = {};
  for (const raw of params) {
    const token = raw.trim();
    if (!token) continue;
    const colon = token.indexOf(':');
    if (colon === -1) {
      out[token] = 'all';
    } else {
      const appId = token.slice(0, colon);
      const choice = token.slice(colon + 1);
      if (appId) out[appId] = choice || 'all';
    }
  }
  return out;
}

export function selectionCount(sel: AppEnvSelection): number {
  return Object.keys(sel).length;
}

export function validateSelection(
  sel: AppEnvSelection,
): { ok: true } | { ok: false; reason: string } {
  const n = selectionCount(sel);
  if (n === 0) return { ok: false, reason: 'Pick at least one app.' };
  if (n > MAX_SELECTED_APPS) {
    return { ok: false, reason: `Pick at most ${MAX_SELECTED_APPS} apps.` };
  }
  return { ok: true };
}

/**
 * A one-line summary for the "Apps" tile. Names the environment only when a
 * single app is selected — with several, the per-app environments differ and a
 * concatenated list reads as one combined filter, which it is not.
 */
export function describeSelection(
  sel: AppEnvSelection,
  appName: (appId: string) => string,
  envLabel: (appId: string, choice: EnvChoice) => string,
): string {
  const ids = Object.keys(sel).sort();
  if (ids.length === 0) return 'No apps selected';
  if (ids.length === 1) return `${appName(ids[0])} · ${envLabel(ids[0], sel[ids[0]])}`;
  const named = ids.slice(0, 2).map(appName).join(', ');
  return ids.length === 2 ? named : `${named} +${ids.length - 2} more`;
}

/**
 * The default `[from, to)` window for a range of `rangeDays` whole UTC days
 * ending with today.
 *
 * `to` is the START of tomorrow UTC, so today's still-filling bar is included
 * in the chart (dropping it would make the range shorter than the picker says)
 * while the headline tiles read from the last COMPLETE day. Both ends are day
 * boundaries because the server floors them anyway, and sending an already
 * floored pair is what keeps the JSON request and the CSV request moments
 * later on the same cache key.
 */
export function defaultWindow(rangeDays: number, now: Date): { from: string; to: string } {
  const toMs = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() + 1);
  const fromMs = toMs - rangeDays * 86_400_000;
  return { from: new Date(fromMs).toISOString(), to: new Date(toMs).toISOString() };
}

/**
 * Render a `YYYY-MM-DD` bucket as a short label IN UTC.
 *
 * `new Date('2026-07-31')` parses as UTC but renders in local time, so a
 * viewer at a negative offset would see the chart and the CSV disagree about
 * which day a number belongs to. `locale` exists so a test can pin the output
 * without pinning the runner's locale.
 */
export function utcDayLabel(day: string, locale?: string): string {
  const d = new Date(`${day}T00:00:00Z`);
  if (Number.isNaN(d.getTime())) return day;
  return d.toLocaleDateString(locale, { month: 'short', day: 'numeric', timeZone: 'UTC' });
}
