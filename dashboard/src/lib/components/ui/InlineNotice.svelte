<!--
  A one-line notice inside a page: "showing cached data, refresh failed",
  "policy saved", "coverage is partial". Replaces `.stale-banner` (three
  copies), `.ok-banner`, `.notice`, `.policy-ok` and the sign-in pages' error
  boxes — each a slightly different box in a slightly different colour.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon, { type IconName } from './Icon.svelte';

  interface Props {
    tone: 'error' | 'warning' | 'info' | 'success';
    children: Snippet;
    action?: Snippet;
  }

  let { tone, children, action }: Props = $props();

  const icon: Record<Props['tone'], IconName> = {
    error: 'triangle-alert',
    warning: 'triangle-alert',
    info: 'info',
    success: 'check',
  };
</script>

<div class="notice {tone}" role={tone === 'error' ? 'alert' : 'status'}>
  <span class="ic" aria-hidden="true"><Icon name={icon[tone]} size={14} /></span>
  <div class="body">{@render children()}</div>
  {#if action}<div class="action">{@render action()}</div>{/if}
</div>

<style>
  .notice {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 8px 12px;
    border-radius: var(--radius);
    border: 1px solid;
    font-size: var(--text-sm);
    color: var(--text);
  }
  .ic {
    display: inline-flex;
    flex-shrink: 0;
  }
  .body {
    flex: 1;
    min-width: 0;
  }
  .action {
    flex-shrink: 0;
  }
  .error {
    background: var(--error-soft);
    border-color: color-mix(in srgb, var(--error) 25%, transparent);
  }
  .error .ic {
    color: var(--error);
  }
  .warning {
    background: var(--warning-soft);
    border-color: color-mix(in srgb, var(--warning) 25%, transparent);
  }
  .warning .ic {
    color: var(--warning);
  }
  .info {
    background: var(--info-soft);
    border-color: color-mix(in srgb, var(--info) 25%, transparent);
  }
  .info .ic {
    color: var(--info);
  }
  .success {
    background: var(--success-soft);
    border-color: color-mix(in srgb, var(--success) 25%, transparent);
  }
  .success .ic {
    color: var(--success);
  }
</style>
