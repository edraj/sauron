const STORAGE_KEY = 'sauron.nav.collapsed';

/**
 * Reads the persisted set of collapsed group labels.
 *
 * Anything unexpected — absent, not JSON, JSON that isn't an array of strings,
 * or written by an older build — yields an empty set rather than throwing. A
 * corrupt preference must not take the whole sidebar down with it.
 */
function initialCollapsed(): Set<string> {
  if (typeof window === 'undefined') return new Set();
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return new Set();
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((v): v is string => typeof v === 'string'));
  } catch {
    return new Set();
  }
}

class NavCollapseStore {
  /**
   * The labels of the *collapsed* sidebar groups.
   *
   * Storing the collapsed set rather than the expanded one is what makes
   * "expanded" the default: a group that has never been toggled — including one
   * added to the nav in a later release — is absent from this set and therefore
   * renders open. Persisting the expanded set instead would silently hide every
   * new group from everyone who already has a stored preference.
   *
   * A plain `Set` in `$state` is fine here: every mutation below reassigns the
   * field, so reactivity does not depend on the proxy tracking `Set` methods.
   */
  collapsed = $state<Set<string>>(new Set());

  constructor() {
    this.collapsed = initialCollapsed();
  }

  isCollapsed(label: string): boolean {
    return this.collapsed.has(label);
  }

  toggle(label: string): void {
    const next = new Set(this.collapsed);
    if (!next.delete(label)) next.add(label);
    this.write(next);
  }

  /** Idempotent — used by the route-change effect, which fires on every navigation. */
  expand(label: string): void {
    if (!this.collapsed.has(label)) return;
    const next = new Set(this.collapsed);
    next.delete(label);
    this.write(next);
  }

  private write(next: Set<string>): void {
    this.collapsed = next;
    if (typeof window === 'undefined') return;
    // Private-mode Safari throws on setItem against a full quota. Collapsing a
    // nav group is cosmetic; losing persistence must not break the click.
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify([...next]));
    } catch {
      /* keep the in-memory value */
    }
  }
}

export const navCollapseStore = new NavCollapseStore();
