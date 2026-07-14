<script lang="ts">
  import type { Snippet } from 'svelte';

  type Tone = 'neutral' | 'primary' | 'success' | 'warning' | 'error' | 'info';

  interface Props {
    label: string;
    value: string | number;
    // Optional secondary line under the value.
    sub?: string;
    // Optional trend delta, e.g. "+12%" — colored by `deltaTone`.
    delta?: string;
    deltaTone?: 'up' | 'down' | 'flat';
    tone?: Tone;
    // Optional inline visual (sparkline etc.).
    visual?: Snippet;
    // Makes the whole tile a link target.
    href?: string;
  }

  let {
    label,
    value,
    sub,
    delta,
    deltaTone = 'flat',
    tone = 'neutral',
    visual,
    href,
  }: Props = $props();
</script>

{#snippet body()}
  <span class="st-label">{label}</span>
  <span class="st-value {tone}">{value}</span>
  <div class="st-foot">
    {#if delta}<span class="st-delta {deltaTone}">{delta}</span>{/if}
    {#if sub}<span class="st-sub">{sub}</span>{/if}
  </div>
  {#if visual}<div class="st-visual">{@render visual()}</div>{/if}
{/snippet}

{#if href}
  <a class="stat-tile interactive" {href}>{@render body()}</a>
{:else}
  <div class="stat-tile">{@render body()}</div>
{/if}

<style>
  .stat-tile {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 14px 16px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    min-width: 0;
    position: relative;
    overflow: hidden;
  }
  .stat-tile.interactive {
    transition: border-color 0.13s ease, background 0.13s ease;
  }
  .stat-tile.interactive:hover {
    border-color: var(--border-strong);
    background: var(--surface-2);
  }
  .st-label {
    font-size: 11.5px;
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--text-muted);
    text-transform: uppercase;
  }
  .st-value {
    font-size: 26px;
    font-weight: 680;
    letter-spacing: -0.02em;
    line-height: 1.15;
    font-variant-numeric: tabular-nums;
  }
  .st-value.primary {
    color: var(--primary);
  }
  .st-value.success {
    color: var(--success);
  }
  .st-value.warning {
    color: var(--warning);
  }
  .st-value.error {
    color: var(--error);
  }
  .st-value.info {
    color: var(--info);
  }
  .st-foot {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 16px;
  }
  .st-delta {
    font-size: 12px;
    font-weight: 620;
  }
  .st-delta.up {
    color: var(--success);
  }
  .st-delta.down {
    color: var(--error);
  }
  .st-delta.flat {
    color: var(--text-faint);
  }
  .st-sub {
    font-size: 12px;
    color: var(--text-faint);
  }
  .st-visual {
    margin-top: 6px;
  }
</style>
