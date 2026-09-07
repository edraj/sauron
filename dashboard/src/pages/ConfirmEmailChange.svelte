<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from '../lib/i18n';
  import { querystring } from 'svelte-spa-router';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import { confirmEmailChange, previewEmailChange } from '../lib/api/auth';
  import type { EmailChangePreview } from '../lib/api/auth';
  import { errorMessage, isNormalizedError } from '../lib/api/client';
  import { readEmailChangeToken } from '../lib/models/email-change';

  // Read ONCE at init, not reactively, so a later navigation cannot swap the
  // token mid-submit. Same house pattern as ResetPassword.svelte — including
  // the `?? null`, because svelte-spa-router types `querystring` as
  // `Readable<string | undefined>` and it is genuinely undefined for a bare
  // `#/confirm-email-change` with no query at all.
  const token = readEmailChangeToken($querystring ?? null);

  let preview = $state<EmailChangePreview | null>(null);
  let loading = $state(true);
  let submitting = $state(false);
  let done = $state(false);
  let deadLink = $state(false);
  let error = $state<string | null>(null);

  // LOADS the pending change; never applies it.
  //
  // Outlook Safe Links and similar scanners fetch every URL in a delivered
  // message, and some execute JS. An onMount that confirmed would approve
  // changes nobody clicked — silently, and for every recipient whose employer
  // runs link scanning. The mutation fires only from the button below, and the
  // same rule holds on CancelEmailChange.svelte, where an auto-fire would
  // instead kill every pending request.
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
      if (p.role !== 'approve') {
        deadLink = true;
        return;
      }
      preview = p;
    } catch {
      // Expired, already used, cancelled, or never existed — the server does
      // not distinguish them and neither does this page.
      deadLink = true;
    } finally {
      loading = false;
    }
  });

  async function confirm() {
    if (!token || submitting) return;
    error = null;
    submitting = true;
    try {
      await confirmEmailChange(token);
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

<AuthLayout title={t('auth.emailChange.confirmTitle')}>
  {#if loading}
    <div class="panel" role="status"><p>{t('auth.emailChange.checking')}</p></div>
  {:else if deadLink}
    <div class="panel" role="status">
      <p>{t('auth.emailChange.invalidLink')}</p>
    </div>
  {:else if done}
    <!-- No forced sign-out, and no push to /login: sessions deliberately
         survive this change, so the user is still signed in everywhere. Only
         the address they type at the sign-in form has moved. -->
    <div class="panel" role="status">
      <p>{t('prose.auth.emailChangeConfirmed')}</p>
      <p><a href="#/login">{t('auth.forgot.backToSignIn')}</a></p>
    </div>
  {:else if preview}
    <div class="form">
      {#if error}<div class="alert" role="alert">{error}</div>{/if}
      <p>{t('prose.auth.emailChangeIntro')}</p>
      <p class="addr">{preview.new_email}</p>
      <p class="muted">{t('prose.auth.emailChangeOrg')} {preview.org_name}</p>
      <Button variant="primary" size="lg" fullWidth loading={submitting} onclick={confirm}>
        {t('auth.emailChange.confirm')}
      </Button>
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
  .addr {
    font-size: 16px;
    font-weight: 600;
    word-break: break-all;
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
