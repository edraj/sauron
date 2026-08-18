<script lang="ts">
  /**
   * The full picture of one error, without leaving the timeline.
   *
   * Every field here is already on the `ErrorEvent` the timeline was served —
   * stacktrace, tags, context, contexts, extra, and the three identifiers that
   * link out. Nothing is refetched to render them, so opening this costs one
   * request and it is for the thing the event genuinely does NOT carry: how
   * often the ISSUE has occurred. `times_seen`/`users_seen` on the issue row
   * would not answer it either — those are all-time and app-wide, and
   * `users_seen` is a HyperLogLog estimate, while these three counts are exact
   * over the same window Issue detail reports.
   *
   * "View more" is the escape hatch to the full issue page: this view is
   * deliberately one occurrence — the one on the timeline — and does not try to
   * be the occurrence list, the assignment controls or the status workflow.
   */
  import { push } from 'svelte-spa-router';
  import type { ErrorEvent, IssueEventStats } from '../models';
  import { sessionStore } from '../stores/session.svelte';
  import { getIssueEventStats } from '../api/issues';
  import { plural } from '../utils/format';
  import Modal from './ui/Modal.svelte';
  import Icon from './ui/Icon.svelte';
  import Badge from './ui/Badge.svelte';
  import LevelBadge from './LevelBadge.svelte';
  import KeyValueList from './KeyValueList.svelte';
  import JsonTree from './JsonTree.svelte';
  import StacktraceView from './StacktraceView.svelte';
  import SymbolicationBadge from './SymbolicationBadge.svelte';

  interface Props {
    /** The occurrence to describe. `null` renders nothing. */
    error: ErrorEvent | null;
    open: boolean;
    onclose: () => void;
  }

  let { error, open, onclose }: Props = $props();

  let stats = $state<IssueEventStats | null>(null);
  /** Kept separate from `stats === null`: absent counts and failed counts are
      different claims, and a dash that means "we could not tell" must not read
      as a zero. */
  let statsFailed = $state(false);

  /**
   * Loads only while the modal is open, and re-loads per issue.
   *
   * Keyed on `issue_id` rather than on the event: two occurrences of the same
   * issue share these counts, and refetching per row would put a request on the
   * wire for an answer already on screen.
   */
  $effect(() => {
    const appId = sessionStore.currentAppId;
    const issueId = error?.issue_id;
    if (!open || !appId || !issueId) return;
    let cancelled = false;
    stats = null;
    statsFailed = false;
    getIssueEventStats(appId, issueId)
      .then((s) => {
        if (!cancelled) stats = s;
      })
      .catch(() => {
        if (!cancelled) statsFailed = true;
      });
    return () => {
      cancelled = true;
    };
  });

  const title = $derived(
    error
      ? error.exception_type
        ? error.exception_value
          ? `${error.exception_type}: ${error.exception_value}`
          : error.exception_type
        : (error.message ?? 'Error')
      : '',
  );

  /** The mail address the SDK attached, when it did — shown under the id. */
  const userEmail = $derived(
    error?.event_user && typeof error.event_user === 'object'
      ? ((error.event_user as { email?: string | null }).email ?? null)
      : null,
  );

  const hasStack = $derived(
    (error?.stacktrace?.length ?? 0) > 0 || error?.debug_meta?.raw_stacktrace != null,
  );
  const hasTags = $derived(!!error?.tags && Object.keys(error.tags).length > 0);
  const hasContext = $derived(!!error?.context && Object.keys(error.context).length > 0);
  const hasContexts = $derived(!!error?.contexts && Object.keys(error.contexts).length > 0);
  const hasExtra = $derived(!!error?.extra && Object.keys(error.extra).length > 0);

  /**
   * Navigate, then close.
   *
   * The order matters on the ONE destination that does not unmount this
   * component's page: a link back into the same session leaves the modal open
   * over the new route otherwise. Closing unconditionally costs nothing on the
   * other three.
   */
  function go(path: string) {
    onclose();
    void push(path);
  }
</script>

