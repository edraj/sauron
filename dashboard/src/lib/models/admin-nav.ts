import type { IconName } from '../components/ui/Icon.svelte';
import type { MessageKey } from '../i18n';
import { canAccessPage, pageLockedBy, resolvePageAccess, type LockReason } from './page-access';

export interface AdminNavItem {
  href: string;
  /** Catalog key, so the rail is translated like every other nav label. */
  labelKey: MessageKey;
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
  { href: '/admin/members', labelKey: 'admin.nav.members', icon: 'key-round' },
  { href: '/admin/roles', labelKey: 'admin.nav.roles', icon: 'shield-check' },
  { href: '/admin/projects', labelKey: 'admin.nav.projects', icon: 'folders' },
  { href: '/admin/environments', labelKey: 'admin.nav.environments', icon: 'layers' },
  { href: '/admin/settings', labelKey: 'admin.nav.settings', icon: 'settings' },
  { href: '/admin/source-maps', labelKey: 'admin.nav.sourceMaps', icon: 'braces' },
  { href: '/admin/alerts', labelKey: 'admin.nav.alerts', icon: 'bell' },
  { href: '/admin/storage', labelKey: 'admin.nav.storage', icon: 'server' },
  { href: '/admin/privacy', labelKey: 'admin.nav.privacy', icon: 'shield-alert' },
  { href: '/admin/wall-of-shame', labelKey: 'admin.nav.wallOfShame', icon: 'scroll-text' },
  { href: '/admin/ingest-failures', labelKey: 'admin.nav.ingestFailures', icon: 'refresh' },
  { href: '/admin/purge', labelKey: 'admin.nav.purge', icon: 'circle-x' },
];

/** An admin child plus the permission keeping it locked, if any. */
export interface LockedAdminNavItem extends AdminNavItem {
  /** `null` when the member may open it, else what they lack. */
  locked: LockReason | null;
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
