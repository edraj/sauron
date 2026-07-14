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
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border-radius: var(--radius-pill);
    font-weight: 560;
    line-height: 1;
    white-space: nowrap;
    border: 1px solid transparent;
  }
  .badge.md {
    padding: 4px 10px;
    font-size: 12px;
  }
  .badge.sm {
    padding: 3px 8px;
    font-size: 11px;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
  }

  .neutral {
    background: var(--neutral-soft);
    color: var(--neutral);
    border-color: color-mix(in srgb, var(--neutral) 22%, transparent);
  }
  .primary {
    background: var(--primary-soft);
    color: var(--primary);
    border-color: var(--primary-border);
  }
  .error {
    background: var(--error-soft);
    color: var(--error);
    border-color: color-mix(in srgb, var(--error) 30%, transparent);
  }
  .fatal {
    background: var(--fatal-soft);
    color: var(--fatal);
    border-color: color-mix(in srgb, var(--fatal) 30%, transparent);
  }
  .warning {
    background: var(--warning-soft);
    color: var(--warning);
    border-color: color-mix(in srgb, var(--warning) 30%, transparent);
  }
  .success {
    background: var(--success-soft);
    color: var(--success);
    border-color: color-mix(in srgb, var(--success) 30%, transparent);
  }
  .info {
    background: var(--info-soft);
    color: var(--info);
    border-color: color-mix(in srgb, var(--info) 30%, transparent);
  }
</style>
