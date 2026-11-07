<!--
  "Couldn't load {thing}" — the one load-failure state. Replaces five patterns
  (four of them hand-rolled `.err-banner`s in different colours), and always
  offers Retry when the page can retry.

  `error` may be a NormalizedError, a CachedView (its error/errorStatus/
  errorCode are read), an Error, or a plain string; `describeLoadError` turns
  it into a human sentence and keeps the raw text for the "Details" line.
-->
<script lang="ts">
  import Button from './Button.svelte';
  import Icon from './Icon.svelte';
  import { t } from '../../i18n';
  import { describeLoadError, loadErrorText } from '../../models/load-error';

  interface Props {
    title: string;
    error: unknown;
    onretry?: () => void;
    /** `page` stands alone; `section` sits inside a section, compact. */
    size?: 'page' | 'section';
  }

  let { title, error, onretry, size = 'page' }: Props = $props();

  const d = $derived(describeLoadError(error));
  const sentence = $derived(loadErrorText(error));
  // Show the raw text only when it adds something beyond the sentence.
  const showDetails = $derived(!!d.reason && !!d.details && d.details !== sentence);
</script>

<!-- Assertive only for the page-level state. A page whose API is down renders
     several section errors at once, and stacked `alert`s interrupt each other. -->
<div class="es {size}" role={size === 'page' ? 'alert' : 'status'}>
  {#if size === 'page'}
    <div class="glyph" aria-hidden="true"><Icon name="triangle-alert" size={15} /></div>
  {/if}
  <p class="title">{title}</p>
  {#if sentence}<p class="reason">{sentence}</p>{/if}
  {#if showDetails}
    <p class="details"><span>{t('common.loadError.details')}:</span> {d.details}</p>
  {/if}
  {#if onretry}
    <div class="action">
      <Button variant="secondary" size="sm" onclick={onretry}>{t('common.retry')}</Button>
    </div>
  {/if}
</div>

<style>
  .es {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 4px;
  }
  .es.page {
    padding: 40px 24px;
  }
  .es.section {
    align-items: flex-start;
    text-align: start;
    padding: 12px 0;
  }
  .glyph {
    width: 30px;
    height: 30px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    border: 1px solid color-mix(in srgb, var(--error) 35%, transparent);
    color: var(--error);
    margin-bottom: 8px;
  }
  .title {
    font-size: var(--text-md);
    font-weight: var(--weight-strong);
  }
  .section .title {
    font-size: var(--text-sm);
    color: var(--error);
  }
  .reason {
    color: var(--text-muted);
    font-size: var(--text-sm);
    max-width: 52ch;
  }
  .details {
    color: var(--text-faint);
    font-size: var(--text-xs);
    max-width: 64ch;
    overflow-wrap: anywhere;
  }
  .action {
    margin-top: 10px;
  }
  .section .action {
    margin-top: 6px;
  }
</style>
