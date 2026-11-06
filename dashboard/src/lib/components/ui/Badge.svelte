<script lang="ts">
  import type { Snippet } from 'svelte';

  type Tone = 'neutral' | 'primary' | 'error' | 'warning' | 'success' | 'info' | 'fatal';

  interface Props {
    tone?: Tone;
    dot?: boolean;
    size?: 'sm' | 'md';
    children: Snippet;
  }

  let { tone = 'neutral', dot = false, size = 'md', children }: Props = $props();
</script>

<span class="badge {tone} {size}">
  {#if dot}<span class="dot"></span>{/if}
  {@render children()}
</span>

<style>
  /* A small, flat label: rectangle, soft tone fill, no border. Quiet enough to
     sit in every table row without turning the column into a stripe. */
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    border-radius: var(--radius-sm);
    font-weight: var(--weight-medium);
    line-height: 1;
    white-space: nowrap;
  }
  .badge.md {
    height: 20px;
    padding: 0 7px;
    font-size: var(--text-xs);
  }
  .badge.sm {
    height: 18px;
    padding: 0 6px;
    font-size: var(--text-xs);
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
  }

  .neutral {
    background: var(--surface-2);
    color: var(--text-muted);
  }
  .primary {
    background: var(--primary-soft);
    color: var(--primary);
  }
  .error {
    background: var(--error-soft);
    color: var(--error);
  }
  .fatal {
    background: var(--fatal-soft);
    color: var(--fatal);
  }
  .warning {
    background: var(--warning-soft);
    color: var(--warning);
  }
  .success {
    background: var(--success-soft);
    color: var(--success);
  }
  .info {
    background: var(--info-soft);
    color: var(--info);
  }
</style>
