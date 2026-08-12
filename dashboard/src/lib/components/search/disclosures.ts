/**
 * What a search result set LEAVES OUT, as prose.
 *
 * Split from `SearchDisclosure.svelte` so it is testable as a plain module —
 * no test in this project imports from a `.svelte` file, and the message
 * wording is the part worth pinning.
 */
import type { ClampInfo } from '../../api/search';

export interface Disclosure {
  text: string;
  tone: 'warning' | 'info';
}

/**
 * `clamped` names the window SERVED, by the rule that actually bound — the
 * handler is careful to report the tightest of the caller's own window, the
 * route's ceiling and the planner's cost clamp, so this copy can quote it
 * directly rather than re-deriving anything.
 *
 * `payloadSearched` has THREE states and only one is worth a line:
 * `null` = no free-text search ran, `true` = it ran in full, `false` = it ran
 * and silently matched fewer columns than this reader assumes. Folding `null`
 * and `false` together would put a warning on every unfiltered page load.
 */
export function disclosuresFor(
  clamped: ClampInfo | null | undefined,
  payloadSearched: boolean | null | undefined,
): Disclosure[] {
  const out: Disclosure[] = [];
  if (clamped) {
    out.push({
      text: `Showing the last ${clamped.to} only — ${clamped.reason}. Rows outside that window are not included.`,
      tone: 'warning',
    });
  }
  if (payloadSearched === false) {
    out.push({
      text: 'Your search matched titles and metadata only — event payloads need event:read, so some matching rows may be missing.',
      tone: 'info',
    });
  }
  return out;
}
