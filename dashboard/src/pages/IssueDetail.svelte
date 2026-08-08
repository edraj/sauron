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
  import TimeValue from '../lib/components/TimeValue.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import StacktraceView from '../lib/components/StacktraceView.svelte';
  import BreadcrumbTrail from '../lib/components/BreadcrumbTrail.svelte';
  import KeyValueList from '../lib/components/KeyValueList.svelte';
  import JsonTree from '../lib/components/JsonTree.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import FilterBar from '../lib/components/filters/FilterBar.svelte';
  import { OCCURRENCE_FIELDS, encodeFilters, type Filter } from '../lib/components/filters/filters';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { lockedBy } from '../lib/models/page-access';
  import {
    getIssue,
    updateIssueStatus,
    listIssueEvents,
    getIssueEventStats,
  } from '../lib/api/issues';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';
  import {
    relativeTime,
    formatDateTimeSeconds,
    formatDateTimeZone,
  } from '../lib/utils/format';
  import type { IssueDetail, IssueEventStats, IssueStatus, ErrorEvent } from '../lib/models';

  interface Props {
    params?: { id?: string };
  }
  let { params }: Props = $props();

  let issue = $state<IssueDetail | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let updating = $state(false);

  const issueId = $derived(params?.id ?? '');
  // issues.rs:153 uses the STRICT `authorize_app`, so an env-scoped grant that
  // can read this issue still cannot resolve it.
  const writeLock = $derived(
    lockedBy('issue:write', { app: sessionStore.currentAppId, level: 'app' }),
  );

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
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const id = issueId;
    if (aid && id) void load(aid, id);
  });

  let occurrences = $state<ErrorEvent[]>([]);
  let occLoading = $state(false);
  let occFilters = $state<Filter[]>([]);
  let occSearch = $state('');
  let occSince = $state(3650);
  let occStats = $state<IssueEventStats | null>(null);
  let occTimer: ReturnType<typeof setTimeout> | undefined;

  async function loadOccurrences(appId: string, id: string, enc: string[], term: string, since: number) {
    occLoading = true;
    const params = { filters: enc, q: term || undefined, sinceDays: since };
    try {
      // Issued together so the counts and the rows they describe swap in on the
      // same frame; resolving them separately would briefly caption the new
      // rows with the previous filter's totals.
      //
      // `allSettled`, NOT `all`: the counts run `count(DISTINCT …)` over the
      // whole matched range while the list just reads 50 indexed rows, so the
      // stats call is by far the likelier of the two to time out on a large
      // issue. Under `all`, that would reject the pair and blank a perfectly
      // good occurrence table. Losing the stat strip is the acceptable
      // degradation here; losing the rows is not.
      const [rows, stats] = await Promise.allSettled([
        listIssueEvents(appId, id, { ...params, limit: 50 }),
        getIssueEventStats(appId, id, params),
      ]);
      occurrences = rows.status === 'fulfilled' ? rows.value : [];
      occStats = stats.status === 'fulfilled' ? stats.value : null;
    } catch {
      occurrences = [];
      occStats = null;
    } finally {
      occLoading = false;
    }
  }

  function plural(n: number, word: string): string {
    return `${n.toLocaleString()} ${n === 1 ? word : `${word}s`}`;
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const id = issueId;
    const enc = encodeFilters(occFilters);
    const term = occSearch;
    const since = occSince;
    if (!aid || !id) return;
    clearTimeout(occTimer);
    occTimer = setTimeout(() => void loadOccurrences(aid, id, enc, term, since), 250);
    return () => clearTimeout(occTimer);
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

  const eventMeta = $derived.by(() => {
    const ev = latestEvent;
    if (!ev) return '';
    const body = (ev.exception_value ?? ev.message ?? '').trim();
    // `join` rather than a template: a message-only event has no
    // `exception_type`, and an exception with no value has no body. Either
    // interpolated blind leaves a dangling ": ".
    return [latestEventType, body].filter(Boolean).join(': ');
  });

  /**
   * Whether the red subtitle on "Latest event" merely repeats the page <h1>.
   *
   * It usually does: the heading renders `issue.title`, which the pipeline
   * builds as `"{type}: {value}"` with the value truncated to 200 chars
   * (sauron-pipeline `build_title`), and the subtitle is that same pair at full
   * length.
   *
   * Hence prefix rather than equality — that 200-char cap means the two are
   * rarely byte-identical, and a strict `===` would essentially never fire on
   * the long messages where the duplication is most glaring. The length floor
   * keeps a short heading like "Error" from suppressing an "Error: connection
   * refused" subtitle that does carry new information.
   */
  const metaRedundant = $derived.by(() => {
    const title = squash(issue?.title ?? '');
    const meta = squash(eventMeta);
    if (!title || !meta) return false;
    return title === meta || (meta.startsWith(title) && title.length >= MIN_SHARED_PREFIX);
  });

  const MIN_SHARED_PREFIX = 60;

  // Titles round-trip through Postgres and JSON; compare on collapsed
  // whitespace so a stray newline in a message doesn't defeat the match.
  function squash(s: string): string {
    return s.replace(/\s+/g, ' ').trim();
  }

  // Prefer a name a human recognises, falling back to the id the link points at.
  function userLabel(ev: ErrorEvent): string {
    return ev.event_user?.email ?? ev.event_user?.username ?? ev.distinct_id ?? 'anonymous';
  }

  function nested(ctx: Record<string, unknown> | null, group: string, key: string): string | null {
    const g = ctx?.[group];
    if (g == null || typeof g !== 'object') return null;
    const v = (g as Record<string, unknown>)[key];
    return typeof v === 'string' && v !== '' ? v : null;
  }

  // Mirrors how the pipeline derives `device_key` (sauron-pipeline enrich.rs), so
  // the label always describes the device its link resolves to.
  function deviceLabel(ev: ErrorEvent): string | null {
    const c = ev.context;
    const hardware = [nested(c, 'device', 'family'), nested(c, 'device', 'model')]
      .filter(Boolean)
      .join(' ');
    if (hardware) return hardware;
    const os = [nested(c, 'os', 'name'), nested(c, 'os', 'version')].filter(Boolean).join(' ');
    if (os) return os;
    return nested(c, 'runtime', 'name') ?? nested(c, 'ua', 'name');
  }
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
        <div class="actions">
          {#if issue.status !== 'resolved'}
            <Button
              variant="primary"
              loading={updating}
              lockedReason={writeLock}
              onclick={() => setStatus('resolved')}
            >
              Resolve
            </Button>
          {/if}
          {#if issue.status !== 'ignored'}
            <Button
              variant="secondary"
              loading={updating}
              lockedReason={writeLock}
              onclick={() => setStatus('ignored')}
            >
              Ignore
            </Button>
          {/if}
          {#if issue.status !== 'unresolved'}
            <Button
              variant="subtle"
              loading={updating}
              lockedReason={writeLock}
              onclick={() => setStatus('unresolved')}
            >
              Unresolve
            </Button>
          {/if}
        </div>
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
                {#if !metaRedundant}
                  <span class="event-meta mono">{eventMeta}</span>
                {/if}
              </div>
            {/snippet}
            <div class="event-body">
              <div class="section">
                <div class="section-head">
                  <span class="section-label">Stacktrace</span>
                  {#if latestEvent.symbolication_status}
                    {@const s = latestEvent.symbolication_status}
                    {@const isDart = latestEvent.debug_meta?.raw_stacktrace != null}
                    <span
                      class="sym-badge"
                      class:ok={s === 'symbolicated'}
                      class:partial={s === 'partial'}
                      class:none={s === 'no_artifacts'}
                      title={s === 'no_artifacts'
                        ? `Upload ${isDart ? 'debug symbols' : 'source maps'} for this release to see original frames`
                        : ''}
                    >
                      {s === 'symbolicated'
                        ? 'Symbolicated'
                        : s === 'partial'
                          ? 'Partially symbolicated'
                          : s === 'no_artifacts'
                            ? isDart
                              ? 'No symbols'
                              : 'No source maps'
                            : s === 'pending'
                              ? 'Pending'
                              : 'Not applicable'}
                    </span>
                  {/if}
                </div>
                <StacktraceView
                  frames={latestEvent.stacktrace ?? []}
                  symbolicated={latestEvent.stacktrace_symbolicated}
                  rawTrace={latestEvent.debug_meta?.raw_stacktrace}
                />
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

        {#if latestEvent}
          <Card title="Tags">
            <KeyValueList data={latestEvent.tags} emptyLabel="No tags" />
          </Card>

          <div class="data-row">
            <Card title="Contexts">
              {#if latestEvent.contexts && Object.keys(latestEvent.contexts).length > 0}
                <JsonTree value={latestEvent.contexts} name="contexts" expandTo={2} />
              {:else}
                <span class="faint">No contexts</span>
              {/if}
            </Card>

            <Card title="Additional data">
              {#if latestEvent.extra && Object.keys(latestEvent.extra).length > 0}
                <JsonTree value={latestEvent.extra} name="extra" expandTo={2} />
              {:else}
                <span class="faint">No additional data</span>
              {/if}
            </Card>
          </div>
        {/if}

        <Card title="Occurrences">
          {#snippet actions()}
            {#if occStats}
              <span class="occ-stats" title="Across the selected range and filters">
                {plural(occStats.events, 'event')}
                <span class="sep">·</span>
                {plural(occStats.users, 'user')}
                <span class="sep">·</span>
                {plural(occStats.sessions, 'session')}
              </span>
            {/if}
          {/snippet}
          <FilterBar
            fields={OCCURRENCE_FIELDS}
            bind:filters={occFilters}
            bind:search={occSearch}
            bind:sinceDays={occSince}
          />
          {#if occLoading}
            <div class="center"><Spinner size={20} /></div>
          {:else if occurrences.length === 0}
            <p class="faint">No occurrences match this filter.</p>
          {:else}
            <DataTable class="occ-table">
              {#snippet head()}
                <tr>
                  <th>Time</th>
                  <th>User</th>
                  <th>Session</th>
                  <th>Device</th>
                </tr>
              {/snippet}
              {#snippet children()}
                {#each occurrences as ev (ev.id)}
                  <tr>
                    <td title={`${relativeTime(ev.occurred_at)} · ${formatDateTimeZone(ev.occurred_at)}`}>
                      <span class="cell-time">{formatDateTimeSeconds(ev.occurred_at)}</span>
                    </td>
                    <td>
                      {#if ev.distinct_id}
                        <a
                          class="link trunc"
                          href={`#/persons/${encodeURIComponent(ev.distinct_id)}`}
                          title={userLabel(ev)}
                        >
                          {userLabel(ev)}
                        </a>
                      {:else}
                        <span class="faint">anonymous</span>
                      {/if}
                    </td>
                    <td>
                      {#if ev.session_id}
                        <a
                          class="link cell-mono trunc"
                          href={`#/sessions/${encodeURIComponent(ev.session_id)}`}
                          title={ev.session_id}
                        >
                          {ev.session_id}
                        </a>
                      {:else}
                        <span class="faint">—</span>
                      {/if}
                    </td>
                    <td>
                      {#if ev.device_key}
                        <a
                          class="link trunc"
                          href={`#/devices/${encodeURIComponent(ev.device_key)}`}
                          title={deviceLabel(ev) ?? ev.device_key}
                        >
                          {deviceLabel(ev) ?? ev.device_key}
                        </a>
                      {:else if deviceLabel(ev)}
                        <span class="trunc">{deviceLabel(ev)}</span>
                      {:else}
                        <span class="faint">—</span>
                      {/if}
                    </td>
                  </tr>
                {/each}
              {/snippet}
            </DataTable>
          {/if}
        </Card>
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
              <dd><TimeValue value={issue.first_seen} /></dd>
            </div>
            <div>
              <dt>Last seen</dt>
              <dd><TimeValue value={issue.last_seen} /></dd>
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
                <dd><TimeValue value={latestEvent.occurred_at} /></dd>
              </div>
            {/if}
            <div>
              <dt>Fingerprint</dt>
              <dd class="mono fp" title={issue.fingerprint}>{issue.fingerprint.slice(0, 16)}…</dd>
            </div>
          </dl>
        </Card>

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
  /* Contexts + Additional data sit side by side under the full-width Tags card. */
  .data-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 18px;
    align-items: start;
  }
  @media (max-width: 640px) {
    .data-row {
      grid-template-columns: 1fr;
    }
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
  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }
  .sym-badge {
    font-size: 10px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 2px 7px;
    border-radius: var(--radius-pill);
    color: var(--text-muted);
    background: var(--surface-2, var(--surface));
    border: 1px solid var(--border);
  }
  .sym-badge.ok {
    color: var(--success, #30a46c);
    background: color-mix(in srgb, var(--success, #30a46c) 14%, transparent);
    border-color: transparent;
  }
  .sym-badge.partial {
    color: var(--warning, #f5a623);
    background: color-mix(in srgb, var(--warning, #f5a623) 16%, transparent);
    border-color: transparent;
  }
  .sym-badge.none {
    cursor: help;
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

  .faint { color: var(--text-muted); font-size: 12.5px; }

  .occ-stats {
    font-size: 12.5px;
    color: var(--text-muted);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .occ-stats .sep {
    opacity: 0.5;
    margin: 0 2px;
  }
  /* Tabular figures so the stamps form a straight column, and no wrapping —
     a date-time broken across two lines is unreadable at a glance. */
  .cell-time {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  :global(.occ-table) {
    margin-top: 8px;
  }
  /* Ids can be long; keep each identity column bounded so no single cell pushes
     the table into horizontal scroll. */
  .trunc {
    display: inline-block;
    max-width: 260px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: bottom;
  }
  .link {
    color: var(--primary);
    text-decoration: none;
  }
  .link:hover {
    text-decoration: underline;
  }
</style>
