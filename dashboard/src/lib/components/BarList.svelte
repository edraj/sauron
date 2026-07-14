<script lang="ts">
  interface Item {
    name: string;
    count: number;
  }

  interface Props {
    items: Item[];
    selected?: string | null;
    onselect?: (name: string) => void;
    valueLabel?: string;
  }

  let { items, selected = null, onselect, valueLabel }: Props = $props();

  const max = $derived(items.length ? Math.max(...items.map((i) => i.count), 1) : 1);
</script>

<div class="barlist">
  {#each items as item (item.name)}
    <button
      class="bl-row"
      class:selected={selected === item.name}
      class:interactive={!!onselect}
      onclick={() => onselect?.(item.name)}
      type="button"
      disabled={!onselect}
    >
      <span class="fill" style="width:{(item.count / max) * 100}%"></span>
      <span class="name mono">{item.name}</span>
      <span class="count">{item.count.toLocaleString()}{valueLabel ? ` ${valueLabel}` : ''}</span>
    </button>
  {/each}
</div>

<style>
  .barlist {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .bl-row {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    width: 100%;
    padding: 9px 12px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    text-align: left;
    overflow: hidden;
    transition: border-color 0.12s ease;
  }
  .bl-row.interactive {
    cursor: pointer;
  }
  .bl-row:disabled {
    cursor: default;
  }
  .fill {
    position: absolute;
    inset: 0 auto 0 0;
    background: var(--primary-soft);
    border-radius: var(--radius-sm);
    z-index: 0;
    transition: width 0.3s ease;
  }
  .bl-row.interactive:hover .fill {
    background: color-mix(in srgb, var(--primary) 22%, transparent);
  }
  .bl-row.selected {
    border-color: var(--primary-border);
  }
  .bl-row.selected .fill {
    background: color-mix(in srgb, var(--primary) 26%, transparent);
  }
  .name,
  .count {
    position: relative;
    z-index: 1;
  }
  .name {
    font-size: 12.5px;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-muted);
    flex-shrink: 0;
  }
</style>
