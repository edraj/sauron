<script lang="ts">
  import { t } from '../../i18n';
  import Card from '../ui/Card.svelte';
  import Badge from '../ui/Badge.svelte';
  import RowActionsMenu from '../ui/RowActionsMenu.svelte';
  import { sessionStore } from '../../stores/session.svelte';
  import { initials } from '../../utils/format';
  import { canCancelPasswordReset, canResetMemberPassword } from '../../models/password-reset';
    import { lockTip } from '../../actions/lock-tip';
  import Icon from '../ui/Icon.svelte';
  import type {
    App,
    AppEnvironment,
    Member,
    MemberGrant,
    Permission,
    ScopeType,
  } from '../../models';

  interface Props {
    grouped: Member[];
    appsById: Record<string, App>;
    /** Environments per app, keyed by app id. Only ever populated for apps
        whose row has been expanded somewhere in the scope tree this session —
        see ScopeTree's doc comment — so an env chip falls back to a truncated
        id until then, same as a genuinely deleted target would. */
    envsByApp?: Record<string, AppEnvironment[]>;
    /** Names for the projects an app grant can hang off. Optional — without it
        an app chip just drops its project prefix instead of breaking. */
    projectsById?: Record<string, { name: string }>;
    /** Missing permission for member administration, or `null` if allowed.
        A lock rather than a boolean so each control can say WHY it is off. */
    manageLock: Permission | null;
    /** Grant id mid-removal, if any — only that chip shows busy styling, but
        every chip is disabled while it's in flight since deleteGrant() only
        allows one removal at a time. */
    removingId: string | null;
    /** User id whose active/inactive toggle is in flight. */
    togglingUserId: string | null;
    /** `member:credential`, NOT `manageLock`. A custom role can hold
        `member:manage` without it — that is the whole point of the carve-out —
        and showing the button to that role means every click 403s. */
    revokeLock: Permission | null;
    /** User id whose force-logout is in flight. */
    revokingUserId: string | null;
    onrevokesessions: (member: Member) => void;
    onedit: (userId: string) => void;
    ontoggle: (member: Member) => void;
    /** Id of the signed-in user, for the self-check the server also makes. */
    currentUserId: string;
    /** `member:credential` AND `member:manage` — the server requires both, so a
        menu gating on either one alone offers an action the server refuses.
        Holds whichever of the two is missing. */
    credentialLock: Permission | null;
    /** ONE callback rather than two, so the table cannot offer a member both a
        reset and a cancel. */
    onresetpassword: (member: Member, action: 'reset' | 'cancel') => void;
    onremovegrant: (grantId: string) => void;
  }

  let {
    grouped,
    appsById,
    envsByApp = {},
    projectsById = {},
    manageLock,
    removingId,
    togglingUserId,
    revokeLock,
    revokingUserId,
    onrevokesessions,
    onedit,
    ontoggle,
    currentUserId,
    credentialLock,
    onresetpassword,
    onremovegrant,
  }: Props = $props();

  const envsById = $derived.by(() => {
    const map: Record<string, AppEnvironment> = {};
    for (const list of Object.values(envsByApp)) for (const e of list) map[e.id] = e;
    return map;
  });

  function projectName(id: string): string | undefined {
    return projectsById[id]?.name ?? sessionStore.projects.find((x) => x.id === id)?.name;
  }

  function scopeLabel(member: MemberGrant): string {
    if (member.scope_type === 'org') return 'Org';
    if (member.scope_type === 'project') {
      return `Project: ${projectName(member.scope_id) ?? member.scope_id.slice(0, 8)}`;
    }
    if (member.scope_type === 'env') {
      // scope_id carries no FK, and — unlike project/app — an env may simply
      // never have been fetched yet (see envsByApp's doc comment above), not
      // only deleted. Either way the fallback is the same truncated id.
      const env = envsById[member.scope_id];
      if (!env) return `Env: ${member.scope_id.slice(0, 8)}`;
      const a = appsById[env.app_id];
      return a ? `Env: ${a.name} / ${env.name}` : `Env: ${env.name}`;
    }
    // Grants can point at a deleted project/app — delete_project/delete_app
    // don't cascade to role_grants — so every lookup falls back a step.
    const a = appsById[member.scope_id];
    if (!a) return `App: ${member.scope_id.slice(0, 8)}`;
    const p = projectName(a.project_id);
    return p ? `App: ${p} / ${a.name}` : `App: ${a.name}`;
  }

  function scopeTone(type: ScopeType): 'primary' | 'info' | 'neutral' | 'success' {
    if (type === 'org') return 'primary';
    if (type === 'project') return 'info';
    if (type === 'env') return 'success';
    return 'neutral';
  }
