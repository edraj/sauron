<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from '../lib/i18n';
  import { querystring } from 'svelte-spa-router';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import { cancelEmailChange, previewEmailChange } from '../lib/api/auth';
  import type { EmailChangePreview } from '../lib/api/auth';
  import { errorMessage, isNormalizedError } from '../lib/api/client';
  import { readEmailChangeToken } from '../lib/models/email-change';

  // Read ONCE at init — see ConfirmEmailChange.svelte's identical comment.
  const token = readEmailChangeToken($querystring ?? null);

  let preview = $state<EmailChangePreview | null>(null);
  let loading = $state(true);
  let submitting = $state(false);
  let done = $state(false);
  let deadLink = $state(false);
  let error = $state<string | null>(null);

  // LOADS the pending change; never cancels it. An onMount that cancelled would
  // let a mail link-scanner silently kill every pending request — see the fuller
  // note in ConfirmEmailChange.svelte. The mutation is the button below.
  onMount(async () => {
    if (!token) {
      deadLink = true;
      loading = false;
      return;
    }
    try {
      const p = await previewEmailChange(token);
      // The token has to be the RIGHT HALF of the pair. Both halves resolve —
      // `preview` accepts either — so opening the other one here would
      // otherwise render a plausible page whose button always 401s, and on the
      // confirm side an address field that is simply blank because the server
      // (correctly) withheld it. A dead-link state is the honest answer.
      if (p.role !== 'cancel') {
        deadLink = true;
        return;
      }
      preview = p;
    } catch {
      deadLink = true;
    } finally {
      loading = false;
    }
  });

  async function stop() {
    if (!token || submitting) return;
    error = null;
    submitting = true;
    try {
      await cancelEmailChange(token);
      done = true;
    } catch (err) {
      if (isNormalizedError(err) && err.status === 401) {
        deadLink = true;
        return;
      }
      error = errorMessage(err);
    } finally {
      submitting = false;
    }
  }
</script>

<AuthLayout title={t('auth.emailChange.cancelTitle')}>
  {#if loading}
    <div class="panel" role="status"><p>{t('auth.emailChange.checking')}</p></div>
  {:else if deadLink}
    <div class="panel" role="status">
      <p>{t('auth.emailChange.invalidLink')}</p>
    </div>
  {:else if done}
    <!-- "Nothing was changed", not "the change was undone": it had not taken
         effect, and telling someone their address was reverted would send them
         looking for a change that never happened. -->
    <div class="panel" role="status">
      <p>{t('prose.auth.emailChangeCancelled')}</p>
      <p><a href="#/login">{t('auth.forgot.backToSignIn')}</a></p>
    </div>
  {:else if preview}
    <div class="form">
      {#if error}<div class="alert" role="alert">{error}</div>{/if}
      <!-- `preview.new_email` is DELIBERATELY not rendered here, and the server
           does not send it for a cancellation token: this page is reached from
           the mail to the address being REPLACED, which must never learn what
           it is being replaced with. Do not add it if a future API change
           starts sending the field. -->
      <p>{t('prose.auth.emailChangeCancelIntro')}</p>
      <p class="muted">{t('prose.auth.emailChangeOrg')} {preview.org_name}</p>
      <Button variant="danger" size="lg" fullWidth loading={submitting} onclick={stop}>
        {t('auth.emailChange.stop')}
      </Button>
      <p class="muted">{t('prose.auth.emailChangeCancelIgnore')}</p>
    </div>
  {/if}

  {#snippet footer()}
    <span><a href="#/login">{t('auth.forgot.backToSignIn')}</a></span>
  {/snippet}
</AuthLayout>

<style>
  .panel {
    text-align: center;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .muted {
    color: var(--text-muted);
    font-size: 13px;
  }
  .alert {
    color: var(--danger, #c0392b);
    font-size: 13.5px;
  }
</style>
