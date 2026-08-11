/**
 * Which value each sortable column of the Source Maps artifact table orders by.
 *
 * Lives beside the page rather than inside it for the reason `monitor-sort.ts`
 * gives: vitest runs on the node environment and cannot import a `.svelte`
 * file, so an accessor map written inline in the component is untestable — and
 * these accessors are exactly where the interesting mistakes are. The Size
 * column is the sharpest example on this page: its cell renders `fmtBytes`,
 * and ordering that text puts "9.5 KB" after "10.0 MB" and "1.0 GB" first,
 * while looking like a working sort.
 *
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header — it
 * falls through to the default column while the caret sits on the header that
 * was clicked. `artifactAccessor` warns in dev to make that audible; the map
 * itself is a convention, not a guard.
 */
import type { SymbolArtifact } from '../api/artifacts';
import type { SortState } from './sort';
import type { SortValue } from './sort-rows';

const ACCESSORS: Record<string, (a: SymbolArtifact) => SortValue> = {
  // Nullable and NOT coerced to '': `sortRows` puts an absent value last in
  // both directions, and an empty string would sort a release-less artifact
  // to the top of an ascending list as if it were named "".
  release: (a) => a.release,
  // Exactly what the cell falls back through — `name ?? debug_id` — so the
  // column is ordered by the text it shows. An artifact with neither is null,
  // and lands last both ways.
  file: (a) => a.name ?? a.debug_id,
  // The cell renders "android / arm64", so the accessor builds the same
  // string. Ordering by `platform` alone would leave the arch suffix in
  // arbitrary order INSIDE each platform, which reads as a partly-broken sort.
  platform: (a) => `${a.platform}${a.arch ? ` / ${a.arch}` : ''}`,
  kind: (a) => a.kind,
  // Bytes, never `fmtBytes`. See the header comment.
  size: (a) => a.uncompressed_size,
  // The raw ISO instant; the cell's `toLocaleString` is locale-ordered text.
  uploaded: (a) => a.created_at,
};

/**
 * The order the table is in before anyone clicks a header.
 *
 * This one DESCRIBES the endpoint rather than replacing it: `list_symbol_artifacts`
 * (backend `repo.rs`) returns `created_at DESC`, which is exactly
 * `{ uploaded, desc }`, so the page opens in the order it does today and the
 * caret names the column that produced it.
 *
 * Exported so the page seeds from the same constant the unknown-key fallback
 * uses — seeding one column and recovering to another would make the initial
 * order and the recovery order disagree, silently.
 */
export const ARTIFACT_DEFAULT_SORT: SortState = { key: 'uploaded', dir: 'desc' };

export function artifactAccessor(key: string): (a: SymbolArtifact) => SortValue {
  const accessor = ACCESSORS[key];
  if (accessor) return accessor;
  if (import.meta.env.DEV) {
    console.warn(
      `[artifact-sort] no accessor for column "${key}" — sorting by ` +
        `"${ARTIFACT_DEFAULT_SORT.key}" instead. Add it to ACCESSORS in artifact-sort.ts.`,
    );
  }
  return ACCESSORS[ARTIFACT_DEFAULT_SORT.key];
}
