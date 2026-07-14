<script lang="ts">
  import type { TimelineItem } from '../models';
  import { formatTime, formatMs, latencyTone } from '../utils/format';
  import LatencyBadge from './LatencyBadge.svelte';
  import LevelBadge from './LevelBadge.svelte';
  import Badge from './ui/Badge.svelte';
  import Icon, { type IconName } from './ui/Icon.svelte';
  import JsonTree from './JsonTree.svelte';

  interface Props {
    items: TimelineItem[];
    // When set, the session start — renders an elapsed offset per row.
    startedAt?: string | null;
  }

  let { items, startedAt = null }: Props = $props();

  let expanded = $state<Set<number>>(new Set());

  function toggle(i: number) {
    const next = new Set(expanded);
    if (next.has(i)) next.delete(i);
    else next.add(i);
    expanded = next;
  }

  function icon(item: TimelineItem): IconName {
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
    return 'event';
  }

  function title(item: TimelineItem): string {
    switch (item.kind) {
      case 'event':
        return item.event.name;
      case 'error': {
        const e = item.error;
        if (e.exception_type) {
          return e.exception_value ? `${e.exception_type}: ${e.exception_value}` : e.exception_type;
        }
        return e.message ?? 'Error';
      }
      case 'transaction':
        return item.transaction.name;
    }
  }

  function payload(item: TimelineItem): unknown {
    switch (item.kind) {
      case 'event':
        return { properties: item.event.properties, context: item.event.context };
      case 'error':
        return {
          exception: { type: item.error.exception_type, value: item.error.exception_value },
          stacktrace: item.error.stacktrace,
          context: item.error.context,
          tags: item.error.tags,
        };
      case 'transaction':
        return item.transaction;
    }
  }

  function screenOf(item: TimelineItem): string | null {
    if (item.kind === 'event') return item.event.screen ?? null;
    if (item.kind === 'error') return item.error.screen ?? null;
    return null;
  }

  function elapsed(at: string): string {
    if (!startedAt) return '';
    const ms = new Date(at).getTime() - new Date(startedAt).getTime();
    if (ms < 0 || Number.isNaN(ms)) return '';
    return `+${formatMs(ms)}`;
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
          <span class="kind kind-{item.kind}">{item.kind}</span>
          <span class="title truncate">{title(item)}</span>
          <span class="trail">
            {#if item.kind === 'transaction'}
              <Badge tone="neutral" size="sm">{item.transaction.op}</Badge>
              <LatencyBadge ms={item.transaction.duration_ms} size="sm" />
            {:else if item.kind === 'error'}
              <LevelBadge level={item.error.level} size="sm" />
            {/if}
            {#if startedAt}<span class="elapsed faint mono">{elapsed(item.at)}</span>{/if}
            <span class="caret" class:open={expanded.has(i)}><Icon name="chevron-right" size={13} /></span>
          </span>
        </button>
        {#if expanded.has(i)}
          <div class="detail">
            <div class="detail-links">
              {#if item.kind === 'error'}
                <a class="issue-link" href={`#/issues/${item.error.issue_id}`}>View issue <Icon name="arrow-right" size={12} /></a>
              {/if}
              {#if screenOf(item)}
                <a class="screen-link" href={`#/screens/${encodeURIComponent(screenOf(item) ?? '')}`}>
                  <Icon name="layout-panel-top" size={12} />{screenOf(item)}
                </a>
              {/if}
            </div>
            <JsonTree value={payload(item)} expandTo={2} />
          </div>
        {/if}
      </div>
    </li>
  {:else}
    <li class="tl-empty faint">No activity recorded in this session.</li>
  {/each}
</ol>

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
  .kind-event {
    color: var(--primary);
    background: var(--primary-soft);
  }
  .title {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    font-weight: 520;
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
  .issue-link:hover,
  .screen-link:hover {
    text-decoration: underline;
  }
  .tl-empty {
    padding: 24px;
    text-align: center;
    font-size: 13px;
  }
</style>
