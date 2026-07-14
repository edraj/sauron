<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import LevelBadge from '../lib/components/LevelBadge.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import StacktraceView from '../lib/components/StacktraceView.svelte';
  import BreadcrumbTrail from '../lib/components/BreadcrumbTrail.svelte';
  import KeyValueList from '../lib/components/KeyValueList.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { getIssue, updateIssueStatus } from '../lib/api/issues';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { relativeTime, formatDateTime } from '../lib/utils/format';
  import type { IssueDetail, IssueStatus } from '../lib/models';

  interface Props {
    params?: { id?: string };
  }
  let { params }: Props = $props();

  let issue = $state<IssueDetail | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let updating = $state(false);

  const issueId = $derived(params?.id ?? '');
  const canWrite = $derived(sessionStore.can('issue:write', { app: sessionStore.currentAppId }));

  async function load(appId: string, id: string) {
    loading = true;
    error = null;
    try {
      issue = await getIssue(appId, id);
    } catch (err) {
      error = errorMessage(err);
      issue = null;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const id = issueId;
    if (aid && id) void load(aid, id);
  });

  async function setStatus(next: IssueStatus) {
    const aid = sessionStore.currentAppId;
    const current = issue;
    if (!current || !aid || updating || current.status === next) return;
    const previous = current.status;
    // Optimistic — mutate the reactive $state object in place.
    current.status = next;
    updating = true;
    try {
      const updated = await updateIssueStatus(aid, current.id, next);
      current.status = updated.status;
      current.updated_at = updated.updated_at;
      toastStore.success(`Issue marked ${next}.`);
    } catch (err) {
      current.status = previous;
      toastStore.error(errorMessage(err));
    } finally {
      updating = false;
    }
  }

  const distinctId = $derived(issue?.latest_event?.distinct_id ?? null);
  const eventUserEmail = $derived(
    issue?.latest_event?.event_user?.email ??
      (issue?.latest_event?.context?.user as { email?: string } | undefined)?.email ??
      null,
  );
  const latestEvent = $derived(issue?.latest_event ?? null);
  const latestEventType = $derived(latestEvent?.exception_type ?? issue?.type ?? '');
</script>

