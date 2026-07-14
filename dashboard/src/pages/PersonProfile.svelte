<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import JsonTree from '../lib/components/JsonTree.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { getPerson } from '../lib/api/persons';
  import { errorMessage } from '../lib/api/client';
  import { relativeTime, formatDateTime, initials } from '../lib/utils/format';
  import type { AnalyticsEvent, ErrorEvent, PersonProfile } from '../lib/models';

  interface Props {
    params?: { distinctId?: string };
  }
  let { params }: Props = $props();

  const distinctId = $derived(decodeURIComponent(params?.distinctId ?? ''));

  let profile = $state<PersonProfile | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  type TimelineItem =
    | { kind: 'event'; at: number; data: AnalyticsEvent }
    | { kind: 'error'; at: number; data: ErrorEvent };

  const timeline = $derived.by<TimelineItem[]>(() => {
    if (!profile) return [];
    const items: TimelineItem[] = [];
    for (const e of profile.events) {
      items.push({ kind: 'event', at: new Date(e.occurred_at).getTime(), data: e });
    }
    for (const err of profile.errors) {
      items.push({ kind: 'error', at: new Date(err.occurred_at).getTime(), data: err });
    }
    return items.sort((a, b) => b.at - a.at);
  });

  async function load(appId: string, id: string) {
    loading = true;
    error = null;
    try {
      profile = await getPerson(appId, id, 100);
    } catch (err) {
      error = errorMessage(err);
      profile = null;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const id = distinctId;
    if (aid && id) void load(aid, id);
  });

  function errorTitle(e: ErrorEvent): string {
    const type = e.exception_type ?? 'Error';
    const val = e.exception_value ?? e.message ?? '';
    return val ? `${type}: ${val}` : type;
  }

  // session_id is typed on AnalyticsEvent but may also ride along on error
  // payloads at runtime — read it defensively without widening the shared model.
  function sessionIdOf(data: AnalyticsEvent | ErrorEvent): string | null {
    const sid = (data as { session_id?: string | null }).session_id;
    return sid && sid.length > 0 ? sid : null;
  }

  const sessionCount = $derived.by(() => {
    if (!profile) return 0;
    const ids = new Set<string>();
    for (const e of profile.events) {
      const sid = sessionIdOf(e);
      if (sid) ids.add(sid);
    }
    for (const e of profile.errors) {
      const sid = sessionIdOf(e);
      if (sid) ids.add(sid);
    }
    return ids.size;
  });

  const hasTraits = $derived(
    !!profile?.user?.properties && Object.keys(profile.user.properties).length > 0,
  );
</script>

