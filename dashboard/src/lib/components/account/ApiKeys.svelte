<script lang="ts">
  import { t } from '../../i18n';
  import { apiBaseUrl } from '../../config/env';
  import { errorMessage, errorStatus } from '../../api/client';
  import { createApiKey, listApiKeys, revokeApiKey } from '../../api/account';
  import { mcpAddCommand } from '../../mcp-command';
  import { revokedReasonKey } from '../../models/api-keys';
  import { CachedView } from '../../stores/cached-view.svelte';
  import { viewCache, viewKey } from '../../stores/view-cache';
  import { sessionStore } from '../../stores/session.svelte';
  import { toastStore } from '../../stores/toast.svelte';
  import type { ApiKey, ApiKeyExpiry } from '../../models';
  import Card from '../ui/Card.svelte';
  import Button from '../ui/Button.svelte';
  import Badge from '../ui/Badge.svelte';
  import Input from '../ui/Input.svelte';
  import Select from '../ui/Select.svelte';
  import Modal from '../ui/Modal.svelte';
  import InlineNotice from '../ui/InlineNotice.svelte';
  import CodeBlock from '../ui/CodeBlock.svelte';
  import CopyButton from '../ui/CopyButton.svelte';
  import Skeleton from '../ui/Skeleton.svelte';
  import EmptyState from '../ui/EmptyState.svelte';
  import ErrorState from '../ui/ErrorState.svelte';
  import ConfirmDialog from '../ui/ConfirmDialog.svelte';
  import DataTable from '../DataTable.svelte';
  import TimeValue from '../TimeValue.svelte';

  const KEYS_VIEW = 'account.apiKeys';

  const view = new CachedView<ApiKey[]>();
  const keys = $derived(view.data ?? []);

  // Revoked keys, with when and why (you, an admin, a password reset), on
  // request, like the Sessions card's recent sign-outs. Part of the view key,
  // so flipping it paints the cached list for the other setting at once.
  let showRevoked = $state(false);

  async function load(force = false) {
    await view.load(
      viewKey(KEYS_VIEW, sessionStore.scopeKey, showRevoked),
      () => listApiKeys(showRevoked),
      force,
    );
  }

  /** For the page's Refresh button (`bind:this` in Account.svelte). */
  export async function refresh() {
    await load(true);
  }

  // Create dialog. The token lives ONLY in this component's state while the
  // reveal modal is open: not in a store, not in storage, not in the view cache
  // (the list endpoint never returns it), and never logged. Cleared on close.
  let dialogOpen = $state(false);
  let name = $state('');
  let expiry = $state<ApiKeyExpiry>('90d');
  let saving = $state(false);
  let formError = $state<string | null>(null);
  let token = $state<string | null>(null);

  // Replaced wholesale, identity never compared: $state.raw.
  let pending = $state.raw<ApiKey | null>(null);
  let revoking = $state(false);

  const trimmed = $derived(name.trim());
  const canSubmit = $derived(trimmed.length >= 1 && trimmed.length <= 64 && !saving);

  const expiryOptions = $derived<{ value: ApiKeyExpiry; label: string }[]>([
    { value: '30d', label: t('account.apiKeys.expiry.30d') },
    { value: '90d', label: t('account.apiKeys.expiry.90d') },
    { value: '1y', label: t('account.apiKeys.expiry.1y') },
    { value: 'never', label: t('account.apiKeys.expiry.never') },
  ]);

  const command = $derived(
    token ? mcpAddCommand(apiBaseUrl, token, window.location.origin) : '',
  );

  function openDialog() {
    name = '';
    expiry = '90d';
    formError = null;
    token = null;
    dialogOpen = true;
  }

  async function submit() {
    if (!canSubmit) return;
    saving = true;
    formError = null;
    try {
      const created = await createApiKey(trimmed, expiry);
      if (!dialogOpen) {
        // The dialog was closed mid-request. Never hold a token nobody can see;
        // just make the new key appear in the list.
        token = null;
        viewCache.invalidate(KEYS_VIEW);
        await load(true);
        return;
      }
      // Reveal, do not close. This is the only time the token exists client-side.
      token = created.token;
    } catch (err) {
      const status = errorStatus(err);
      if (status === 409 || status === 400) {
        // The server's own message: the 25-key cap or the validation reason.
        formError = errorMessage(err);
      } else {
        toastStore.error(errorMessage(err));
      }
    } finally {
      saving = false;
    }
  }

  async function closeDialog() {
    const revealed = token !== null;
    dialogOpen = false;
    token = null;
    formError = null;
    if (revealed) {
      viewCache.invalidate(KEYS_VIEW);
      await load(true);
    }
  }

  async function confirmRevoke() {
    const target = pending;
    if (!target) return;
    revoking = true;
    try {
      await revokeApiKey(target.id);
      toastStore.success(t('account.apiKeys.toast.revoked'));
      pending = null;
      viewCache.invalidate(KEYS_VIEW);
      await load(true);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      revoking = false;
    }
  }

  $effect(() => {
    void load();
  });
</script>

