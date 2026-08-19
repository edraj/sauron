<script lang="ts">
  import { t } from '../../i18n';
  interface Props {
    size?: number;
    stroke?: number;
  }
  let { size = 20, stroke = 2 }: Props = $props();
</script>

<span
  class="spinner"
  style="--s:{size}px; --sw:{stroke}px"
  role="status"
  aria-label={t('common.loading')}
></span>

<style>
  .spinner {
    display: inline-block;
    width: var(--s);
    height: var(--s);
    border-radius: 50%;
    border: var(--sw) solid var(--border-strong);
    border-top-color: var(--primary);
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
