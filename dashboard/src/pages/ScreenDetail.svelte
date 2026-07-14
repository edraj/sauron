<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { getScreenDetail } from '../lib/api/screens';
  import { errorMessage } from '../lib/api/client';
  import { compactNumber, formatDuration, formatDateTime, relativeTime } from '../lib/utils/format';
  import type { ScreenDetail } from '../lib/models';

  interface Props {
    params?: { name?: string };
  }
  let { params }: Props = $props();

  const screenName = $derived(decodeURIComponent(params?.name ?? ''));

  let detail = $state<ScreenDetail | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load(appId: string, name: string) {
    loading = true;
    error = null;
    try {
      detail = await getScreenDetail(appId, name);
    } catch (err) {
      error = errorMessage(err);
      detail = null;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const name = screenName;
    if (aid && name) void load(aid, name);
  });
</script>

<AppShell requireApp>
  <button class="back" onclick={() => push('/screens')}>
    <Icon name="arrow-left" size={14} />
    Screens
  </button>

  {#if loading && !detail}
    <div class="center"><Spinner size={26} /></div>
  {:else if error}
    <EmptyState title="Couldn't load screen" description={error} icon="triangle-alert">
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/screens')}>Back to screens</Button>
      {/snippet}
    </EmptyState>
  {:else if detail}
    <h1 class="page-title mono screen-title">{screenName}</h1>

    <StatTiles min={150}>
      <StatTile label="Views" value={compactNumber(detail.stats.views)} tone="primary" />
      <StatTile label="Users" value={compactNumber(detail.stats.users)} />
      <StatTile label="Events" value={compactNumber(detail.stats.events)} />
      <StatTile
        label="Exceptions"
        value={compactNumber(detail.stats.exceptions)}
        tone={detail.stats.exceptions > 0 ? 'error' : 'neutral'}
      />
      <StatTile label="Avg dwell" value={formatDuration(detail.stats.avg_dwell_ms)} />
      <StatTile label="Total dwell" value={formatDuration(detail.stats.total_dwell_ms)} />
    </StatTiles>

    <div class="lists">
      <Card title="Recent events">
        {#if detail.recent_events.length === 0}
          <p class="muted empty-note">No events on this screen.</p>
        {:else}
          <ul class="rows">
            {#each detail.recent_events as e (e.id)}
              <li>
                <span class="mono truncate">{e.name}</span>
                <span class="faint" title={formatDateTime(e.occurred_at)}>{relativeTime(e.occurred_at)}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </Card>
      <Card title="Recent exceptions">
        {#if detail.recent_exceptions.length === 0}
          <p class="muted empty-note">No exceptions on this screen.</p>
        {:else}
          <ul class="rows">
            {#each detail.recent_exceptions as x (x.id)}
              <li>
                <button class="link mono truncate" onclick={() => push('/issues/' + x.issue_id)}>
                  {x.exception_type ?? x.message}
                </button>
                <span class="faint" title={formatDateTime(x.occurred_at)}>{relativeTime(x.occurred_at)}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </Card>
    </div>
  {:else}
    <EmptyState
      title="Screen not found"
      description="No data for this screen in the selected range."
      icon="layout-panel-top"
    />
  {/if}
</AppShell>

<style>
  .back {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 13px;
    padding: 0;
    margin-bottom: 16px;
  }
  .back:hover {
    color: var(--text);
  }
  .center {
    display: grid;
    place-items: center;
    padding: 80px;
  }
  .screen-title {
    word-break: break-word;
    margin-bottom: 18px;
  }
  .lists {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 18px;
    margin-top: 20px;
    align-items: start;
  }
  .empty-note {
    font-size: 13px;
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .rows li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }
  .truncate {
    display: inline-block;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: middle;
    font-size: 12.5px;
  }
  .link {
    background: none;
    border: none;
    color: var(--primary);
    cursor: pointer;
    padding: 0;
    text-align: left;
  }
  .link:hover {
    text-decoration: underline;
  }
  .faint {
    font-size: 12px;
    color: var(--text-faint);
    white-space: nowrap;
  }

  @media (max-width: 900px) {
    .lists {
      grid-template-columns: 1fr;
    }
  }
</style>
