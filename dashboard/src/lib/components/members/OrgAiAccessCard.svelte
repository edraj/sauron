<script lang="ts">
  import { t } from '../../i18n';
  import Card from '../ui/Card.svelte';
  import Skeleton from '../ui/Skeleton.svelte';
  import ErrorState from '../ui/ErrorState.svelte';
  import { lockTip } from '../../actions/lock-tip';
  import { lockedBy } from '../../models/page-access';
  import { getOrgSettings, updateOrgSettings } from '../../api/orgs';
  import { errorMessage } from '../../api/client';
  import { sessionStore } from '../../stores/session.svelte';
  import { toastStore } from '../../stores/toast.svelte';

  // Org-level permission: the level must be passed, it defaults to 'app'.
  const lock = $derived(lockedBy('org:manage', { level: 'org' }));
  // No org selected: there is no setting to show, so no toggle either (it
  // would show the previous org's value, or a default posing as one).
  const hasOrg = $derived(Boolean(sessionStore.currentOrgId));

  // Plain booleans: only the value matters, never identity.
  let enabled = $state(false);
  let loading = $state(true);
  let saving = $state(false);
  let loadError = $state.raw<unknown>(null);
  // Guards against a slow response for a previous org landing after a switch:
  // bumped on every load, so a GET or a PATCH that started under an older
  // value belongs to an org this card no longer shows.
  let token = 0;

  async function load(): Promise<void> {
    const orgId = sessionStore.currentOrgId;
    const mine = ++token;
    // A PATCH still in flight for the previous org must not keep this org's
    // toggle disabled; its result is discarded by the token check in onToggle.
    saving = false;
    if (!orgId) {
      // Nothing to load: end the skeleton, which would otherwise never stop,
      // and drop the previous org's value. The toggle is hidden without an
      // org (and onToggle bails on !orgId).
      enabled = false;
      loadError = null;
      loading = false;
      return;
    }
    loading = true;
    loadError = null;
    try {
      const s = await getOrgSettings(orgId);
      if (mine !== token) return;
      enabled = s.mcp_enabled;
    } catch (err) {
      if (mine !== token) return;
      loadError = err;
    } finally {
      if (mine === token) loading = false;
    }
  }

  $effect(() => {
    void sessionStore.currentOrgId;
    void load();
  });

  async function onToggle(e: Event): Promise<void> {
    const box = e.currentTarget as HTMLInputElement;
    const orgId = sessionStore.currentOrgId;
    if (lock !== null || saving || !orgId) {
      box.checked = enabled;
      return;
    }
    const next = box.checked;
    // The org this PATCH is for. If the card switches org (or reloads) while
    // it is in flight, its answer is about an org no longer shown: no state
    // write and no toast, or the old org's value and message would land in
    // the new org's card.
    const mine = token;
    saving = true;
    try {
      const s = await updateOrgSettings(orgId, { mcp_enabled: next });
      if (mine !== token) return;
      enabled = s.mcp_enabled;
      box.checked = enabled;
      toastStore.success(t(enabled ? 'members.aiAccess.toast.on' : 'members.aiAccess.toast.off'));
    } catch (err) {
      if (mine !== token) return;
      // Revert to the server value: the element was mutated by the click.
      box.checked = enabled;
      toastStore.error(errorMessage(err));
    } finally {
      if (mine === token) {
        saving = false;
      } else if (sessionStore.currentOrgId === orgId) {
        // A→B→A: the card is showing this org again, but its GET may have
        // been answered before this PATCH landed. Reload, so it shows what
        // the server holds now rather than the value from before the PATCH.
        void load();
      }
    }
  }
</script>

<Card title={t('members.aiAccess.title')}>
  {#if loading}
    <Skeleton />
  {:else if loadError}
    <ErrorState
      size="section"
      title={t('common.couldntLoad', { thing: t('members.aiAccess.thing') })}
      error={loadError}
      onretry={() => void load()}
    />
  {:else}
    {#if hasOrg}
      <label class="toggle">
        <input
          type="checkbox"
          checked={enabled}
          disabled={saving}
          use:lockTip={lock}
          onchange={onToggle}
        />
        <span>{t('members.aiAccess.toggle')}</span>
      </label>
    {/if}
    <p class="muted body">{t('members.aiAccess.body')}</p>
  {/if}
</Card>

<style>
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .body {
    margin: 8px 0 0;
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
</style>
