<script lang="ts">
  import { untrack } from 'svelte';
  import Self from './JsonTree.svelte';
  import Icon from './ui/Icon.svelte';

  interface Props {
    value: unknown;
    name?: string | null;
    depth?: number;
    // Auto-expand nodes shallower than this depth.
    expandTo?: number;
  }

  let { value, name = null, depth = 0, expandTo = 1 }: Props = $props();

  const isArray = $derived(Array.isArray(value));
  const isObject = $derived(
    value !== null && typeof value === 'object' && !Array.isArray(value),
  );
  const branch = $derived(isArray || isObject);

  const entries = $derived(
    isArray
      ? (value as unknown[]).map((v, i) => [String(i), v] as const)
      : isObject
        ? Object.entries(value as Record<string, unknown>)
        : [],
  );

  // One-time initial expansion from the depth/expandTo props; `open` is then
  // toggled by the user, so we deliberately snapshot rather than track.
  let open = $state(untrack(() => depth < expandTo));

  function preview(): string {
    if (isArray) return `[${(value as unknown[]).length}]`;
    if (isObject) return `{${Object.keys(value as object).length}}`;
    return '';
  }

  function leafClass(v: unknown): string {
    if (v === null) return 'j-null';
    switch (typeof v) {
      case 'number':
        return 'j-num';
      case 'boolean':
        return 'j-bool';
      case 'string':
        return 'j-str';
      default:
        return 'j-str';
    }
  }

  function leafText(v: unknown): string {
    if (v === null) return 'null';
    if (typeof v === 'string') return `"${v}"`;
    return String(v);
  }
</script>

<div class="jt" style="--depth:{depth}">
  {#if branch}
    <button class="j-row j-branch" onclick={() => (open = !open)} type="button">
      <span class="j-caret" class:open><Icon name="chevron-right" size={11} /></span>
      {#if name !== null}<span class="j-key">{name}</span><span class="j-colon">:</span>{/if}
      <span class="j-preview">{preview()}</span>
    </button>
    {#if open}
      <div class="j-children">
        {#each entries as [k, v] (k)}
          <Self value={v} name={k} depth={depth + 1} {expandTo} />
        {/each}
      </div>
    {/if}
  {:else}
    <div class="j-row j-leaf">
      {#if name !== null}<span class="j-key">{name}</span><span class="j-colon">:</span>{/if}
      <span class={leafClass(value)}>{leafText(value)}</span>
    </div>
  {/if}
</div>

<style>
  .jt {
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.7;
  }
  .j-row {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 0;
    background: none;
    border: none;
    text-align: left;
    width: 100%;
    color: var(--text);
  }
  .j-branch {
    cursor: pointer;
  }
  .j-caret {
    display: inline-flex;
    align-items: center;
    color: var(--text-faint);
    transition: transform 0.12s ease;
    transform: rotate(0deg);
    flex-shrink: 0;
  }
  .j-caret.open {
    transform: rotate(90deg);
  }
  .j-key {
    color: var(--info);
  }
  .j-colon {
    color: var(--text-faint);
    margin-left: -4px;
  }
  .j-preview {
    color: var(--text-faint);
  }
  .j-children {
    border-left: 1px solid var(--border);
    margin-left: 3px;
    padding-left: 12px;
  }
  .j-leaf {
    padding-left: 14px;
  }
  .j-str {
    color: var(--success);
    word-break: break-word;
  }
  .j-num {
    color: var(--warning);
  }
  .j-bool {
    color: var(--primary);
  }
  .j-null {
    color: var(--text-faint);
  }
</style>
