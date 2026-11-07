<script lang="ts">
  import { t } from '../../i18n';
  import { untrack } from 'svelte';
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import Select from '../ui/Select.svelte';
  import InlineNotice from '../ui/InlineNotice.svelte';
  import CopyButton from '../ui/CopyButton.svelte';
  import ScopeTree from './ScopeTree.svelte';
  import { createMember } from '../../api/orgs';
  import { errorMessage } from '../../api/client';
  import {
    EMPTY_SELECTION,
    isEmptySelection,
    selectionToScopes,
    type ScopeSelection,
  } from '../../models/scope-tree';
  import type { App, AppEnvironment, Project, Role } from '../../models';

  interface Props {
    open: boolean;
    orgId: string;
    orgName: string;
    roles: Role[];
    projects: Project[];
    appsByProject: Record<string, App[]>;
    /** Environments per app, keyed by app id — see ScopeTree's own doc comment.
        Owned by the parent (Members.svelte) so the same cache can be reused by
        the grant form, the edit dialog, and the members table. */
    envsByApp: Record<string, AppEnvironment[]>;
    loadingEnvApps: Set<string>;
    onopenapp: (appId: string) => void;
    onclose: () => void;
    oncreated: () => void;
  }

  let {
    open,
    orgId,
    orgName,
    roles,
    projects,
    appsByProject,
    envsByApp,
    loadingEnvApps,
    onopenapp,
    onclose,
    oncreated,
  }: Props = $props();

  let email = $state('');
  let name = $state('');
  let roleId = $state('');
  // Fresh arrays — EMPTY_SELECTION's are frozen, and $state proxies what it is
  // handed. Nothing is preselected: the org tick is the broadest grant there is.
  let selection = $state<ScopeSelection>({ ...EMPTY_SELECTION, projects: [], apps: [], envs: [] });
  let saving = $state(false);
  let error = $state<string | null>(null);
  /** Set once the account exists. The dialog switches to the reveal panel. */
  let tempPassword = $state<string | null>(null);

  const projectOfApp = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const p of projects) for (const a of appsByProject[p.id] ?? []) map[a.id] = p.id;
    return map;
  });

  const appOfEnv = $derived.by(() => {
    const map: Record<string, string> = {};
    for (const [appId, envs] of Object.entries(envsByApp)) {
      for (const e of envs) map[e.id] = appId;
    }
    return map;
  });

  // Repopulate on the false -> true transition only. Every prop this reads is
  // read inside `untrack` — a parent-triggered reload replaces `roles`,
  // `projects` and `appsByProject` with new references while the dialog is
  // still open (e.g. right after a successful create), and tracking any of them
  // would re-run this effect and wipe the one-time temp-password reveal panel
  // underneath the admin.
  $effect(() => {
    if (!open) return;
    untrack(() => {
      email = '';
      name = '';
      roleId = roles[0]?.id ?? '';
      selection = { ...EMPTY_SELECTION, projects: [], apps: [], envs: [] };
      tempPassword = null;
      error = null;
    });
  });

  const canSubmit = $derived(
    !saving && email.trim().includes('@') && roleId !== '' && !isEmptySelection(selection),
  );

  async function submit() {
    if (!canSubmit) return;
    saving = true;
    error = null;
    try {
      const result = await createMember(orgId, {
        email: email.trim(),
        name: name.trim(),
        role_id: roleId,
        scopes: selectionToScopes(selection, orgId, projectOfApp, appOfEnv),
      });
      // Reveal, do not close. This is the only time this value exists.
      tempPassword = result.temp_password;
      oncreated();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      saving = false;
    }
  }
</script>

<Modal
  {open}
  size="lg"
  title={tempPassword ? t('members.created') : t('members.new')}
  dismissible={tempPassword === null}
  onclose={onclose}
>
  {#if tempPassword}
    <p class="lede">
      {t('members.give')} <strong>{email}</strong> {t('prose.members.tempPasswordTail')}
    </p>
    <div class="temp-password">
      <code>{tempPassword}</code>
      <CopyButton value={tempPassword} size="sm" />
    </div>
    <InlineNotice tone="warning">{t('prose.members.oneTimeSecret')}</InlineNotice>
  {:else}
    <div class="fields">
      <Input
        label={t('common.email')}
        type="email"
        bind:value={email}
        placeholder={t('members.placeholder.email')}
        required
      />
      <Input label={t('common.name')} bind:value={name} placeholder={t('members.placeholder.name')} />
      <Select
        label={t('members.column.role')}
        bind:value={roleId}
        options={roles.map((r) => ({ value: r.id, label: r.name }))}
      />
      <div class="gf-field">
        <span class="lbl">{t('members.column.scope')}</span>
        <ScopeTree
          {orgId}
          {orgName}
          {projects}
          {appsByProject}
          {envsByApp}
          {loadingEnvApps}
          {onopenapp}
          value={selection}
          disabled={saving}
          onchange={(next) => (selection = next)}
        />
      </div>
    </div>
    {#if error}<p class="err-msg" role="alert">{error}</p>{/if}
  {/if}

  {#snippet footer()}
    {#if tempPassword}
      <Button variant="primary" onclick={onclose}>{t('common.done')}</Button>
    {:else}
      <Button variant="secondary" onclick={onclose} disabled={saving}>{t('common.cancel')}</Button>
      <Button variant="primary" disabled={!canSubmit} loading={saving} onclick={submit}>
        {t('members.create')}
      </Button>
    {/if}
  {/snippet}
</Modal>

<style>
  .lede {
    font-size: var(--text-sm);
    color: var(--text-muted);
    margin-bottom: 14px;
  }
  .fields {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-bottom: 12px;
  }
  .gf-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .lbl {
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    color: var(--text-muted);
  }
  .temp-password {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    padding: 12px 14px;
    margin-bottom: 14px;
  }
  .temp-password code {
    font-size: var(--text-sm);
    font-weight: var(--weight-strong);
    letter-spacing: 0.02em;
    color: var(--text);
    word-break: break-all;
  }
  .err-msg {
    color: var(--error);
    font-size: var(--text-sm);
    margin-top: 4px;
  }
</style>
