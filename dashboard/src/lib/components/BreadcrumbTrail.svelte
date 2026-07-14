<script lang="ts">
  import type { Breadcrumb } from '../models';
  import { formatTime } from '../utils/format';

  interface Props {
    breadcrumbs: Breadcrumb[];
  }

  let { breadcrumbs }: Props = $props();

  function summary(b: Breadcrumb): string {
    if (b.message) return b.message;
    if (b.data && typeof b.data === 'object') {
      const entries = Object.entries(b.data);
      if (entries.length) return entries.map(([k, v]) => `${k}: ${String(v)}`).join(', ');
    }
    return b.category ?? b.type;
  }

  const toneFor: Record<string, string> = {
    error: 'var(--error)',
    fatal: 'var(--fatal)',
    warning: 'var(--warning)',
    info: 'var(--info)',
    debug: 'var(--neutral)',
  };

  function dotColor(b: Breadcrumb): string {
    if (b.level && toneFor[b.level.toLowerCase()]) return toneFor[b.level.toLowerCase()];
    return 'var(--text-faint)';
  }
</script>

{#if breadcrumbs.length === 0}
  <p class="muted empty">No breadcrumbs recorded.</p>
{:else}
  <ol class="trail">
    {#each breadcrumbs as crumb, i (i)}
      <li class="crumb">
        <span class="node">
          <span class="dot" style="background:{dotColor(crumb)}"></span>
          {#if i < breadcrumbs.length - 1}<span class="line"></span>{/if}
        </span>
        <div class="content">
          <div class="top">
            <span class="cat">{crumb.category ?? crumb.type}</span>
            <span class="type-chip">{crumb.type}</span>
            <span class="time mono">{formatTime(crumb.timestamp)}</span>
          </div>
          <div class="summary">{summary(crumb)}</div>
        </div>
      </li>
    {/each}
  </ol>
{/if}

<style>
  .empty {
    padding: 8px 2px;
    font-size: 13px;
  }
  .trail {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
  }
  .crumb {
    display: flex;
    gap: 12px;
  }
  .node {
    position: relative;
    display: flex;
    justify-content: center;
    width: 12px;
    flex-shrink: 0;
    padding-top: 5px;
  }
  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    z-index: 1;
    box-shadow: 0 0 0 3px var(--surface);
  }
  .line {
    position: absolute;
    top: 12px;
    bottom: -8px;
    width: 2px;
    background: var(--border);
  }
  .content {
    padding-bottom: 14px;
    min-width: 0;
    flex: 1;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .cat {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text);
  }
  .type-chip {
    font-size: 10.5px;
    color: var(--text-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    padding: 1px 6px;
    border-radius: var(--radius-pill);
  }
  .time {
    font-size: 11px;
    color: var(--text-faint);
    margin-left: auto;
  }
  .summary {
    font-size: 12.5px;
    color: var(--text-muted);
    margin-top: 2px;
    word-break: break-word;
  }
</style>
