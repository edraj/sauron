<script lang="ts">
  import { labelFor, t } from '../../i18n';
  import { errorMessage } from '../../api/client';
  import { LEVEL_KEYS } from '../../models/issue-labels';
  import { untrack } from 'svelte';
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import Select from '../ui/Select.svelte';
  import ScopeTree from '../members/ScopeTree.svelte';
  import { EMPTY_SELECTION, type ScopeSelection } from '../../models/scope-tree';
  import {
    clampConditions,
    kindSupportsEnvFilter,
    selectionToSubscriptionScope,
    validateSubscription,
  } from '../../models/notification-prefs';
  import { createSubscription, updateSubscription } from '../../api/notification-prefs';
  import type {
    NotificationSubscription,
    SubscriptionDelivery,
    SubscriptionKind,
  } from '../../models';
  import { toastStore } from '../../stores/toast.svelte';

  interface Props {
    open: boolean;
    orgId: string;
    orgName: string;
    projects: { id: string; name: string }[];
    appsByProject: Record<string, { id: string; name: string }[]>;
    /** Catalogue environments per project id, loaded on demand by the parent. */
    catalogueEnvsByProject: Record<string, { id: string; name: string }[]>;
    existing: NotificationSubscription | null;
    onopenproject: (projectId: string) => void;
    onsaved: () => void;
    onclose: () => void;
  }

  let {
    open = $bindable(false),
    orgId,
    orgName,
    projects,
    appsByProject,
    catalogueEnvsByProject,
    existing,
    onopenproject,
    onsaved,
    onclose,
  }: Props = $props();

  let kind = $state<SubscriptionKind>('error_spike');
  let selection = $state<ScopeSelection>(EMPTY_SELECTION);
  let environmentIds = $state<string[]>([]);
  let windowSeconds = $state('900');
  let factor = $state('3');
  let minCount = $state('10');
  let level = $state('');
  let delivery = $state<SubscriptionDelivery>('immediate');
  let throttleSeconds = $state('900');
  let quietStart = $state('');
  let quietEnd = $state('');
  let quietTz = $state('UTC');
  let saving = $state(false);
  let error = $state('');

  // Reseeding from props inside `untrack` so a parent reload — which replaces
  // `existing` with an equal-but-not-identical object — cannot wipe a
  // half-finished edit mid-typing.
  $effect(() => {
    const src = existing;
    const isOpen = open;
    untrack(() => {
      if (!isOpen) return;
      if (src) {
        kind = src.kind;
        selection =
          src.scope_type === 'project'
            ? { org: false, projects: [src.scope_id], apps: [], envs: [] }
            : { org: false, projects: [], apps: [src.scope_id], envs: [] };
        environmentIds = [...src.environment_ids];
        windowSeconds = String(src.conditions.window_seconds ?? 900);
        factor = String(src.conditions.factor ?? 3);
        minCount = String(src.conditions.min_count ?? 10);
        level = src.conditions.level ?? '';
        delivery = src.delivery;
        throttleSeconds = String(src.throttle_seconds);
        quietStart = src.quiet_start_min === null ? '' : String(src.quiet_start_min);
        quietEnd = src.quiet_end_min === null ? '' : String(src.quiet_end_min);
        quietTz = src.quiet_tz;
      } else {
        kind = 'error_spike';
        selection = EMPTY_SELECTION;
        environmentIds = [];
        windowSeconds = '900';
        factor = '3';
        minCount = '10';
        level = '';
        delivery = 'immediate';
        throttleSeconds = '900';
        quietStart = '';
        quietEnd = '';
        quietTz = 'UTC';
      }
      error = '';
    });
  });

  // Copied in shape from Alerts.svelte's `triggerNeeds` — which fields a kind
  // actually uses, decided in one place.
  const needs = $derived({
    spike: kind === 'error_spike',
    level: kind !== 'uptime',
    envFilter: kindSupportsEnvFilter(kind),
  });

  const scopeProjectId = $derived.by(() => {
    const s = selectionToSubscriptionScope(selection);
    if (!s.ok) return null;
    if (s.scope_type === 'project') return s.scope_id;
    for (const [pid, apps] of Object.entries(appsByProject)) {
      if (apps.some((a) => a.id === s.scope_id)) return pid;
    }
    return null;
  });

  $effect(() => {
    const pid = scopeProjectId;
    if (pid && !catalogueEnvsByProject[pid]) onopenproject(pid);
  });

  const offeredEnvs = $derived(
    scopeProjectId ? (catalogueEnvsByProject[scopeProjectId] ?? []) : [],
  );

  const draft = $derived({
    kind,
    selection,
    environmentIds,
    conditions: {
      window_seconds: Number(windowSeconds),
      factor: Number(factor),
      min_count: Number(minCount),
      level: level ? level : null,
    },
    delivery,
    throttleSeconds: Number(throttleSeconds),
    quietStartMin: quietStart === '' ? null : Number(quietStart),
    quietEndMin: quietEnd === '' ? null : Number(quietEnd),
    quietTz,
  });

  const problems = $derived(validateSubscription(draft));

  function toggleEnv(id: string) {
    // Replaced, never mutated: `$state` arrays are proxies and an in-place
    // push does not always re-derive downstream.
    environmentIds = environmentIds.includes(id)
      ? environmentIds.filter((e) => e !== id)
      : [...environmentIds, id];
  }

  async function save() {
    const scope = selectionToSubscriptionScope(selection);
    if (!scope.ok) return;
    saving = true;
    error = '';
    try {
      // Exactly the fields PATCH honours. Scope and kind are deliberately
      // absent: the server ignores them on an update, so including them made a
      // re-pointed subscription report success and change nothing.
      const mutable = {
        environment_ids: needs.envFilter ? environmentIds : [],
        conditions: clampConditions(kind, draft.conditions),
        delivery,
        throttle_seconds: Number(throttleSeconds),
        quiet_start_min: draft.quietStartMin,
        quiet_end_min: draft.quietEndMin,
        quiet_tz: quietTz,
      };
      if (existing) {
        await updateSubscription(existing.id, mutable);
      } else {
        await createSubscription({
          scope_type: scope.scope_type,
          scope_id: scope.scope_id,
          kind,
          ...mutable,
        });
      }
      toastStore.success(t('notif.toast.saved'));
      onsaved();
    } catch (e) {
      error = errorMessage(e, t('notif.toast.saveFailed'));
    } finally {
      saving = false;
    }
  }
