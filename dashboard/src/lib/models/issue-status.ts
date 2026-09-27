import type { CachedView } from '../stores/cached-view.svelte';
import type { Issue, IssueStatus } from './index';

/**
 * Change the status of the issue a view is showing: optimistically, then for
 * real, then back again if the write fails.
 *
 * Every step REPLACES the view's payload; none edits it. `CachedView.data` is
 * `$state.raw` and is the very object the cache holds, so an in-place
 * `issue.status = next` does two wrong things at once: it writes no signal, so
 * nothing on screen changes, and it alters the cached payload for every later
 * reader. That is how Resolve came to answer 200 while the page went on showing
 * "unresolved" above a Resolve button.
 *
 * `adopt` is the primitive for "a value that arrived without a fetch", and it
 * carries the one guard this needs. The router reuses the page across
 * `#/issues/A` -> `#/issues/B`, so by the time a write lands the view may be
 * showing another issue: the result is then filed under the key it belongs to
 * and never reaches the page.
 *
 * Only `status` and `updated_at` are taken from the response. The write
 * answers with the app-wide record, while the one on screen may be scoped to an
 * environment — adopting the rest would swap the counts under the reader.
 *
 * Returns `false`, without writing, when there is nothing loaded or nothing to
 * change. Rethrows a failed write after restoring, so the caller reports it.
 */
export async function applyIssueStatus(
  view: CachedView<Issue>,
  next: IssueStatus,
  write: () => Promise<Issue>,
): Promise<boolean> {
  const key = view.lastKey;
  const before = view.data;
  if (key === null || before === undefined || before.status === next) return false;

  // Read at each step rather than captured once: what matters is the key the
  // view is on WHEN the value arrives.
  const put = (value: Issue) => view.adopt(key, view.lastKey ?? key, value);

  put({ ...before, status: next });
  try {
    const updated = await write();
    put({ ...before, status: updated.status, updated_at: updated.updated_at });
    return true;
  } catch (err) {
    // The original object, not a copy of it: nothing was edited, so putting it
    // back restores the cache entry exactly.
    put(before);
    throw err;
  }
}
