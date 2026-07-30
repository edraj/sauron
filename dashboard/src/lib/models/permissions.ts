import type { Permission } from './index';

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
  'role:manage',
  'org:manage',
  'alert:read',
  'alert:write',
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
  {
    label: 'Organization',
    permissions: ['member:read', 'member:manage', 'role:manage', 'org:manage'],
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
  'role:manage': 'Create and edit roles',
  'org:manage': 'Manage organization settings',
  'alert:read': 'View alert rules and channels',
  'alert:write': 'Create and edit alert rules and channels',
};