<AppShell requireApp>
  <button class="back" onclick={() => push('/issues')}>
    <Icon name="arrow-left" size={14} />
    Back to issues
  </button>

  {#if loading}
    <div class="center"><Spinner size={26} /></div>
  {:else if error}
    <EmptyState title="Couldn't load issue" description={error} icon="triangle-alert">
      {#snippet action()}
        <Button variant="secondary" onclick={() => push('/issues')}>Back to issues</Button>
      {/snippet}
    </EmptyState>
  {:else if issue}
    <header class="detail-head">
      <div class="head-main">
        <div class="badges">
          <span class="type-tag mono">{issue.type}</span>
        </div>
        <h1 class="issue-title">{issue.title}</h1>
        {#if issue.culprit}<p class="culprit mono">{issue.culprit}</p>{/if}
      </div>
      {#if canWrite}
        <div class="actions">
          {#if issue.status !== 'resolved'}
            <Button variant="primary" loading={updating} onclick={() => setStatus('resolved')}>
              Resolve
            </Button>
          {/if}
          {#if issue.status !== 'ignored'}
            <Button variant="secondary" loading={updating} onclick={() => setStatus('ignored')}>
              Ignore
            </Button>
          {/if}
          {#if issue.status !== 'unresolved'}
            <Button variant="subtle" loading={updating} onclick={() => setStatus('unresolved')}>
              Unresolve
            </Button>
          {/if}
        </div>
      {/if}
    </header>

    <div class="issue-body">
      <div class="col-main">
        <Card title="Events over time">
          <TimeSeriesChart data={issue.series} height={170} color="var(--error)" />
        </Card>

        {#if latestEvent}
          <Card>
            {#snippet header()}
              <div class="event-head">
                <h3 class="card-title-inline">Latest event</h3>
                <span class="event-meta mono">
                  {latestEventType}: {latestEvent.exception_value ?? latestEvent.message ?? ''}
                </span>
              </div>
            {/snippet}
            <div class="event-body">
              <div class="section">
                <span class="section-label">Stacktrace</span>
                <StacktraceView frames={latestEvent.stacktrace ?? []} />
              </div>
              <div class="section">
                <span class="section-label">Breadcrumbs</span>
                <BreadcrumbTrail breadcrumbs={latestEvent.breadcrumbs ?? []} />
              </div>
              <div class="section">
                <span class="section-label">Context</span>
                <KeyValueList data={latestEvent.context} emptyLabel="No context" />
              </div>
            </div>
          </Card>
        {:else}
          <Card title="Latest event">
            <p class="muted">No event payload available for this issue.</p>
          </Card>
        {/if}
      </div>

      <aside class="rail">
        <Card title="Overview">
          <dl class="side-dl">
            <div>
              <dt>Status</dt>
              <dd><StatusBadge status={issue.status} /></dd>
            </div>
            <div>
              <dt>Level</dt>
              <dd><LevelBadge level={issue.level} /></dd>
            </div>
            <div><dt>Events</dt><dd>{issue.times_seen.toLocaleString()}</dd></div>
            <div><dt>Users affected</dt><dd>{issue.users_seen.toLocaleString()}</dd></div>
            <div>
              <dt>First seen</dt>
              <dd title={formatDateTime(issue.first_seen)}>{relativeTime(issue.first_seen)}</dd>
            </div>
            <div>
              <dt>Last seen</dt>
              <dd title={formatDateTime(issue.last_seen)}>{relativeTime(issue.last_seen)}</dd>
            </div>
            <div><dt>Type</dt><dd class="mono">{issue.type}</dd></div>
            {#if latestEvent?.release}
              <div><dt>Release</dt><dd class="mono">{latestEvent.release}</dd></div>
            {/if}
            {#if latestEvent?.screen}
              <div>
                <dt>Screen</dt>
                <dd>
                  <a class="screen-link mono" href={`#/screens/${encodeURIComponent(latestEvent.screen)}`}>
                    <Icon name="layout-panel-top" size={13} />{latestEvent.screen}
                  </a>
                </dd>
              </div>
            {/if}
            {#if latestEvent}
              <div>
                <dt>Occurred</dt>
                <dd title={formatDateTime(latestEvent.occurred_at)}>
                  {relativeTime(latestEvent.occurred_at)}
                </dd>
              </div>
            {/if}
            <div>
              <dt>Fingerprint</dt>
              <dd class="mono fp" title={issue.fingerprint}>{issue.fingerprint.slice(0, 16)}…</dd>
            </div>
          </dl>
        </Card>

        {#if latestEvent}
          <Card title="Tags">
            <KeyValueList data={latestEvent.tags} emptyLabel="No tags" />
          </Card>
        {/if}

        {#if distinctId}
          <Card title="Affected user">
            <button class="person" onclick={() => push(`/persons/${encodeURIComponent(distinctId)}`)}>
              <span class="p-avatar">{(eventUserEmail ?? distinctId).slice(0, 1).toUpperCase()}</span>
              <span class="p-meta">
                <span class="p-id mono">{distinctId}</span>
                {#if eventUserEmail}<span class="p-email">{eventUserEmail}</span>{/if}
              </span>
              <span class="p-arrow"><Icon name="arrow-right" size={14} /></span>
            </button>
          </Card>
        {/if}
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
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 20px;
    margin-bottom: 20px;
    flex-wrap: wrap;
  }
  .badges {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 10px;
    flex-wrap: wrap;
  }
  .type-tag {
    font-size: 12px;
    color: var(--text-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    padding: 3px 9px;
    border-radius: var(--radius-pill);
  }
  .issue-title {
    font-size: 22px;
    font-weight: 660;
    line-height: 1.3;
    word-break: break-word;
  }
  .culprit {
    color: var(--text-muted);
    font-size: 13px;
    margin-top: 6px;
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
  }
  .issue-body {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    gap: 22px;
    align-items: start;
  }
  .col-main {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-width: 0;
  }
  .rail {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .event-head {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .card-title-inline {
    font-size: 14.5px;
    font-weight: 620;
  }
  .event-meta {
    font-size: 12px;
    color: var(--error);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
  .event-body {
    display: flex;
    flex-direction: column;
    gap: 22px;
  }
  .section {
    display: flex;
    flex-direction: column;
    gap: 9px;
  }
  .side-dl {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 0;
  }
  .side-dl > div {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  .side-dl dt {
    font-size: 12px;
    color: var(--text-faint);
  }
  .side-dl dd {
    margin: 0;
    font-size: 12.5px;
    color: var(--text);
    text-align: right;
    word-break: break-word;
  }
  .fp {
    font-size: 11.5px;
  }
  .screen-link {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--primary);
    font-size: 12px;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .screen-link:hover {
    text-decoration: underline;
  }
  .person {
    display: flex;
    align-items: center;
    gap: 11px;
    width: 100%;
    padding: 4px 2px;
    background: none;
    border: none;
    text-align: left;
  }
  .person:hover .p-arrow {
    transform: translateX(3px);
    color: var(--primary);
  }
  .p-avatar {
    width: 34px;
    height: 34px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--primary-soft);
    color: var(--primary);
    font-weight: 650;
    flex-shrink: 0;
  }
  .p-meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .p-id {
    font-size: 12.5px;
    font-weight: 560;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .p-email {
    font-size: 11.5px;
    color: var(--text-faint);
  }
  .p-arrow {
    color: var(--text-faint);
    transition: transform 0.14s ease, color 0.14s ease;
  }

  @media (max-width: 900px) {
    .issue-body {
      grid-template-columns: 1fr;
    }
  }
</style>
