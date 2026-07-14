<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import Timeline from '../lib/components/Timeline.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import JsonTree from '../lib/components/JsonTree.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { getSession } from '../lib/api/sessions';
  import { errorMessage, isNormalizedError } from '../lib/api/client';
  import { formatDateTime, formatDuration, durationBetween } from '../lib/utils/format';
  import type { SessionDetail } from '../lib/models';

  interface Props {
    params?: { id?: string };
  }
  let { params }: Props = $props();

  const sessionId = $derived(decodeURIComponent(params?.id ?? ''));

  let detail = $state<SessionDetail | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let notFound = $state(false);

  async function load(appId: string, id: string) {
    loading = true;
    error = null;
    notFound = false;
    try {
      detail = await getSession(appId, id);
    } catch (err) {
      if (isNormalizedError(err) && err.status === 404) {
        notFound = true;
      } else {
        error = errorMessage(err);
      }
      detail = null;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const id = sessionId;
    if (aid && id) void load(aid, id);
  });

  const s = $derived(detail?.session ?? null);
  const durationMs = $derived(s ? durationBetween(s.started_at, s.last_event_at) : 0);
  const hasContext = $derived(
    !!s && !!s.context && typeof s.context === 'object' && Object.keys(s.context).length > 0,
  );
</script>

<AppShell requireApp>
  <button class="back" onclick={() => push('/sessions')}><Icon name="arrow-left" size={14} /> Sessions</button>

  {#if loading}
    <div class="center"><Spinner size={26} /></div>
  {:else if notFound}
    <EmptyState
      title="Session not found"
      description="This session no longer exists, or it never reached this app."
      icon="inbox"
    >
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/sessions')}>Back to sessions</Button>
      {/snippet}
    </EmptyState>
  {:else if error}
    <EmptyState title="Couldn't load session" description={error} icon="triangle-alert">
      {#snippet action()}
        <Button
          variant="secondary"
          onclick={() => sessionStore.currentAppId && load(sessionStore.currentAppId, sessionId)}
        >
          Retry
        </Button>
      {/snippet}
    </EmptyState>
  {:else if detail && s}
    <header class="detail-head">
      <div class="id-row">
        <h1 class="session-id mono">{s.session_id}</h1>
        <CopyButton value={s.session_id} size="sm" />
      </div>
      <div class="meta-row">
        {#if s.distinct_id}
          <a class="meta-link mono" href={`#/persons/${encodeURIComponent(s.distinct_id)}`}>
            <Icon name="user" size={14} />{s.distinct_id}
          </a>
        {:else}
          <span class="meta-static muted"><Icon name="user" size={14} />anonymous</span>
        {/if}
        {#if s.device_key}
          <a class="meta-link mono" href={`#/devices/${encodeURIComponent(s.device_key)}`}>
            <Icon name="monitor" size={14} />{s.device_key}
          </a>
        {/if}
        {#if s.release}<Badge tone="neutral" size="sm">release {s.release}</Badge>{/if}
        {#if s.environment_id}
          <span class="meta-static faint mono">env {s.environment_id}</span>
        {/if}
      </div>
    </header>

    <StatTiles min={160}>
      <StatTile label="Duration" value={formatDuration(durationMs)} />
      <StatTile label="Events" value={s.events_count.toLocaleString()} />
      <StatTile
        label="Errors"
        value={s.errors_count.toLocaleString()}
        tone={s.errors_count > 0 ? 'error' : 'neutral'}
      />
      <StatTile label="Started" value={formatDateTime(s.started_at)} />
    </StatTiles>

    <div class="grid">
      <div class="col-main">
        <Card title="Timeline">
          <Timeline items={detail.timeline} startedAt={s.started_at} />
        </Card>
      </div>
      <aside class="col-side">
        <Card title="Session context">
          {#if hasContext}
            <div class="ctx"><JsonTree value={s.context} expandTo={1} /></div>
          {:else}
            <p class="muted empty-ctx">No context recorded for this session.</p>
          {/if}
        </Card>
      </aside>
    </div>
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
  .detail-head {
    margin-bottom: 20px;
  }
  .id-row {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }
  .session-id {
    font-size: 20px;
    font-weight: 640;
    word-break: break-all;
  }
  .meta-row {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 10px;
    flex-wrap: wrap;
  }
  .meta-link,
  .meta-static {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 4px 10px;
    border-radius: var(--radius-pill);
    border: 1px solid var(--border);
    background: var(--surface-2);
  }
  .meta-link {
    color: var(--text-muted);
    text-decoration: none;
    transition: color 0.12s ease, border-color 0.12s ease;
  }
  .meta-link:hover {
    color: var(--primary);
    border-color: var(--primary-border);
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 340px;
    gap: 18px;
    align-items: start;
    margin-top: 20px;
  }
  .col-main {
    min-width: 0;
  }
  .col-side {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .ctx {
    overflow-x: auto;
  }
  .empty-ctx {
    font-size: 13px;
  }

  @media (max-width: 960px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
