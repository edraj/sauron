<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import NotificationSubscriptions from '../lib/components/account/NotificationSubscriptions.svelte';
  import { authStore } from '../lib/stores/auth.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { errorMessage } from '../lib/api/client';
  import { listMySessions, revokeMyOtherSessions, revokeMySession } from '../lib/api/account';
  import {
    allSameIp,
    describeSession,
    hasCurrentSession,
    otherSessionCount,
    sortSessions,
  } from '../lib/models/account-sessions';
  import type { AccountSession } from '../lib/models';

  let sessions = $state<AccountSession[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let showRevoked = $state(false);
  let busy = $state(false);

  // One dialog for both verbs, matching Members.svelte's requestToggle /
  // confirmDeactivate shape. `$state.raw` because this is replaced wholesale and
  // nothing reads through a proxy.
  let pending = $state.raw<{ kind: 'one'; id: string; label: string } | { kind: 'all' } | null>(
    null,
  );

  const rows = $derived(sortSessions(sessions));
  const live = $derived(rows.filter((s) => s.revoked_at === null));
  const revoked = $derived(rows.filter((s) => s.revoked_at !== null));
  const otherCount = $derived(otherSessionCount(sessions));
  const hasCurrent = $derived(hasCurrentSession(sessions));
  const proxied = $derived(allSameIp(sessions));

  async function load() {
    loading = true;
    error = null;
    try {
      sessions = await listMySessions(showRevoked);
    } catch (err) {
      error = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  async function toggleHistory() {
    showRevoked = !showRevoked;
    await load();
  }

  function requestRevokeOne(s: AccountSession) {
    pending = { kind: 'one', id: s.id, label: describeSession(s) };
  }

  function requestRevokeAll() {
    pending = { kind: 'all' };
  }

  async function confirmPending() {
    const target = pending;
    if (!target) return;
    busy = true;
    try {
      if (target.kind === 'one') {
        await revokeMySession(target.id);
        toastStore.success('That device will be signed out within a few seconds.');
      } else {
        const n = await revokeMyOtherSessions();
        toastStore.success(
          n === 1 ? 'One other device signed out.' : `${n} other devices signed out.`,
        );
      }
      pending = null;
      await load();
    } catch (err) {
      // The backend's 409/400/404 bodies carry the actionable text — surface it
      // verbatim rather than a generic failure.
      toastStore.error(errorMessage(err));
    } finally {
      busy = false;
    }
  }

  function reasonLabel(reason: string | null): string {
    switch (reason) {
      case 'logout':
        return 'Logged out';
      case 'user_revoked':
        return 'Signed out from your account page';
      case 'user_revoked_others':
        return 'Signed out with "other devices"';
      case 'admin_revoked':
        return 'Signed out by an administrator';
      case 'password_changed':
        return 'Password changed';
      case 'deactivated':
        return 'Account deactivated';
      case 'reuse':
        return 'Security: token replay detected';
      default:
        return 'Ended';
    }
  }

  $effect(() => {
    void load();
  });
</script>

<AppShell requireProject={false}>
  <div class="head">
    <div>
      <h1 class="page-title">Account</h1>
      <p class="sub muted">Your profile and the devices signed in to it.</p>
    </div>
    <RefreshButton onclick={() => void load()} loading={loading} />
  </div>

  {#if error}
    <div class="err-banner" role="alert">
      <Icon name="triangle-alert" size={15} />
      <span>{error}</span>
    </div>
  {/if}

  <div class="cards">
    <Card title="Profile">
      <dl class="profile">
        <dt>Name</dt>
        <dd>{authStore.user?.name || '—'}</dd>
        <dt>Email</dt>
        <dd class="cell-mono">{authStore.user?.email ?? '—'}</dd>
        <dt>Last sign-in</dt>
        <dd><TimeValue value={authStore.user?.last_login_at} /></dd>
      </dl>
      <div class="profile-actions">
        <Button variant="secondary" href="#/change-password">Change password</Button>
      </div>
    </Card>

    <Card title="Active sessions" padding="none">
      {#snippet actions()}
        <Button variant="ghost" size="sm" onclick={() => void toggleHistory()}>
          {showRevoked ? 'Hide recent sign-outs' : 'Show recent sign-outs'}
        </Button>
        <Button
          variant="danger"
          size="sm"
          disabled={otherCount === 0 || !hasCurrent}
          onclick={requestRevokeAll}
        >
          Sign out other devices
        </Button>
      {/snippet}

      {#if !hasCurrent && !loading && live.length > 0}
        <div class="err-banner inset" role="status">
          <Icon name="info" size={15} />
          <span>Reload the dashboard to manage your devices.</span>
        </div>
      {/if}

      {#if loading}
        <div class="center"><Spinner size={24} /></div>
      {:else if live.length === 0}
        <div class="pad">
          <EmptyState title="No active sessions" description="Sign in again to see this device." />
        </div>
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <th>Device</th>
              <th>IP</th>
              <th>Signed in</th>
              <th>Last used</th>
              <th aria-label="actions"></th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each live as s (s.id)}
              <tr>
                <td>
                  <span class="device">
                    {describeSession(s)}
                    {#if s.current}<Badge tone="primary" size="sm">This device</Badge>{/if}
                  </span>
                </td>
                <td class="cell-mono cell-muted">{s.ip ?? '—'}</td>
                <td><TimeValue value={s.created_at} /></td>
                <td><TimeValue value={s.last_used_at} /></td>
                <td class="col-act">
                  {#if !s.current}
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={!hasCurrent}
                      onclick={() => requestRevokeOne(s)}
                    >
                      Sign out
                    </Button>
                  {/if}
                </td>
              </tr>
            {/each}
            {#each revoked as s (s.id)}
              <tr class="dim">
                <td>{describeSession(s)}</td>
                <td class="cell-mono cell-muted">{s.ip ?? '—'}</td>
                <td><TimeValue value={s.created_at} /></td>
                <td>Signed out {#if s.revoked_at}<TimeValue value={s.revoked_at} />{/if}</td>
                <td class="col-act cell-muted">{reasonLabel(s.revoked_reason)}</td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>

        {#if proxied}
          <p class="hint muted">
            All sessions show the same address — the API is behind a proxy and
            <code>API_TRUST_FORWARDED_HEADERS</code> is not set.
          </p>
        {/if}
      {/if}
    </Card>

    <NotificationSubscriptions />
  </div>
</AppShell>

<ConfirmDialog
  danger
  open={pending !== null}
  title={pending?.kind === 'all' ? 'Sign out other devices' : 'Sign out this device'}
  message={pending?.kind === 'all'
    ? 'Every device except this one will be signed out. You will stay logged in here.'
    : `${pending?.kind === 'one' ? pending.label : 'That device'} will be signed out within a few seconds and will have to log in again.`}
  confirmLabel="Sign out"
  loading={busy}
  onconfirm={() => void confirmPending()}
  oncancel={() => (pending = null)}
/>

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 18px;
  }
  .cards {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .profile {
    display: grid;
    grid-template-columns: 130px 1fr;
    row-gap: 8px;
    column-gap: 12px;
    margin: 0;
    font-size: 13.5px;
  }
  .profile dt {
    color: var(--text-faint);
  }
  .profile dd {
    margin: 0;
  }
  .profile-actions {
    margin-top: 16px;
  }
  .device {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .col-act {
    text-align: right;
    width: 1%;
    white-space: nowrap;
  }
  tr.dim td {
    opacity: 0.55;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 36px 0;
  }
  .pad {
    padding: 18px;
  }
  .hint {
    margin: 10px 16px 14px;
    font-size: 12px;
  }
  .err-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    margin-bottom: 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-2);
    font-size: 13px;
  }
  .err-banner.inset {
    margin: 14px 16px 0;
  }
</style>
