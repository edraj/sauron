<!--
  One row per app: a checkbox that selects the app and a `<select>` that picks
  which environment its numbers come from.

  `ScopeTree.svelte` cannot be reused, and the reason matters. `ScopeSelection`
  is `{ org, projects[], apps[], envs[] }` and `selectionToScopes` COLLAPSES a
  ticked env under a ticked app — that collapse is the whole point of the grant
  model and it is exactly the pairing this feature must preserve. Running this
  selection through `grant-plan.ts`'s coverage-diff machinery would actively
  destroy the per-app environment choice.

  Raw `<input type="checkbox">` inside a `<label class="node">` and a raw
  `<select class="sel">`: `lib/components/ui/` has no Checkbox and no Select
  primitive, and this is the idiom `ScopeTree`/`PermissionPicker` already use.
-->
<script lang="ts">
  import { t } from '../i18n';
  import Spinner from './ui/Spinner.svelte';
  import { sessionStore } from '../stores/session.svelte';
  import type { AppEnvSelection, EnvChoice } from '../models/active-users';
  import type { App, AppEnvironment, SelectionView } from '../models';

  interface Props {
    apps: App[];
    /** Enrollments per app, lazily loaded by the page. */
    envsByApp: Record<string, AppEnvironment[]>;
    loadingEnvApps: Set<string>;
    value: AppEnvSelection;
    /** Keyed by app id; supplies the "2 of 5 environments" label. */
    resolvedByApp: Record<string, SelectionView>;
    onchange: (next: AppEnvSelection) => void;
    onopenapp: (appId: string) => void;
  }

  let { apps, envsByApp, loadingEnvApps, value, resolvedByApp, onchange, onopenapp }: Props =
    $props();

  function toggle(appId: string, checked: boolean) {
    // Records in `$state` are REPLACED, never mutated: a mutation on a
    // deep-proxied object is not what the parent's `$effect` compares against,
    // and the reload silently does not fire.
    const next: AppEnvSelection = { ...value };
    if (checked) {
      next[appId] = 'all';
      onopenapp(appId);
    } else {
      delete next[appId];
    }
    onchange(next);
  }

  function chooseEnv(appId: string, choice: EnvChoice) {
    onchange({ ...value, [appId]: choice });
  }

  /**
   * "Unattributed" is offered only to a caller with app-wide reach, mirroring
   * the backend's `UnattributedNeedsAppReach`: rows attributed to no
   * environment belong to no single environment, so an env-scoped grant can
   * never authorize them. Offering it anyway would produce a 403 on selection.
   *
   * Rendered as a DISABLED option rather than an omitted one, so the choice is
   * discoverable — but `level: 'app'` is what keeps it honest: an env-scoped
   * grant must not satisfy this, which is the whole point of the backend rule
   * it mirrors.
   */
  function canSeeUnattributed(appId: string): boolean {
    return sessionStore.can('event:read', { app: appId, level: 'app' });
  }

  /**
   * What the row says the environment filter is. When the server came back
   * `subset`, the picker's own "All environments" is a LIE — the caller's
   * grants reach only some of the app's environments and the number covers
   * only those.
   */
  function envSummary(appId: string): string | null {
    const view = resolvedByApp[appId];
    if (!view || view.resolved !== 'subset') return null;
    const total = envsByApp[appId]?.length ?? view.environment_ids.length;
    return `${view.environment_ids.length} of ${total} environments`;
  }
</script>

<div class="picker">
  {#each apps as app (app.id)}
    {@const checked = app.id in value}
    <div class="row">
      <label class="node">
        <input
          type="checkbox"
          {checked}
          onchange={(e) => toggle(app.id, (e.currentTarget as HTMLInputElement).checked)}
        />
        <span class="name">{app.name}</span>
      </label>

      <div class="env">
        {#if checked && loadingEnvApps.has(app.id)}
          <Spinner size={14} />
        {:else}
          <select
            class="sel"
            disabled={!checked}
            value={value[app.id] ?? 'all'}
            onchange={(e) => chooseEnv(app.id, (e.currentTarget as HTMLSelectElement).value)}
          >
            <option value="all">{t('ui.env.all')}</option>
            {#each envsByApp[app.id] ?? [] as env (env.id)}
              <option value={env.id}>{env.name}</option>
            {/each}
            {#if canSeeUnattributed(app.id)}
              <option value="none">{t('ui.env.unattributed')}</option>
            {:else}
              <!-- A <select> option cannot host an icon, so the lock is spelled
                   out in the label. -->
              <option value="none" disabled>{t('ui.env.unattributedNeedsAccess')}</option>
            {/if}
          </select>
        {/if}
        {#if checked}
          {@const summary = envSummary(app.id)}
          {#if summary}
            <span class="subset" title={t('ui.env.partialAccess')}>
              {summary}
            </span>
          {/if}
        {/if}
      </div>
    </div>
  {/each}

  {#if apps.length === 0}
    <p class="muted">{t('ui.env.noApps')}</p>
  {/if}
</div>

<style>
  .picker {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }
  .node {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    min-width: 0;
  }
  .name {
    font-weight: 560;
    font-size: 13.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .env {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .sel {
    font: inherit;
    font-size: 12.5px;
    padding: 4px 8px;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .sel:disabled {
    opacity: 0.5;
  }
  .subset {
    font-size: 11.5px;
    color: var(--warning);
    white-space: nowrap;
  }
  .muted {
    color: var(--text-faint);
    font-size: 13px;
  }
</style>
