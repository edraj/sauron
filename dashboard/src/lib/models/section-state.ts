/**
 * Which face an independently loaded section shows: its content, its failure,
 * or a skeleton. `ViewSection` draws this, and it lives here so the rule has a
 * test and one copy; cards that need a different face for one state take it
 * through ViewSection's props rather than re-deriving the branch.
 */
export type SectionState = 'content' | 'failed' | 'loading';

/** The parts of a `CachedView` the rule reads. */
export interface SectionView {
  hasData: boolean;
  error: string | null;
  loading: boolean;
}

/**
 * - Content whenever there is data, even if a revalidation over it failed.
 * - Failed only once the request settled with nothing to show; a retry in
 *   flight is loading again.
 * - `waiting` holds loaded content back for a section that also needs
 *   something another request supplies.
 */
export function sectionState(view: SectionView, waiting = false): SectionState {
  if (view.hasData && !waiting) return 'content';
  if (view.error && !view.loading) return 'failed';
  return 'loading';
}