<Card title={t('account.apiKeys.title')}>
  {#snippet actions()}
    <Button variant="ghost" size="sm" onclick={() => (showRevoked = !showRevoked)}>
      {showRevoked ? t('account.apiKeys.hideRevoked') : t('account.apiKeys.showRevoked')}
    </Button>
    <Button variant="primary" size="sm" onclick={openDialog}>{t('account.apiKeys.create')}</Button>
  {/snippet}

  <p class="explain muted">{t('account.apiKeys.explainer')}</p>
  <p class="explain explain-last muted">{t('account.apiKeys.revocationNote')}</p>

  {#if view.loading}
    <Skeleton rows={3} />
  {:else if view.error && view.data === undefined}
    <ErrorState
      size="section"
      title={t('common.couldntLoad', { thing: t('account.apiKeys.thing') })}
      error={view}
      onretry={() => void load(true)}
    />
  {:else if keys.length === 0}
    <EmptyState
      size="section"
      icon="key-round"
      title={t('account.apiKeys.empty.title')}
      description={t('account.apiKeys.empty.description')}
    />
  {:else}
    <DataTable>
      {#snippet head()}
        <tr>
          <th>{t('account.apiKeys.column.name')}</th>
          <th>{t('account.apiKeys.column.key')}</th>
          <th>{t('account.apiKeys.column.created')}</th>
          <th>{t('account.apiKeys.column.lastUsed')}</th>
          <th>{t('account.apiKeys.column.expires')}</th>
          <th aria-label={t('common.actions')}></th>
        </tr>
      {/snippet}
      {#snippet children()}
        {#each keys as k (k.id)}
          <tr class:revoked={Boolean(k.revoked_at)}>
            <td>{k.name}</td>
            <td><code>{k.prefix}…</code></td>
            <td><TimeValue value={k.created_at} /></td>
            <td>
              {#if k.last_used_at}
                <TimeValue value={k.last_used_at} />
              {:else}
                <span class="cell-muted">{t('account.apiKeys.never')}</span>
              {/if}
            </td>
            <!-- `wrap` (DataTable's opt-out of its no-wrap cells): the reason
                 line may break rather than widen the table. -->
            <td class:wrap={Boolean(k.revoked_at)}>
              {#if k.revoked_at}
                <span class="revoked-at">
                  <Badge tone="neutral" size="sm">{t('account.apiKeys.revoked')}</Badge>
                  <TimeValue value={k.revoked_at} />
                </span>
                <!-- Why it stopped working: you, an admin, or a password
                     reset. Under the status rather than in the actions
                     column, whose no-wrap would widen the whole table. -->
                <span class="revoked-why">{t(revokedReasonKey(k.revoked_reason))}</span>
              {:else if k.expired}
                <Badge tone="error" size="sm">{t('account.apiKeys.expired')}</Badge>
              {:else if k.expires_at}
                <TimeValue value={k.expires_at} />
              {:else}
                <span class="cell-muted">{t('account.apiKeys.never')}</span>
              {/if}
            </td>
            <td class="col-act">
              <!-- A revoked key has nothing left to act on. -->
              {#if !k.revoked_at}
                <Button variant="ghost" size="sm" onclick={() => (pending = k)}>
                  {t('account.apiKeys.revoke')}
                </Button>
              {/if}
            </td>
          </tr>
        {/each}
      {/snippet}
    </DataTable>
  {/if}
</Card>

<Modal
  open={dialogOpen}
  size="lg"
  title={token ? t('account.apiKeys.reveal.title') : t('account.apiKeys.dialog.title')}
  dismissible={token === null && !saving}
  onclose={() => void closeDialog()}
>
  {#if token}
    <InlineNotice tone="warning">{t('account.apiKeys.reveal.warning')}</InlineNotice>
    <p class="lede">{t('account.apiKeys.reveal.commandLabel')}</p>
    <CodeBlock code={command} language="bash" />
    <p class="lede">{t('account.apiKeys.reveal.tokenLabel')}</p>
    <div class="token-row">
      <code>{token}</code>
      <CopyButton value={token} size="sm" />
    </div>
  {:else}
    <div class="fields">
      <Input
        label={t('account.apiKeys.dialog.name')}
        bind:value={name}
        placeholder={t('account.apiKeys.dialog.namePlaceholder')}
        hint={t('account.apiKeys.dialog.nameHint')}
        required
      />
      <Select
        label={t('account.apiKeys.dialog.expiry')}
        bind:value={expiry}
        options={expiryOptions}
      />
    </div>
    {#if formError}<p class="err-msg" role="alert">{formError}</p>{/if}
  {/if}

  {#snippet footer()}
    {#if token}
      <Button variant="primary" onclick={() => void closeDialog()}>{t('common.done')}</Button>
    {:else}
      <Button variant="secondary" onclick={() => void closeDialog()} disabled={saving}>
        {t('common.cancel')}
      </Button>
      <Button variant="primary" disabled={!canSubmit} loading={saving} onclick={submit}>
        {t('account.apiKeys.dialog.submit')}
      </Button>
    {/if}
  {/snippet}
</Modal>

<ConfirmDialog
  danger
  open={pending !== null}
  title={t('account.apiKeys.confirm.title')}
  message={t('account.apiKeys.confirm.body', { name: pending?.name ?? '' })}
  confirmLabel={t('account.apiKeys.revoke')}
  loading={revoking}
  onconfirm={() => void confirmRevoke()}
  oncancel={() => (pending = null)}
/>

<style>
  .explain {
    font-size: var(--text-sm);
    margin-bottom: 6px;
  }
  .explain-last {
    margin-bottom: 14px;
  }
  .lede {
    font-size: var(--text-sm);
    color: var(--text-muted);
    margin: 14px 0 8px;
  }
  .fields {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-bottom: 12px;
  }
  .token-row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .token-row code {
    font-size: var(--text-xs);
    word-break: break-all;
  }
  .err-msg {
    color: var(--error);
    font-size: var(--text-sm);
  }
  .col-act {
    text-align: end;
    width: 1%;
    white-space: nowrap;
  }
  tr.revoked td:not(.col-act) {
    opacity: 0.58;
  }
  .revoked-at {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 6px;
  }
  .revoked-why {
    display: block;
    margin-top: 3px;
    font-size: var(--text-xs);
    color: var(--text-muted);
  }
</style>
