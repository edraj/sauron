<script lang="ts">
  interface Range {
    days: number;
    label: string;
  }

  interface Props {
    value: number;
    onchange: (days: number) => void;
    ranges?: Range[];
  }

  const DEFAULT: Range[] = [
    { days: 1, label: '24h' },
    { days: 7, label: '7d' },
    { days: 30, label: '30d' },
    { days: 90, label: '90d' },
  ];

  let { value, onchange, ranges = DEFAULT }: Props = $props();
</script>

<div class="ranges" role="tablist">
  {#each ranges as r (r.days)}
    <button
      class="range"
      class:active={value === r.days}
      onclick={() => onchange(r.days)}
      type="button"
      role="tab"
      aria-selected={value === r.days}
    >
      {r.label}
    </button>
  {/each}
</div>

<style>
  .ranges {
    display: inline-flex;
    gap: 4px;
    padding: 4px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .range {
    padding: 6px 13px;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 560;
    border-radius: var(--radius-sm);
  }
  .range:hover {
    color: var(--text);
  }
  .range.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
</style>
