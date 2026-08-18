<script lang="ts">
  import type { ErrorEvent, TimelineItem, Transaction } from '../models';
  import {
    httpStatusTone,
    isHttp,
    isNavigation,
    offsetMs,
    rowCulprit,
    rowKind,
    rowTitle,
    type TimeMode,
  } from '../models/timeline-row';
  import { formatTime, formatMs, latencyTone } from '../utils/format';
  import LatencyBadge from './LatencyBadge.svelte';
  import LevelBadge from './LevelBadge.svelte';
  import Badge from './ui/Badge.svelte';
  import Icon, { type IconName } from './ui/Icon.svelte';
  import JsonTree from './JsonTree.svelte';
  import StacktraceView from './StacktraceView.svelte';
  import SymbolicationBadge from './SymbolicationBadge.svelte';
  import IssueQuickView from './IssueQuickView.svelte';

  interface Props {
    items: TimelineItem[];
    // When set, the session start — renders an elapsed offset per row.
    startedAt?: string | null;
    // What that offset reads against: the session start, or the row above.
    // The control that flips it lives with the caller (the Card header).
    timeMode?: TimeMode;
    onslice?: (item: Transaction) => void;
    /**
     * What to say when `items` is empty.
     *
     * The default speaks for an unfiltered timeline. A caller that narrows the
     * list MUST override it: "No activity recorded in this session" is a claim
     * about the session, and repeating it over a filter that hid every row
     * states something false about the data rather than about the filter.
     */
    emptyLabel?: string;
  }

  /**
   * The occurrence the quick view is describing, or `null` when it is closed.
   *
   * Held here rather than by each caller because the row that opens it lives
   * here: a callback prop would make every page that renders a timeline
   * re-implement the same modal, and a page that forgot would show a "View
   * issue" link that navigated away mid-session instead.
   */
  let quickView = $state<ErrorEvent | null>(null);

  let {
    items,
    startedAt = null,
    timeMode = 'session',
    onslice,
    emptyLabel = 'No activity recorded in this session.',
  }: Props = $props();

  let expanded = $state<Set<number>>(new Set());

  function toggle(i: number) {
    const next = new Set(expanded);
    if (next.has(i)) next.delete(i);
    else next.add(i);
    expanded = next;
  }

  function icon(item: TimelineItem): IconName {
    if (isNavigation(item)) return 'compass';
    switch (item.kind) {
      case 'event':
        return 'diamond';
      case 'error':
        return 'x';
      case 'transaction':
        return 'zap';
    }
  }

  function tone(item: TimelineItem): string {
    if (item.kind === 'error') {
      const l = String(item.error.level).toLowerCase();
      return l === 'fatal' ? 'fatal' : l === 'warning' ? 'warning' : 'error';
    }
    if (item.kind === 'transaction') return latencyTone(item.transaction.duration_ms);
    // Its own tone rather than the existing `warning` (which happens to carry
    // the same values): the rail node and the row's badge are one signal, and a
    // navigation node left on the event indigo would read as an ordinary event
    // wearing an odd glyph.
    return isNavigation(item) ? 'navigation' : 'event';
  }

  /**
   * The JSON half of an expanded row.
   *
   * An error's `stacktrace` is deliberately absent: frames render through
   * `StacktraceView` above this tree — symbolicated, collapsible, with source
   * context — and repeating them here as raw JSON would show every frame
   * twice, the second time in the minified form the symbolication exists to
   * replace.
   *
   * `context` is absent for the same reason. The ingest pipeline enriches one
   * context per envelope and writes that same value to both the event row and
   * the session row, so on this page it is already on screen once, in the
   * "Session context" card beside the timeline — and it would otherwise repeat
   * there under every expanded row. What stays is the part that varies per
   * row: an event's `properties`, an error's exception and `tags`.
   */
  function payload(item: TimelineItem): unknown {
    switch (item.kind) {
      case 'event':
        return { properties: item.event.properties };
      case 'error':
        return {
          // Keyed by what each field IS. These read `title`/`culprit` with
          // `exception_type`/`exception_value` as the fallback, back when the
          // API served neither and the fallback was the only branch that ever
          // ran. It now serves both, and `title` is `"{type}: {value}"` while
          // `culprit` is a frame — so the old mapping would have labelled the
          // whole headline "type" and the crash site "value".
          exception: {
            type: item.error.exception_type,
            value: item.error.exception_value,
            culprit: item.error.culprit,
          },
          tags: item.error.tags,
        };
      case 'transaction':
        return item.transaction;
    }
  }

  /**
   * Whether an error carries anything a `StacktraceView` could render. A
   * message-only error (a `captureMessage`, or a crash whose SDK sent no
   * frames) has none of the three, and gets no Stacktrace heading rather than
   * a heading over the words "No stacktrace on this event."
   */
  function hasStack(e: ErrorEvent): boolean {
    return (
      (e.stacktrace?.length ?? 0) > 0 ||
      (e.stacktrace_symbolicated?.length ?? 0) > 0 ||
      e.debug_meta?.raw_stacktrace != null
    );
  }

  function screenOf(item: TimelineItem): string | null {
    if (item.kind === 'event') return item.event.screen ?? null;
    if (item.kind === 'error') return item.error.screen ?? null;
    return null;
  }

  /**
   * The trailing offset label. An em dash — not a `+` with nothing after it —
   * when there is no reference point, which in `delta` mode is the first row.
   */
  function offsetLabel(i: number): string {
    const ms = offsetMs(items, i, startedAt, timeMode);
    return ms === null ? '—' : `+${formatMs(ms)}`;
  }
