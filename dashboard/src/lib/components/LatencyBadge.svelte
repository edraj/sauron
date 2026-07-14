<script lang="ts">
  import { formatMs, latencyTone } from '../utils/format';

  interface Props {
    ms: number | null | undefined;
    // Thresholds in ms: below `good` is green, below `ok` is amber, else red.
    good?: number;
    ok?: number;
    // Show a leading status dot.
    dot?: boolean;
    size?: 'sm' | 'md';
  }

  let { ms, good = 1000, ok = 3000, dot = true, size = 'md' }: Props = $props();

  const tone = $derived(
    ms === null || ms === undefined || Number.isNaN(ms) ? 'neutral' : latencyTone(ms, good, ok),
  );
</script>

<span class="lat {tone} {size}" class:has-dot={dot}>
  {#if dot}<span class="dot"></span>{/if}
  <span class="mono">{formatMs(ms)}</span>
</span>

<style>
  .lat {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 560;
    line-height: 1;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .lat.md {
    font-size: 12.5px;
  }
  .lat.sm {
    font-size: 11.5px;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
    flex-shrink: 0;
  }
  .success {
    color: var(--success);
  }
  .warning {
    color: var(--warning);
  }
  .error {
    color: var(--error);
  }
  .neutral {
    color: var(--text-faint);
  }
</style>
