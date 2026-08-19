<script lang="ts">
  import { t } from '../lib/i18n';
  import { querystring } from 'svelte-spa-router';
  import { get } from 'svelte/store';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
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

<div class="wrap">
  <Card title={t('auth.unsubscribe.title')}>
    {#if state === 'missing'}
      <p>{t('auth.unsubscribe.missingToken')}</p>
    {:else if state === 'working'}
      <Spinner />
    {:else}
      <p>{t('auth.unsubscribe.done')}</p>
      <p class="hint">{t('auth.unsubscribe.reenable')}</p>
      <Button href="#/account" variant="primary">{t('auth.unsubscribe.manage')}</Button>
    {/if}
  </Card>
</div>

<style>
  .wrap { max-width: 520px; margin: 64px auto; padding: 0 16px; }
  .hint { font-size: 13px; color: var(--text-faint); }
</style>