</script>

<Modal bind:open size="lg" title={existing ? t('notif.edit') : t('notif.new')} {onclose}>
  <div class="form">
    <!--
      Kind and scope are read-only while editing, because PATCH cannot change
      them. Leaving the controls live would let someone re-point a subscription
      at another app, get a green toast, and walk away believing it moved.
    -->
    <Select
      label={t('notif.dialog.notifyMeAbout')}
      value={kind}
      disabled={!!existing}
      options={[
        { value: 'error_spike', label: t('notif.dialog.errorRate') },
        { value: 'error_new_issue', label: t('notif.dialog.newIssue') },
        { value: 'error_regression', label: t('notif.dialog.regression') },
        { value: 'uptime', label: t('notif.dialog.monitor') },
      ]}
      onchange={(v) => (kind = v as SubscriptionKind)}
    />

    <div class="fld">
      <span class="lbl">{t('notif.dialog.scope')}</span>
      <ScopeTree
        {orgId}
        {orgName}
        {projects}
        {appsByProject}
        envsByApp={{}}
        allowOrg={false}
        allowEnv={false}
        value={selection}
        disabled={!!existing}
        onchange={(next) => (selection = next)}
        onopenapp={() => {}}
      />
      {#if existing}
        <p class="hint">
          {t('notif.dialog.immutable')}
        </p>
      {/if}
    </div>

    {#if needs.envFilter}
      <div class="fld">
        <span class="lbl">{t('notif.column.environments')}</span>
        <p class="hint">{t('notif.dialog.allEnvironments')}</p>
        <div class="chips">
          {#each offeredEnvs as env (env.id)}
            <button
              type="button"
              class="chip"
              class:on={environmentIds.includes(env.id)}
              onclick={() => toggleEnv(env.id)}
            >{env.name}</button>
          {/each}
          {#if offeredEnvs.length === 0}
            <span class="hint">{t('notif.dialog.pickScopeFirst')}</span>
          {/if}
        </div>
      </div>
    {:else}
      <p class="hint">
        {t('notif.dialog.monitorsProjectWide')}
      </p>
    {/if}

    {#if needs.spike}
      <div class="row">
        <Input label={t('notif.dialog.window')} bind:value={windowSeconds} hint="300 – 86400" />
        <Input label={t('notif.dialog.increaseFactor')} bind:value={factor} hint="1.5 – 100" />
        <Input label={t('notif.dialog.minErrors')} bind:value={minCount} hint="1 – 100000" />
      </div>
    {/if}

    {#if needs.level}
      <Select
        label={t('notif.dialog.level')}
        bind:value={level}
        options={[
          { value: '', label: t('notif.dialog.anyLevel') },
          ...(['error', 'warning', 'fatal'] as const).map((l) => ({ value: l, label: labelFor(LEVEL_KEYS, l) })),
        ]}
      />
    {/if}

    <div class="row">
      <Select
        label={t('notif.column.delivery')}
        value={delivery}
        options={[
          { value: 'immediate', label: t('notif.dialog.asItHappens') },
          { value: 'hourly', label: t('notif.dialog.hourly') },
          { value: 'daily', label: t('notif.dialog.daily') },
        ]}
        onchange={(v) => (delivery = v as SubscriptionDelivery)}
      />
      <Input label={t('notif.dialog.throttle')} bind:value={throttleSeconds} hint="0 – 604800" />
    </div>

    <div class="row">
      <Input label={t('notif.dialog.quietFrom')} bind:value={quietStart} hint={t('notif.quietFromHint')} />
      <Input label={t('notif.dialog.quietUntil')} bind:value={quietEnd} hint={t('notif.quietUntilHint')} />
      <Input label={t('notif.dialog.timezone')} bind:value={quietTz} hint={t('notif.dialog.timezoneHint')} />
    </div>
    <p class="hint">
      {t('notif.dialog.quietHoursHelp')}
    </p>

    {#if problems.length > 0}
      <ul class="problems">
        {#each problems as p (p)}<li>{p}</li>{/each}
      </ul>
    {/if}
    {#if error}<p class="err" role="alert">{error}</p>{/if}
  </div>

  {#snippet footer()}
    <Button variant="secondary" onclick={onclose}>{t('common.cancel')}</Button>
    <Button variant="primary" disabled={problems.length > 0} loading={saving} onclick={save}>
      {t('common.save')}
    </Button>
  {/snippet}
</Modal>

<style>
  .form { display: flex; flex-direction: column; gap: 16px; }
  .fld { display: flex; flex-direction: column; gap: 6px; }
  .lbl { font-size: var(--text-xs); font-weight: var(--weight-medium); color: var(--text-muted); }
  .row { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 12px; }
  .hint { font-size: var(--text-xs); color: var(--text-faint); margin: 0; }
  .chips { display: flex; flex-wrap: wrap; gap: 6px; }
  .chip {
    padding: 4px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    background: var(--surface);
    color: var(--text-faint);
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .chip.on { border-color: var(--primary); color: var(--text); }
  .problems { margin: 0; padding-inline-start: 18px; font-size: var(--text-xs); color: var(--text-faint); }
  .err { font-size: var(--text-sm); color: var(--error); margin: 0; }
</style>
