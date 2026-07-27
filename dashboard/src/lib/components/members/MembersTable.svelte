<script lang="ts">
  import Card from '../ui/Card.svelte';
  import Badge from '../ui/Badge.svelte';
  import Button from '../ui/Button.svelte';
  import { sessionStore } from '../../stores/session.svelte';
  import { initials } from '../../utils/format';
  import type { App, Member, MemberGrant, ScopeType } from '../../models';

  interface Props {
    grouped: Member[];
    appsById: Record<string, App>;
    /** Names for the projects an app grant can hang off. Optional — without it
        an app chip just drops its project prefix instead of breaking. */
    projectsById?: Record<string, { name: string }>;
    canManage: boolean;
    /** Grant id mid-removal, if any — only that chip shows busy styling, but
        every chip is disabled while it's in flight since deleteGrant() only
        allows one removal at a time. */
    removingId: string | null;
    /** User id whose active/inactive toggle is in flight. */
    togglingUserId: string | null;
    onedit: (userId: string) => void;
    ontoggle: (member: Member) => void;
    onremovegrant: (grantId: string) => void;
  }

  let {
    grouped,
    appsById,
    projectsById = {},
    canManage,
    removingId,
    togglingUserId,
    onedit,
    ontoggle,
    onremovegrant,
  }: Props = $props();

  function projectName(id: string): string | undefined {
    return projectsById[id]?.name ?? sessionStore.projects.find((x) => x.id === id)?.name;
  }

  function scopeLabel(member: MemberGrant): string {
    if (member.scope_type === 'org') return 'Org';
    if (member.scope_type === 'project') {
      return `Project: ${projectName(member.scope_id) ?? member.scope_id.slice(0, 8)}`;
    }
    // Grants can point at a deleted project/app — delete_project/delete_app
    // don't cascade to role_grants — so every lookup falls back a step.
    const a = appsById[member.scope_id];
    if (!a) return `App: ${member.scope_id.slice(0, 8)}`;
    const p = projectName(a.project_id);
    return p ? `App: ${p} / ${a.name}` : `App: ${a.name}`;
  }

  function scopeTone(type: ScopeType): 'primary' | 'info' | 'neutral' {
    if (type === 'org') return 'primary';
    if (type === 'project') return 'info';
    return 'neutral';
  }
</script>

<Card padding="none">
  <div class="table-scroll">
    <table class="members">
      <thead>
        <tr>
          <th>Member</th>
          <th>Role</th>
          <th>Scope</th>
          {#if canManage}<th class="col-act"></th>{/if}
        </tr>
      </thead>
      <tbody>
        {#each grouped as member (member.user_id)}
          <tr class:inactive={!member.is_active}>
            <td>
              <div class="member-cell">
                <span class="m-avatar">{initials(member.name || member.email)}</span>
                <div class="m-meta">
                  <span class="m-name-row">
                    <span class="m-name">{member.name || member.email}</span>
                    {#if !member.is_active}<Badge tone="warning" size="sm">Deactivated</Badge>{/if}
                  </span>
                  {#if member.name}<span class="m-email">{member.email}</span>{/if}
                </div>
              </div>
            </td>
            <td>
              <div class="chip-list">
                {#each member.grants as grant (grant.id)}
                  <Badge size="sm">{grant.role_name}</Badge>
                {/each}
              </div>
            </td>
            <td>
              <div class="chip-list">
                {#each member.grants as grant (grant.id)}
                  <span class="scope-chip">
                    <Badge tone={scopeTone(grant.scope_type)} size="sm">{scopeLabel(grant)}</Badge>
                    {#if canManage}
                      <button
                        type="button"
                        class="chip-remove"
                        class:removing={removingId === grant.id}
                        aria-label={`Remove ${scopeLabel(grant)} access`}
                        title="Remove access"
                        disabled={removingId !== null}
                        onclick={() => onremovegrant(grant.id)}
                      >
                        ×
                      </button>
                    {/if}
                  </span>
                {/each}
              </div>
            </td>
            {#if canManage}
              <td class="col-act">
                <div class="row-actions">
                  <Button variant="ghost" size="sm" onclick={() => onedit(member.user_id)}>
                    Edit
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    loading={togglingUserId === member.user_id}
                    onclick={() => ontoggle(member)}
                  >
                    {member.is_active ? 'Deactivate' : 'Reactivate'}
                  </Button>
                </div>
              </td>
            {/if}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</Card>

<style>
  .table-scroll {
    overflow-x: auto;
  }
  table.members {
    width: 100%;
    border-collapse: collapse;
    font-size: 13.5px;
  }
  thead th {
    text-align: left;
    padding: 12px 16px;
    font-size: 11px;
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
    border-bottom: 1px solid var(--border);
    white-space: nowrap;
  }
  td {
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
    vertical-align: middle;
  }
  tbody tr:last-child td {
    border-bottom: none;
  }
  tr.inactive td:not(.col-act) {
    opacity: 0.58;
  }
  .col-act {
    text-align: right;
    width: 1%;
    white-space: nowrap;
  }
  .row-actions {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
  }
  .member-cell {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .m-avatar {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--primary-soft);
    color: var(--primary);
    font-size: 11px;
    font-weight: 650;
    flex-shrink: 0;
  }
  .m-meta {
    display: flex;
    flex-direction: column;
    line-height: 1.3;
    gap: 2px;
  }
  .m-name-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .m-name {
    font-weight: 560;
  }
  .m-email {
    font-size: 11.5px;
    color: var(--text-faint);
  }
  .chip-list {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .scope-chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }
  .chip-remove {
    display: inline-grid;
    place-items: center;
    width: 16px;
    height: 16px;
    padding: 0;
    border: none;
    border-radius: 50%;
    background: transparent;
    color: var(--text-faint);
    font-size: 13px;
    line-height: 1;
    cursor: pointer;
  }
  .chip-remove:hover:not(:disabled) {
    background: var(--surface-3);
    color: var(--error);
  }
  .chip-remove:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }
  .chip-remove.removing {
    opacity: 1;
    color: var(--error);
    background: var(--surface-3);
    cursor: wait;
  }
</style>
