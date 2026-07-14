<script lang="ts">
  import CopyButton from './CopyButton.svelte';

  interface Props {
    code: string;
    language?: string;
    copy?: boolean;
  }

  let { code, language, copy = true }: Props = $props();
</script>

<div class="codeblock">
  <div class="bar">
    <span class="lang">{language ?? 'snippet'}</span>
    {#if copy}<CopyButton value={code} size="sm" />{/if}
  </div>
  <pre><code>{code}</code></pre>
</div>

<style>
  .codeblock {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 7px 10px 7px 14px;
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--surface-3) 60%, transparent);
  }
  .lang {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  pre {
    margin: 0;
    padding: 14px 16px;
    overflow-x: auto;
    font-size: 12.5px;
    line-height: 1.6;
    color: var(--text);
  }
  code {
    white-space: pre;
  }
</style>
