<script lang="ts">
  import { t } from '../../i18n';
  import Icon from './Icon.svelte';

  interface Props {
    value: string;
    label?: string;
    size?: 'sm' | 'md';
  }

  let { value, label, size = 'md' }: Props = $props();

  const labelText = $derived(label ?? t('common.copy'));

  let copied = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      // Fallback for insecure contexts / older browsers.
      const ta = document.createElement('textarea');
      ta.value = value;
      ta.style.position = 'fixed';
      ta.style.opacity = '0';
      document.body.appendChild(ta);
      ta.select();
      try {
        document.execCommand('copy');
      } catch {
        /* ignore */
      }
      document.body.removeChild(ta);
    }
    copied = true;
    clearTimeout(timer);
    timer = setTimeout(() => (copied = false), 1600);
  }
</script>

<button class="copy {size}" class:copied onclick={copy} title={t('common.copyToClipboard')} type="button">
  <span class="ico"><Icon name={copied ? 'check' : 'copy'} size={13} /></span>
  <span class="txt">{copied ? t('common.copied') : labelText}</span>
</button>

<style>
  .copy {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-weight: var(--weight-medium);
    transition: background-color 0.12s ease, color 0.12s ease;
  }
  .copy.md {
    height: 28px;
    padding: 0 10px;
    font-size: var(--text-xs);
  }
  .copy.sm {
    height: 22px;
    padding: 0 7px;
    font-size: var(--text-xs);
  }
  .copy:hover {
    background: var(--surface-2);
    color: var(--text);
  }
  .copy.copied {
    color: var(--success);
    border-color: color-mix(in srgb, var(--success) 40%, transparent);
  }
  .ico {
    display: inline-flex;
    align-items: center;
  }
</style>
