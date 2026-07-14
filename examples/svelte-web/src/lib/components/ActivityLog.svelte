<script lang="ts">
  import { activity } from '../store.svelte';
</script>

<section class="log">
  <header class="log-head">
    <div class="log-title">
      <h2>Activity log</h2>
      <span class="hint">client-side echo · the SDK batches &amp; sends in the background</span>
    </div>
    <button class="clear" onclick={() => activity.clear()} disabled={activity.entries.length === 0}>
      Clear
    </button>
  </header>

  {#if activity.entries.length === 0}
    <p class="empty">No activity yet — trigger an action above to see it echoed here.</p>
  {:else}
    <ul>
      {#each activity.entries as entry (entry.id)}
        <li class={entry.kind}>
          <span class="time">{entry.time}</span>
          <span class="badge">{entry.kind}</span>
          <div class="body">
            <span class="title">{entry.title}</span>
            {#if entry.detail}<span class="detail">{entry.detail}</span>{/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .log {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-sm);
    overflow: hidden;
  }

  .log-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    padding: 14px 16px;
    border-bottom: 1px solid var(--border);
  }

  .log-title h2 {
    font-size: 13px;
    font-weight: 600;
  }

  .hint {
    display: block;
    color: var(--text-faint);
    font-size: 11.5px;
    margin-top: 2px;
  }

  .clear {
    flex: none;
    padding: 5px 12px;
    font-size: 12px;
    color: var(--text-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .clear:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--border-strong);
  }
  .clear:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .empty {
    padding: 28px 16px;
    text-align: center;
    color: var(--text-faint);
    font-size: 12.5px;
  }

  ul {
    list-style: none;
    padding: 0;
    max-height: 340px;
    overflow-y: auto;
  }

  li {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 9px 16px;
    border-bottom: 1px solid var(--border);
    --accent: var(--neutral);
    --accent-soft: var(--neutral-soft);
  }
  li:last-child {
    border-bottom: none;
  }

  li.error {
    --accent: var(--error);
    --accent-soft: var(--error-soft);
  }
  li.warning {
    --accent: var(--warning);
    --accent-soft: var(--warning-soft);
  }
  li.event {
    --accent: var(--info);
    --accent-soft: var(--info-soft);
  }
  li.identify {
    --accent: var(--success);
    --accent-soft: var(--success-soft);
  }
  li.breadcrumb {
    --accent: var(--primary);
    --accent-soft: var(--primary-soft);
  }
  li.system {
    --accent: var(--neutral);
    --accent-soft: var(--neutral-soft);
  }

  .time {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-faint);
    flex: none;
    padding-top: 1px;
  }

  .badge {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--accent);
    background: var(--accent-soft);
    padding: 2px 6px;
    border-radius: var(--radius-pill);
    flex: none;
    min-width: 74px;
    text-align: center;
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .title {
    font-size: 12.5px;
    color: var(--text);
    font-weight: 500;
  }

  .detail {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-muted);
    word-break: break-word;
  }
</style>
