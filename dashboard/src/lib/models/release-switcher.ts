import type { AppRelease } from './index';

/**
 * The release names the topbar switcher may offer, in the order the API
 * returned them.
 *
 * Two release values are unrepresentable in that menu and are dropped rather
 * than rendered:
 *
 *   - a blank / whitespace-only release — it would be a row with no visible
 *     label, indistinguishable from any other blank one and naming nothing a
 *     user could recognise. `sauron-ingest` normalises these to "no release" at
 *     the edge and `releases::backfill_all` filters them out of the catalogue,
 *     so they only reach here from rows written before those rules existed.
 *
 *   - a release literally named `none` — `'none'` is the WIRE literal for "no
 *     release" (`?release=none`) and also the id of the menu's "Unknown
 *     release" pseudo-entry. Keeping both would put two items with
 *     `id === 'none'` into `SwitcherMenu`'s keyed `{#each items as item
 *     (item.id)}`, which Svelte 5 throws on ("keyed each duplicate key") —
 *     taking the whole topbar down, not just this menu. Selecting either would
 *     mean the same thing anyway, so the pseudo-entry wins and the real release
 *     is unreachable from the dashboard. Documented in the spec §3 and in
 *     `wiki/Ingest-Wire-Contract.md`.
 *
 * De-duplication is the third rule and it exists for the same crash, but for
 * ONE narrow reason: trim collisions. The API does NOT hand us one row per
 * (app, environment, release) — `routes/releases.rs` folds the catalogue
 * through a `BTreeMap<String, ReleaseView>` keyed on the release, so a release
 * seen in three environments arrives as a single row carrying three
 * `environment_ids`. What that fold cannot collapse is two keys that differ
 * only in whitespace: `' 1.4.0'` and `'1.4.0'` are distinct `BTreeMap` keys
 * server-side and become the same `item.id` here the moment we trim, which
 * would put a duplicate key into `SwitcherMenu`'s keyed `{#each}` and throw.
 * Trimming is what creates the collision, so the dedupe belongs here rather
 * than being pushed back onto the API.
 *
 * Returns the TRIMMED name, which is what gets both rendered and sent as
 * `?release=`. That matches the value the ingest edge stores today; a padded
 * row predating that normalisation is selectable under its trimmed name (and
 * `resolveCurrentRelease` validates a stored selection through this same
 * function, so the two never disagree about what is selectable).
 *
 * Pure and exported so it can be tested directly: as an inline `$derived` in
 * `Topbar.svelte` none of the three rules had a single test, and two of them
 * are the difference between a menu and a blank dashboard.
 */
export function selectableReleases(releases: AppRelease[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const r of releases) {
    const name = r.release.trim();
    if (name === '' || name === 'none') continue;
    if (seen.has(name)) continue;
    seen.add(name);
    out.push(name);
  }
  return out;
}