<Modal {open} {onclose} size="xl" title="Error details">
  {#if error}
    <div class="qv">
      <header class="qv-head">
        <div class="qv-title-row">
          <LevelBadge level={error.level} size="sm" />
          <h3 class="qv-title">{title}</h3>
        </div>
        <!-- The counts describe the ISSUE, so they sit with the issue link
             rather than beside the occurrence's own timestamp. -->
        <div class="qv-sub">
          {#if stats}
            <span class="occ" title="Across the last 30 days">
              {plural(stats.events, 'event')}
              <span class="sep">·</span>
              {plural(stats.users, 'user')}
              <span class="sep">·</span>
              {plural(stats.sessions, 'session')}
            </span>
          {:else if statsFailed}
            <span class="faint">Occurrence counts unavailable</span>
          {:else}
            <span class="faint">Counting occurrences…</span>
          {/if}
          <button class="more" onclick={() => go(`/issues/${error.issue_id}`)}>
            View more <Icon name="arrow-right" size={13} />
          </button>
        </div>
      </header>

      <!-- The three links out. Rendered as a row of what this occurrence is
           attached to, and each is omitted rather than disabled when the event
           carries no such id: a dead "Device" chip claims a device exists. -->
      {#if error.distinct_id || error.device_key || error.screen}
        <div class="qv-links">
          {#if error.distinct_id}
            <button class="qv-link" onclick={() => go(`/persons/${encodeURIComponent(error.distinct_id ?? '')}`)}>
              <span class="ql-icon"><Icon name="user" size={14} /></span>
              <span class="ql-body">
                <span class="ql-label">Affected user</span>
                <span class="ql-value mono">{error.distinct_id}</span>
                {#if userEmail}<span class="ql-extra">{userEmail}</span>{/if}
              </span>
              <Icon name="arrow-right" size={13} />
            </button>
          {/if}
          {#if error.device_key}
            <button class="qv-link" onclick={() => go(`/devices/${encodeURIComponent(error.device_key ?? '')}`)}>
              <span class="ql-icon"><Icon name="monitor-smartphone" size={14} /></span>
              <span class="ql-body">
                <span class="ql-label">Device</span>
                <span class="ql-value mono">{error.device_key}</span>
              </span>
              <Icon name="arrow-right" size={13} />
            </button>
          {/if}
          {#if error.screen}
            <button class="qv-link" onclick={() => go(`/screens/${encodeURIComponent(error.screen ?? '')}`)}>
              <span class="ql-icon"><Icon name="layout-panel-top" size={14} /></span>
              <span class="ql-body">
                <span class="ql-label">Screen</span>
                <span class="ql-value">{error.screen}</span>
              </span>
              <Icon name="arrow-right" size={13} />
            </button>
          {/if}
        </div>
      {/if}

      {#if hasStack}
        <section class="qv-section">
          <div class="qv-section-head">
            <span class="section-label">Stacktrace</span>
            <SymbolicationBadge
              status={error.symbolication_status}
              isDart={error.debug_meta?.raw_stacktrace != null}
            />
          </div>
          <StacktraceView
            frames={error.stacktrace ?? []}
            symbolicated={error.stacktrace_symbolicated}
            rawTrace={error.debug_meta?.raw_stacktrace}
          />
        </section>
      {/if}

      {#if hasTags}
        <section class="qv-section">
          <span class="section-label">Tags</span>
          <div class="qv-tags">
            {#each Object.entries(error.tags ?? {}) as [k, v] (k)}
              <Badge tone="neutral" size="sm">{k}: {String(v)}</Badge>
            {/each}
          </div>
        </section>
      {/if}

      {#if hasContext}
        <section class="qv-section">
          <span class="section-label">Context</span>
          <KeyValueList data={error.context} emptyLabel="No context" />
        </section>
      {/if}

      {#if hasContexts || hasExtra}
        <div class="qv-row">
          {#if hasContexts}
            <section class="qv-section">
              <span class="section-label">Contexts</span>
              <JsonTree value={error.contexts} name="contexts" expandTo={2} />
            </section>
          {/if}
          {#if hasExtra}
            <section class="qv-section">
              <span class="section-label">Additional data</span>
              <JsonTree value={error.extra} name="extra" expandTo={2} />
            </section>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</Modal>

<style>
  .qv {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .qv-head {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .qv-title-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .qv-title {
    margin: 0;
    font-size: 15px;
    font-weight: 620;
    /* Arbitrary exception text in a fixed-width dialog: wrap rather than push
       the close button off the header. */
    word-break: break-word;
  }
  .qv-sub {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
    font-size: 12.5px;
    color: var(--text-muted);
  }
  .sep {
    opacity: 0.5;
    margin: 0 4px;
  }
  .more {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    background: none;
    border: none;
    padding: 0;
    color: var(--primary);
    font-size: 12.5px;
    font-weight: 560;
  }
  .more:hover {
    text-decoration: underline;
  }
  .qv-links {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 10px;
  }
  .qv-link {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    text-align: left;
    padding: 10px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text);
    transition:
      border-color 0.12s ease,
      background 0.12s ease;
  }
  .qv-link:hover {
    border-color: var(--primary-border);
    background: var(--primary-soft);
  }
  .ql-icon {
    display: grid;
    place-items: center;
    color: var(--text-muted);
  }
  .ql-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    /* Without this a long distinct id blows the grid column out instead of
       ellipsing inside it. */
    min-width: 0;
    flex: 1;
  }
  .ql-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
  }
  .ql-value,
  .ql-extra {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ql-value {
    font-size: 12.5px;
  }
  .ql-extra {
    font-size: 11.5px;
    color: var(--text-muted);
  }
  .qv-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }
  .qv-section-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .section-label {
    font-size: 11px;
    font-weight: 620;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .qv-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .qv-row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 18px;
  }
  @media (max-width: 720px) {
    .qv-row {
      grid-template-columns: 1fr;
    }
  }
  .faint {
    color: var(--text-faint);
  }
</style>