</script>

<ol class="tl">
  {#each items as item, i (i)}
    <li class="tl-item">
      <div class="rail">
        <span class="node {tone(item)}"><Icon name={icon(item)} size={12} /></span>
      </div>
      <div class="content">
        <button class="row" onclick={() => toggle(i)} type="button">
          <span class="time mono" title={formatTime(item.at)}>{formatTime(item.at)}</span>
          <span class="kind kind-{rowKind(item)}">{rowKind(item)}</span>
          {#if item.kind === 'transaction'}
            {#if !isHttp(item) && item.transaction.op}
              <Badge tone="neutral" size="sm">{item.transaction.op}</Badge>
            {/if}
          {/if}
          <span class="title truncate">{rowTitle(item)}</span>
          <!--
            WHERE it happened, beside WHAT happened. On an obfuscated build the
            headline is the SDK's `runtimeType` and is unreadable on its own;
            this is the de-obfuscated frame, and without it the only way to
            learn the class was to expand the row and read the stacktrace.
          -->
          {#if rowCulprit(item)}
            <span class="culprit mono truncate">{rowCulprit(item)}</span>
          {/if}
          <span class="trail">
            {#if item.kind === 'transaction'}
              {#if isHttp(item)}
                <!-- The op badge would read "http" beside a badge already
                     reading HTTP; the response code is the fact that isn't
                     anywhere else on the collapsed row. -->
                {#if item.transaction.http_status != null}
                  <Badge tone={httpStatusTone(item.transaction.http_status)} size="sm">
                    {item.transaction.http_status}
                  </Badge>
                {/if}
              {/if}
              <LatencyBadge ms={item.transaction.duration_ms} size="sm" />
              {#if onslice && item.transaction.finished_at}
                <!-- svelte-ignore a11y-click-events-have-key-events -->
                <!-- svelte-ignore a11y-no-static-element-interactions -->
                <span
                  class="in-between-btn"
                  onclick={(e) => {
                    e.stopPropagation();
                    onslice(item.transaction);
                  }}
                  title="Slice timeline to this transaction"
                >
                  In between
                </span>
              {/if}
            {:else if item.kind === 'error'}
              <LevelBadge level={item.error.level} size="sm" />
            {/if}
            {#if startedAt || timeMode === 'delta'}
              <span
                class="elapsed faint mono"
                title={timeMode === 'delta' ? 'Since the previous entry' : 'Since the session started'}
              >{offsetLabel(i)}</span>
            {/if}
            <span class="caret" class:open={expanded.has(i)}><Icon name="chevron-right" size={13} /></span>
          </span>
        </button>
        {#if expanded.has(i)}
          <div class="detail">
            <div class="detail-links">
              {#if item.kind === 'error'}
                <!-- Opens in place rather than navigating. Reading a crash used
                     to cost the session: the link left for Issue detail, which
                     describes the ISSUE across every occurrence, and coming
                     back re-entered the timeline at the top with the row
                     collapsed again. The quick view answers the question the
                     reader actually has here — what happened in THIS session —
                     and keeps "View more" for the full page. -->
                <button
                  type="button"
                  class="issue-link"
                  onclick={() => (quickView = item.error)}
                >
                  View issue <Icon name="arrow-right" size={12} />
                </button>
              {/if}
              {#if screenOf(item)}
                <a class="screen-link" href={`#/screens/${encodeURIComponent(screenOf(item) ?? '')}`}>
                  <Icon name="layout-panel-top" size={12} />{screenOf(item)}
                </a>
              {/if}
            </div>
            {#if item.kind === 'error' && hasStack(item.error)}
              <div class="tl-stack">
                <div class="tl-stack-head">
                  <span class="section-label">Stacktrace</span>
                  <SymbolicationBadge
                    status={item.error.symbolication_status}
                    isDart={item.error.debug_meta?.raw_stacktrace != null}
                  />
                </div>
                <StacktraceView
                  frames={item.error.stacktrace ?? []}
                  symbolicated={item.error.stacktrace_symbolicated}
                  rawTrace={item.error.debug_meta?.raw_stacktrace}
                />
              </div>
            {/if}
            <!--
              Developer-supplied metadata, surfaced ABOVE the raw payload.

              It is already inside `payload(item)` — that returns the whole
              transaction — but buried among twenty machine columns, which is
              where it might as well not be. These are the two fields the
              developer chose to attach, and on an HTTP span they are the
              request and response bodies; they get their own labelled blocks
              for the same reason Issue detail gives `extra` a card of its own.
            -->
            {#if item.kind === 'transaction'}
              {#if item.transaction.extra?._truncated === true}
                <p class="tl-truncated" role="status">
                  <Icon name="info" size={13} />
                  <span>
                    The SDK capped this payload at 16 KB and sent a marker instead. The
                    span and its timing are accurate; only the attached data was dropped.
                  </span>
                </p>
              {/if}
              {#if item.transaction.tags && Object.keys(item.transaction.tags).length > 0}
                <div class="tl-meta">
                  <span class="section-label">Tags</span>
                  <div class="tl-tags">
                    {#each Object.entries(item.transaction.tags) as [k, v] (k)}
                      <Badge tone="neutral" size="sm">{k}: {v}</Badge>
                    {/each}
                  </div>
                </div>
              {/if}
              {#if item.transaction.extra && Object.keys(item.transaction.extra).length > 0}
                <div class="tl-meta">
                  <span class="section-label">Additional data</span>
                  <JsonTree value={item.transaction.extra} expandTo={1} />
                </div>
              {/if}
            {/if}
            <JsonTree value={payload(item)} expandTo={2} />
          </div>
        {/if}
      </div>
    </li>
  {:else}
    <li class="tl-empty faint">{emptyLabel}</li>
  {/each}
</ol>

<IssueQuickView error={quickView} open={quickView !== null} onclose={() => (quickView = null)} />

<style>
  .tl {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .tl-item {
    display: grid;
    grid-template-columns: 28px 1fr;
    gap: 10px;
  }
  .rail {
    display: flex;
    flex-direction: column;
    align-items: center;
    position: relative;
  }
  .rail::before {
    content: '';
    position: absolute;
    top: 22px;
    bottom: -6px;
    width: 1px;
    background: var(--border);
  }
  .tl-item:last-child .rail::before {
    display: none;
  }
  .node {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    font-size: 10px;
    z-index: 1;
    border: 1px solid transparent;
    background: var(--surface-3);
  }
  .node.event {
    color: var(--primary);
    background: var(--primary-soft);
    border-color: var(--primary-border);
  }
  .node.success {
    color: var(--success);
    background: var(--success-soft);
  }
  .node.warning {
    color: var(--warning);
    background: var(--warning-soft);
  }
  .node.navigation {
    color: var(--warning);
    background: var(--warning-soft);
  }
  .node.error {
    color: var(--error);
    background: var(--error-soft);
  }
  .node.fatal {
    color: var(--fatal);
    background: var(--fatal-soft);
  }
  .content {
    min-width: 0;
    padding-bottom: 8px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 6px 10px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    text-align: left;
    transition: border-color 0.12s ease;
  }
  .row:hover {
    border-color: var(--border-strong);
  }
  .time {
    font-size: 11.5px;
    color: var(--text-faint);
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }
  .kind {
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    flex-shrink: 0;
    background: var(--surface-3);
    color: var(--text-muted);
  }
  .kind-error {
    color: var(--error);
    background: var(--error-soft);
  }
  .kind-transaction {
    color: var(--info);
    background: var(--info-soft);
  }
  /* Shares the transaction tone on purpose: an HTTP row IS a transaction, just
     a named one, and the response-code badge beside it already carries the
     colour that varies per row. */
  .kind-http {
    color: var(--info);
    background: var(--info-soft);
  }
  .kind-event {
    color: var(--primary);
    background: var(--primary-soft);
  }
  /* Its own tone: --primary is taken by events and --info by transactions, and
     a navigation row that borrows either reads as one of them at a glance. */
  .kind-navigation {
    color: var(--warning);
    background: var(--warning-soft);
  }
  .title {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    font-weight: 520;
  }
  /* `flex: 0 1 auto` against the title's `flex: 1`: on a narrow row the crash
     site gives up width first. It is the supporting half of the pair, and a
     truncated headline beside an intact file path reads as the wrong emphasis.
     `max-width` keeps a deep package path from taking half the row on a wide
     one. */
  .culprit {
    flex: 0 1 auto;
    min-width: 0;
    max-width: 40%;
    font-size: 11px;
    color: var(--text-faint);
  }
  .trail {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }
  .elapsed {
    font-size: 11px;
  }
  .caret {
    display: inline-flex;
    align-items: center;
    color: var(--text-faint);
    transition: transform 0.12s ease;
  }
  .caret.open {
    transform: rotate(90deg);
  }
  .detail {
    margin-top: 6px;
    padding: 12px 14px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow-x: auto;
  }
  /* `tl-` prefixed for the same reason `.tl-stack` below is. */
  .tl-meta {
    margin-bottom: 12px;
  }
  .tl-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 6px;
  }
  .tl-truncated {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 12px;
    padding: 7px 10px;
    border-radius: var(--radius);
    background: var(--info-soft);
    color: var(--info);
    font-size: 12px;
  }
  /* `tl-`prefixed because `.stack` is a global utility in app.css. */
  .tl-stack {
    margin-bottom: 12px;
  }
  .tl-stack-head {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 6px;
  }
  .detail-links {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-bottom: 8px;
    flex-wrap: wrap;
  }
  .issue-link,
  .screen-link {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 12px;
    font-weight: 560;
    color: var(--primary);
  }
  /* `.issue-link` is a <button> since it opens the quick view instead of
     navigating; these three reset the UA button styling so it still reads as
     the link it sits beside. */
  .issue-link {
    background: none;
    border: none;
    padding: 0;
  }
  .issue-link:hover,
  .screen-link:hover {
    text-decoration: underline;
  }
  .tl-empty {
    padding: 24px;
    text-align: center;
    font-size: 13px;
  }
  .in-between-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--text-muted);
    font-size: 10px;
    font-weight: 560;
    text-transform: uppercase;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    cursor: pointer;
    transition: background 0.12s, color 0.12s;
  }
  .in-between-btn:hover {
    background: var(--surface-3);
    color: var(--text);
  }
</style>
