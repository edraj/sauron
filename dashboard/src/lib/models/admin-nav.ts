import type { IconName } from '../components/ui/Icon.svelte';
import type { Permission } from './index';
import { canAccessPage, pageLockedBy, resolvePageAccess } from './page-access';

export interface AdminNavItem {
  href: string;
  label: string;
  icon: IconName;
}

/**
 * The admin sub-nav, in display order.
 *
 * Deliberately carries no permission of its own: visibility is resolved
 * through PAGE_ACCESS exactly as Sidebar does, so the rail, the sidebar and
 * AppShell's in-page gate cannot disagree about who may see a page.
 */
export const ADMIN_NAV: AdminNavItem[] = [
  { href: '/admin/members', label: 'Members', icon: 'key-round' },
  { href: '/admin/roles', label: 'Roles', icon: 'shield-check' },
  { href: '/admin/projects', label: 'Projects', icon: 'folders' },
  { href: '/admin/environments', label: 'Environments', icon: 'layers' },
  { href: '/admin/settings', label: 'App settings', icon: 'settings' },
  { href: '/admin/source-maps', label: 'Source Maps', icon: 'braces' },
  { href: '/admin/alerts', label: 'Alerts', icon: 'bell' },
  { href: '/admin/storage', label: 'Storage', icon: 'server' },
  { href: '/admin/privacy', label: 'Privacy', icon: 'shield-alert' },
  { href: '/admin/wall-of-shame', label: 'Wall of Shame', icon: 'scroll-text' },
  { href: '/admin/ingest-failures', label: 'Ingest failures', icon: 'refresh' },
  { href: '/admin/purge', label: 'Purge data', icon: 'circle-x' },
];

/** An admin child plus the permission keeping it locked, if any. */
export interface LockedAdminNavItem extends AdminNavItem {
  /** `null` when the member may open it, else the permission they lack. */
  locked: Permission | null;
}

/**
 * Every admin child, in nav order, each carrying its lock reason.
 *
 * Length-invariant on purpose: the rail and the sidebar LOCK rather than hide,
 * so the list a member sees no longer depends on what they hold. Hiding meant
 * an Admin was silently short four of these twelve pages — a capability nobody
 * can ask for because nobody can see it exists.
 *
 * Distinct from [`visibleAdminNav`], which still filters: `AdminIndex` has to
 * redirect to a child the member can actually open, and a locked one is not
 * that.
 */
export function adminNavLocks(): LockedAdminNavItem[] {
  return ADMIN_NAV.map((i) => ({ ...i, locked: pageLockedBy(i.href) }));
}

/** Admin children the current user may open, in nav order. */
export function visibleAdminNav(): AdminNavItem[] {
  return ADMIN_NAV.filter((i) => canAccessPage(resolvePageAccess(i.href)));
}

/**
 * Where `/admin` should land. `null` when the user can reach no child at all,
 * which AdminIndex renders as a denial rather than a redirect loop.
 */
export function firstAccessibleAdminPath(): string | null {
  return visibleAdminNav()[0]?.href ?? null;
}
