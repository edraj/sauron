<script lang="ts">
  import { t } from '../lib/i18n';
  import { querystring } from 'svelte-spa-router';
  import { get } from 'svelte/store';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import InlineNotice from '../lib/components/ui/InlineNotice.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import { unsubscribe } from '../lib/api/notification-prefs';

  // Read once at init, NEVER inside an effect: an effect that re-reads the
  // query string would re-POST the token on every unrelated store update.
  const token = new URLSearchParams(get(querystring) ?? '').get('token') ?? '';

  let state = $state<'working' | 'done' | 'missing'>(token ? 'working' : 'missing');

  $effect(() => {
    if (state !== 'working') return;
    void (async () => {
      try {
        await unsubscribe(token);
      } catch {
        // The endpoint answers a generic 200 whether or not the token matched,
        // so anything reaching here is a transport problem. Show the same
        // confirmation either way rather than inviting a retry loop that the
        // rate limiter will refuse.
      }
      state = 'done';
    })();
  });
</script>

<AuthLayout title={t('auth.unsubscribe.title')}>
  {#if state === 'missing'}
    <div class="panel" role="status">
      <p>{t('auth.unsubscribe.missingToken')}</p>
    </div>
  {:else if state === 'working'}
    <div class="working"><Spinner size={18} /></div>
  {:else}
    <div class="form">
      <InlineNotice tone="success">{t('auth.unsubscribe.done')}</InlineNotice>
      <div class="panel">
        <p class="muted">{t('auth.unsubscribe.reenable')}</p>
      </div>
      <Button href="#/account" variant="primary" size="lg" fullWidth>{t('auth.unsubscribe.manage')}</Button>
    </div>
  {/if}
</AuthLayout>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  .muted {
    color: var(--text-muted);
  }
  .working {
    display: flex;
    justify-content: center;
    padding: 8px 0;
  }
</style>