</script>

<Card padding="none">
  <div class="table-scroll">
    <table class="members">
      <thead>
        <tr>
          <th>{t('members.column.member')}</th>
          <th>{t('members.column.role')}</th>
          <th>{t('members.column.scope')}</th>
          <th class="col-act"></th>
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
                    {#if !member.is_active}<Badge tone="warning" size="sm">{t('members.deactivated')}</Badge>{/if}
                    {#if member.credentials_invalidated_at}
                      <!-- An account nobody can sign in to is a state the table has
                           to show without being opened: the admin who forced it may
                           not be the one fielding "I can't log in". -->
                      <Badge tone="warning" size="sm">{t('members.resetPending')}</Badge>
                    {/if}
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
                    <button
                      type="button"
                      class="chip-remove"
                      class:removing={removingId === grant.id}
                      aria-label={`Remove ${scopeLabel(grant)} access`}
                      title={t('members.removeAccess')}
                      use:lockTip={manageLock}
                      disabled={removingId !== null}
                      onclick={() => onremovegrant(grant.id)}
                    >
                      ×
                    </button>
                  </span>
                {/each}
              </div>
            </td>
              <td class="col-act">
                <RowActionsMenu label={`Actions for ${member.email}`}>
                  {#snippet children(close)}
                    <button
                      type="button"
                      role="menuitem"
                      class="ram-item"
                      use:lockTip={manageLock}
                      onclick={() => {
                        close();
                        onedit(member.user_id);
                      }}
                    >
                      {#if manageLock}<span class="ram-lock" aria-hidden="true"
                          ><Icon name="lock" size={12} /></span
                        >{/if}Edit
                    </button>
                    <!-- The helpers are called with `true` for the permission
                         so they answer only the member-state question (self?
                         active? reset already pending?). Permission is applied
                         as a lock below instead, so the item stays visible and
                         explains itself rather than vanishing. -->
                    {#if canResetMemberPassword(member, currentUserId, true)}
                      <button
                        type="button"
                        role="menuitem"
                        class="ram-item"
                        use:lockTip={credentialLock}
                        onclick={() => {
                          close();
                          onresetpassword(member, 'reset');
                        }}
                      >
                        {#if credentialLock}<span class="ram-lock" aria-hidden="true"
                            ><Icon name="lock" size={12} /></span
                          >{/if}Reset password
                      </button>
                    {:else if canCancelPasswordReset(member, currentUserId, true)}
                      <button
                        type="button"
                        role="menuitem"
                        class="ram-item"
                        use:lockTip={credentialLock}
                        onclick={() => {
                          close();
                          onresetpassword(member, 'cancel');
                        }}
                      >
                        {#if credentialLock}<span class="ram-lock" aria-hidden="true"
                            ><Icon name="lock" size={12} /></span
                          >{/if}Cancel password reset
                      </button>
                    {/if}
                    <!-- Signing yourself out of every device from the members
                         table is not a permission question — the server refuses
                         it outright — so this stays an `{#if}`, not a lock. -->
                    {#if member.user_id !== currentUserId}
                      <button
                        type="button"
                        role="menuitem"
                        class="ram-item"
                        disabled={revokingUserId === member.user_id}
                        use:lockTip={revokeLock}
                        onclick={() => {
                          close();
                          onrevokesessions(member);
                        }}
                      >
                        {#if revokeLock}<span class="ram-lock" aria-hidden="true"
                            ><Icon name="lock" size={12} /></span
                          >{/if}Sign out all devices
                      </button>
                    {/if}
                    <button
                      type="button"
                      role="menuitem"
                      class="ram-item danger"
                      disabled={togglingUserId === member.user_id}
                      use:lockTip={manageLock}
                      onclick={() => {
                        close();
                        ontoggle(member);
                      }}
                    >
                      {#if manageLock}<span class="ram-lock" aria-hidden="true"
                          ><Icon name="lock" size={12} /></span
                        >{/if}{member.is_active ? 'Deactivate' : 'Reactivate'}
                    </button>
                  {/snippet}
                </RowActionsMenu>
              </td>
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
    text-align: start;
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
    text-align: end;
    width: 1%;
    white-space: nowrap;
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
