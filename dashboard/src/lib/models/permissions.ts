import type { Permission } from './index';
import { t, type MessageKey } from '../i18n';

/**
 * Every permission the backend recognises, mirroring perm::ALL in
 * backend/crates/sauron-auth/src/rbac.rs:56 in the same order.
 *
 * Kept complete on purpose. The role editor submits the full checkbox state,
 * so a permission missing from this list is a permission the editor silently
 * removes from any role that has it.
 */
export const ALL_PERMISSIONS: Permission[] = [
  'issue:read',
  'issue:write',
  'event:read',
  'funnel:write',
  'artifact:write',
  'source:read',
  'monitor:read',
  'monitor:write',
  'app:read',
  'app:create',
  'app:update',
  'app:delete',
  'env:read',
  'env:create',
  'env:update',
  'env:delete',
  'env:rotate_key',
  'project:read',
  'project:create',
  'project:update',
  'project:delete',
  'member:read',
  'member:manage',
  'member:credential',
  'role:manage',
  'org:manage',
  'alert:read',
  'alert:write',
  'pii:read',
  'pii:manage',
];

export interface PermissionGroup {
  label: string;
  permissions: Permission[];
}

/** Rendering order for the checkbox grid. Every permission appears once. */
export const PERMISSION_GROUPS: PermissionGroup[] = [
  { label: 'Issues & events', permissions: ['issue:read', 'issue:write', 'event:read'] },
  { label: 'Analytics', permissions: ['funnel:write'] },
  { label: 'Symbolication', permissions: ['artifact:write', 'source:read'] },
  { label: 'Uptime', permissions: ['monitor:read', 'monitor:write'] },
  {
    label: 'Apps',
    permissions: ['app:read', 'app:create', 'app:update', 'app:delete'],
  },
  {
    label: 'Environments',
    permissions: ['env:read', 'env:create', 'env:update', 'env:delete', 'env:rotate_key'],
  },
  {
    label: 'Projects',
    permissions: ['project:read', 'project:create', 'project:update', 'project:delete'],
  },
  { label: 'Alerting', permissions: ['alert:read', 'alert:write'] },
  { label: 'Privacy', permissions: ['pii:read', 'pii:manage'] },
  {
    label: 'Organization',
    permissions: ['member:read', 'member:manage', 'member:credential', 'role:manage', 'org:manage'],
  },
];

export const PERMISSION_LABELS: Record<string, string> = {
  'issue:read': 'View issues',
  'issue:write': 'Resolve, assign, and comment on issues',
  'event:read': 'View raw events and analytics',
  'funnel:write': 'Create and edit funnels',
  'artifact:write': 'Upload source maps and debug symbols',
  'source:read': 'View de-obfuscated source code in stack traces',
  'monitor:read': 'View uptime monitors',
  'monitor:write': 'Create and edit uptime monitors',
  'app:read': 'View apps',
  'app:create': 'Create apps',
  'app:update': 'Edit app settings',
  'app:delete': 'Delete apps',
  'env:read': 'View environments and their ingest keys',
  'env:create': 'Create environments',
  'env:update': 'Rename environments, mute ingest, change the default',
  'env:delete': 'Retire environments',
  'env:rotate_key': 'Rotate environment ingest keys',
  'project:read': 'View projects',
  'project:create': 'Create projects',
  'project:update': 'Edit project settings',
  'project:delete': 'Delete projects',
  'member:read': 'View members',
  'member:manage': 'Add, edit, and deactivate members',
  'member:credential': 'Reset passwords and sign out devices',
  'role:manage': 'Create and edit roles',
  'org:manage': 'Manage organization settings',
  'alert:read': 'View alert rules and channels',
  'alert:write': 'Create and edit alert rules and channels',
  'pii:read': 'View PII scan findings, the mask audit trail, and reveal single values',
  'pii:manage': 'Configure scans and permanently mask values (irreversible)',
};

/**
 * Catalog keys for the same labels, for surfaces that render them in the
 * active language (the lock tooltip). `PERMISSION_LABELS` above stays English
 * for its existing consumers. `permissions.test.ts` checks that every entry
 * of `ALL_PERMISSIONS` has a key here and that its English matches
 * `PERMISSION_LABELS` exactly, so the two cannot drift apart. (`Permission`
 * admits any string, so the type alone cannot enforce completeness.)
 */
export const PERMISSION_LABEL_KEYS: Record<string, MessageKey> = {
  'issue:read': 'access.perm.issueRead',
  'issue:write': 'access.perm.issueWrite',
  'event:read': 'access.perm.eventRead',
  'funnel:write': 'access.perm.funnelWrite',
  'artifact:write': 'access.perm.artifactWrite',
  'source:read': 'access.perm.sourceRead',
  'monitor:read': 'access.perm.monitorRead',
  'monitor:write': 'access.perm.monitorWrite',
  'app:read': 'access.perm.appRead',
  'app:create': 'access.perm.appCreate',
  'app:update': 'access.perm.appUpdate',
  'app:delete': 'access.perm.appDelete',
  'env:read': 'access.perm.envRead',
  'env:create': 'access.perm.envCreate',
  'env:update': 'access.perm.envUpdate',
  'env:delete': 'access.perm.envDelete',
  'env:rotate_key': 'access.perm.envRotateKey',
  'project:read': 'access.perm.projectRead',
  'project:create': 'access.perm.projectCreate',
  'project:update': 'access.perm.projectUpdate',
  'project:delete': 'access.perm.projectDelete',
  'member:read': 'access.perm.memberRead',
  'member:manage': 'access.perm.memberManage',
  'member:credential': 'access.perm.memberCredential',
  'role:manage': 'access.perm.roleManage',
  'org:manage': 'access.perm.orgManage',
  'alert:read': 'access.perm.alertRead',
  'alert:write': 'access.perm.alertWrite',
  'pii:read': 'access.perm.piiRead',
  'pii:manage': 'access.perm.piiManage',
};

/**
 * A permission's human label in the active language, or `undefined` for a
 * permission this dashboard does not know yet. Call at render time.
 */
export function permissionLabel(perm: string): string | undefined {
  const key: MessageKey | undefined = PERMISSION_LABEL_KEYS[perm];
  return key ? t(key) : undefined;
}

const PERMISSION_GROUP_KEYS: Record<string, MessageKey> = {
  'Issues & events': 'access.group.issuesEvents',
  Analytics: 'access.group.analytics',
  Symbolication: 'access.group.symbolication',
  Uptime: 'access.group.uptime',
  Apps: 'access.group.apps',
  Environments: 'access.group.environments',
  Projects: 'access.group.projects',
  Alerting: 'access.group.alerting',
  Privacy: 'access.group.privacy',
  Organization: 'access.group.organization',
};

/**
 * A permission group's heading in the active language. `group.label` stays
 * the English id (the picker keys its open/closed state by it, and tests find
 * groups by it); this is only what a person reads. Call at render time.
 */
export function permissionGroupLabel(group: PermissionGroup): string {
  const key = PERMISSION_GROUP_KEYS[group.label];
  return key ? t(key) : group.label;
}
