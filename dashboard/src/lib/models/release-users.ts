/**
 * The Users page under a picked release: which people the persons list means,
 * and how the page recognises a release whose numbers are still being built.
 *
 * With a release `R` in the Topbar switcher, `GET …/persons` and
 * `GET …/counts/persons` answer two different questions, chosen by
 * `release_mode`:
 *
 *  - `used` (the server's default) — everyone with at least one signal on `R`;
 *    their first/last seen and counts are per-release.
 *  - `current` — the same, but only people whose MOST RECENT signal (in the
 *    environment scope) is on `R`: who is on it now, not who ever touched it.
 *
 * The parameter means nothing without a release (the server ignores it), so
 * the page never sends it then and the toggle is not rendered — see
 * `effectiveReleaseMode`. The user's last choice is kept rather than reset, so
 * moving from one release to the next keeps the question they were asking.
 */

export type ReleaseMode = 'used' | 'current';

/** Display order of the toggle. */
export const RELEASE_MODES: readonly ReleaseMode[] = ['used', 'current'];

export const DEFAULT_RELEASE_MODE: ReleaseMode = 'used';

/**
 * The page's own URL parameter — the same name as the API's, so a shared
 * `/users?release_mode=current` link reads the same as the request it
 * produces.
 */
export const RELEASE_MODE_PARAM = 'release_mode';

/**
 * The error code the API answers (HTTP 503) on the three Users reads while
 * the app's release tables are still being backfilled. Not an outage: the
 * numbers appear once the unattended backfill finishes, so the page explains
 * instead of showing an error card.
 */
export const RELEASE_ROLLUP_PENDING = 'release_rollup_pending';

/**
 * The mode a URL asked for. Anything absent or unrecognised degrades to the
 * default rather than 400ing on the next request — the same rule
 * `time-filter`'s `fromParams` applies to a hand-edited window.
 */
export function parseReleaseMode(raw: string | null | undefined): ReleaseMode {
  return raw === 'current' ? 'current' : DEFAULT_RELEASE_MODE;
}

/**
 * The mode that is actually in force: the chosen one while a release is
 * picked, `null` with none (there is nothing for "used it" or "currently on
 * it" to refer to).
 *
 * This, not the raw choice, is what goes into the list and count view keys:
 * with no release picked a remembered `current` must not mint a second cache
 * entry for a request that is byte-identical to the `used` one.
 */
export function effectiveReleaseMode(
  mode: ReleaseMode,
  release: string | null,
): ReleaseMode | null {
  return release === null ? null : mode;
}

/**
 * The query parameters the list and count requests carry for `effective`
 * (the output of `effectiveReleaseMode`).
 *
 * Sent explicitly whenever a release is picked — `used` included, though it is
 * the server default — so the request on the wire states the whole predicate
 * its view key was built from. Nothing at all without a release.
 */
export function releaseModeParams(effective: ReleaseMode | null): { release_mode?: ReleaseMode } {
  return effective === null ? {} : { release_mode: effective };
}

/**
 * Mirror the mode into the page URL's query, in place.
 *
 * Only a non-default mode under a picked release is written: the default
 * would be noise in every link, and with no release the mode is not in force.
 * The in-memory choice is not touched by this — a boot-time render before the
 * stored release has loaded drops the parameter for one tick and the next
 * write restores it.
 */
export function writeReleaseModeParam(
  params: URLSearchParams,
  effective: ReleaseMode | null,
): URLSearchParams {
  if (effective !== null && effective !== DEFAULT_RELEASE_MODE) {
    params.set(RELEASE_MODE_PARAM, effective);
  } else {
    params.delete(RELEASE_MODE_PARAM);
  }
  return params;
}

/** True for the "this release's numbers are still being built" answer. */
export function isReleaseRollupPending(code: string | null | undefined): boolean {
  return code === RELEASE_ROLLUP_PENDING;
}