<AppShell requireApp>
  <button class="back" onclick={() => push('/events')}><Icon name="arrow-left" size={14} /> Back to events</button>

  {#if loading}
    <div class="center"><Spinner size={26} /></div>
  {:else if error}
    <EmptyState title="Couldn't load person" description={error} icon="triangle-alert">
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/events')}>Back</Button>
      {/snippet}
    </EmptyState>
  {:else if profile}
    <header class="identity">
      <span class="avatar">{initials(distinctId)}</span>
      <div class="id-meta">
        <h1 class="id-title mono">{distinctId}</h1>
        <div class="id-sub">
          {#if profile.user}
            <span class="muted">
              First seen {relativeTime(profile.user.first_seen)} · Last seen
              {relativeTime(profile.user.last_seen)}
            </span>
          {:else}
            <span class="muted">Anonymous — no persisted profile record.</span>
          {/if}
        </div>
      </div>
    </header>

    <div class="tiles">
      <StatTiles min={140}>
        <StatTile label="Events" value={profile.events.length.toLocaleString()} />
        <StatTile
          label="Errors"
          value={profile.errors.length.toLocaleString()}
          tone={profile.errors.length > 0 ? 'error' : 'neutral'}
        />
        <StatTile label="Sessions" value={sessionCount > 0 ? sessionCount.toLocaleString() : '—'} />
        <StatTile
          label="First seen"
          value={profile.user ? relativeTime(profile.user.first_seen) : '—'}
          sub={profile.user ? formatDateTime(profile.user.first_seen) : undefined}
        />
        <StatTile
          label="Last seen"
          value={profile.user ? relativeTime(profile.user.last_seen) : '—'}
          sub={profile.user ? formatDateTime(profile.user.last_seen) : undefined}
        />
      </StatTiles>
    </div>

    <div class="grid">
      <div class="col-main">
        <Card title="Activity timeline">
          {#if timeline.length === 0}
            <EmptyState title="No activity" description="This person has no recorded events or errors." icon="inbox" />
          {:else}
            <ol class="timeline">
              {#each timeline as item, i (item.kind + i)}
                {@const sid = sessionIdOf(item.data)}
                <li class="tl-item">
                  <span class="tl-node">
                    <span class="tl-dot {item.kind}"></span>
                    {#if i < timeline.length - 1}<span class="tl-line"></span>{/if}
                  </span>
                  <div class="tl-body">
                    <div class="tl-top">
                      {#if item.kind === 'error'}
                        <LevelBadge level={item.data.level} size="sm" />
                        <button
                          class="tl-title link-title"
                          onclick={() => push(`/issues/${item.data.issue_id}`)}
                        >
                          {errorTitle(item.data)}
                        </button>
                      {:else}
                        <Badge tone="info" size="sm">event</Badge>
                        <span class="tl-title mono">{item.data.name}</span>
                      {/if}
                      {#if sid}
                        <a class="tl-session mono" href={`#/sessions/${encodeURIComponent(sid)}`}>
                          session <Icon name="arrow-up-right" size={14} />
                        </a>
                      {/if}
                      <span class="tl-time" title={formatDateTime(item.data.occurred_at)}>
                        {relativeTime(item.data.occurred_at)}
                      </span>
                    </div>
                    {#if item.kind === 'event' && item.data.properties && Object.keys(item.data.properties).length > 0}
                      <div class="tl-props">
                        {#each Object.entries(item.data.properties) as [k, v] (k)}
                          <span class="prop mono">{k}: {String(v)}</span>
                        {/each}
                      </div>
                    {/if}
                    {#if item.kind === 'error'}
                      <div class="tl-props">
                        {#if item.data.release}<span class="prop mono">release: {item.data.release}</span>{/if}
                      </div>
                    {/if}
                  </div>
                </li>
              {/each}
            </ol>
          {/if}
        </Card>
      </div>

      <aside class="col-side">
        <Card title="Traits">
          {#if hasTraits}
            <JsonTree value={profile.user?.properties} expandTo={2} />
          {:else}
            <p class="muted empty-traits">No traits recorded</p>
          {/if}
        </Card>
        <Card title="Identity">
          <div class="summary">
            <div class="sm-row">
              <span class="muted">Distinct ID</span>
              <span class="sm-val mono small">{distinctId}</span>
            </div>
            {#if profile.user}
              <div class="sm-row">
                <span class="muted">Person ID</span>
                <span class="sm-val mono small">{profile.user.id}</span>
              </div>
            {:else}
              <div class="sm-row">
                <span class="muted">Profile</span>
                <span class="sm-val small">Anonymous</span>
              </div>
            {/if}
          </div>
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
  .identity {
    display: flex;
    align-items: center;
    gap: 16px;
    margin-bottom: 22px;
  }
  .avatar {
    width: 54px;
    height: 54px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--primary-soft);
    color: var(--primary);
    font-size: 18px;
    font-weight: 680;
    flex-shrink: 0;
  }
  .id-title {
    font-size: 21px;
    font-weight: 660;
    word-break: break-all;
  }
  .id-sub {
    margin-top: 4px;
    font-size: 13px;
  }
  .tiles {
    margin-bottom: 18px;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 300px;
    gap: 18px;
    align-items: start;
  }
  .col-main {
    min-width: 0;
  }
  .col-side {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .timeline {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .tl-item {
    display: flex;
    gap: 13px;
  }
  .tl-node {
    position: relative;
    display: flex;
    justify-content: center;
    width: 12px;
    flex-shrink: 0;
    padding-top: 6px;
  }
  .tl-dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    z-index: 1;
    box-shadow: 0 0 0 3px var(--surface);
  }
  .tl-dot.error {
    background: var(--error);
  }
  .tl-dot.event {
    background: var(--info);
  }
  .tl-line {
    position: absolute;
    top: 14px;
    bottom: -8px;
    width: 2px;
    background: var(--border);
  }
  .tl-body {
    padding-bottom: 18px;
    min-width: 0;
    flex: 1;
  }
  .tl-top {
    display: flex;
    align-items: center;
    gap: 9px;
    flex-wrap: wrap;
  }
  .tl-title {
    font-size: 13.5px;
    font-weight: 560;
    color: var(--text);
  }
  .link-title {
    background: none;
    border: none;
    padding: 0;
    text-align: left;
    cursor: pointer;
  }
  .link-title:hover {
    color: var(--primary);
    text-decoration: underline;
  }
  .tl-session {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--text-muted);
    text-decoration: none;
    padding: 1px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    background: var(--surface-2);
    white-space: nowrap;
  }
  .tl-session:hover {
    color: var(--primary);
    border-color: var(--primary-border);
  }
  .tl-time {
    font-size: 11.5px;
    color: var(--text-faint);
    margin-left: auto;
  }
  .empty-traits {
    font-size: 13px;
    padding: 2px 0;
  }
  .tl-props {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 7px;
  }
  .prop {
    font-size: 11px;
    color: var(--text-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    padding: 2px 8px;
    border-radius: var(--radius-pill);
  }
  .summary {
    display: flex;
    flex-direction: column;
    gap: 11px;
  }
  .sm-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    font-size: 13px;
  }
  .sm-val {
    font-weight: 620;
    font-variant-numeric: tabular-nums;
  }
  .sm-val.small {
    font-size: 12px;
    font-weight: 500;
  }

  @media (max-width: 900px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
