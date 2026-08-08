import { PERMISSION_GROUPS, PERMISSION_LABELS } from './permissions';
import type { Permission } from './index';

/**
 * The pure logic behind the permission picker's collapse, per-section
 * select-all, and search behaviour.
 *
 * The dashboard has no DOM test environment, so anything that decides what
 * renders indeterminate, what a click emits, or what a search query matches
 * lives here rather than inside the component, where it could not be tested.
 * Nothing in this file may touch Svelte or the DOM.
 */

export type GroupState = 'all' | 'some' | 'none';

/** Whether a group is fully, partly, or not at all selected. */
export function groupState(groupPermissions: Permission[], selected: Set<Permission>): GroupState {
  const hits = groupPermissions.filter((p) => selected.has(p)).length;
  if (hits === 0) return 'none';
  if (hits === groupPermissions.length) return 'all';
  return 'some';
}

/** Emit in catalog order so a role's stored array is stable regardless of click order. */
export function inCatalogOrder(selected: Set<Permission>): Permission[] {
  return PERMISSION_GROUPS.flatMap((g) => g.permissions).filter((p) => selected.has(p));
}

/** Case-insensitive match against the permission string and its label. */
export function matchesQuery(permission: Permission, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  if (permission.toLowerCase().includes(q)) return true;
  return (PERMISSION_LABELS[permission] ?? '').toLowerCase().includes(q);
}

function sameContent(a: Permission[], b: Permission[]): boolean {
  return a.length === b.length && a.every((p, i) => p === b[i]);
}

export interface SelectionReceipt {
  /**
   * True when the incoming array is a wholesale replacement — the dialog
   * opening on a different role — and the picker must therefore recompute
   * which groups start expanded and clear any live search.
   */
  recompute: boolean;
  /** The pending-emission baseline to carry into the next call. */
  pendingEmit: Permission[] | null;
}

/**
 * Decide whether an incoming `selected` prop is a wholesale replacement or
 * merely the echo of a change this picker just emitted.
 *
 * The picker's consumer (RoleEditorDialog) stores whatever `onchange` emits
 * into its own `$state` and passes it straight back down, and Svelte 5
 * deep-proxies `$state`, so that round-tripped array is never `===` what was
 * emitted even though the contents are identical. Reference identity
 * therefore cannot tell "the user ticked a box" apart from "the dialog opened
 * on a different role"; content comparison against the pending emission can.
 *
 * `pendingEmit` is consumed — returned as `null` on BOTH branches, not just
 * on a replace. That is the load-bearing half, and matching only on content
 * is not enough without it: the picker re-arms the baseline on every single
 * tick, so a role whose permissions happen to equal the last emission would
 * be read as an echo and would silently inherit the previous role's
 * expand/collapse pattern and search box. That is not hypothetical — copying
 * a role produces one with permissions identical to its source by
 * construction, so "edit Developer, then open Copy of Developer" walks
 * straight into it. One emission earns exactly one echo; anything after it is
 * somebody else's write.
 */
export function receiveSelection(
  pendingEmit: Permission[] | null,
  incoming: Permission[],
): SelectionReceipt {
  const isEcho = pendingEmit !== null && sameContent(pendingEmit, incoming);
  return { recompute: !isEcho, pendingEmit: null };
}
