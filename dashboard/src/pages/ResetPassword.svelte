<script lang="ts">
  import { t } from '../lib/i18n';
  import { querystring, replace } from 'svelte-spa-router';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import { resetPassword } from '../lib/api/auth';
  import { errorMessage, isNormalizedError } from '../lib/api/client';
  import { authStore } from '../lib/stores/auth.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { passwordRules, readResetToken } from '../lib/models/password-reset';

  // Read ONCE at init, not reactively, so a later navigation cannot swap the
  // token mid-submit. Same house pattern as Issues.svelte — including the
  // `?? null`, because svelte-spa-router types `querystring` as
  // `Readable<string | undefined>` and it is genuinely undefined for a bare
  // `#/reset-password` with no query at all.
  const token = readResetToken($querystring ?? null);

  let newPassword = $state('');
  let confirmPassword = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);
  let deadLink = $state(false);

  const rules = $derived(passwordRules(newPassword, confirmPassword));

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!token || !rules.canSubmit || submitting) return;
    error = null;
    submitting = true;
    try {
      await resetPassword(token, newPassword);
      toastStore.success('Password updated. Sign in with your new password.');
      // These three statements, in this order. `replace('/login')` alone is a
      // no-op for the visitor this page most needs to handle: App.svelte pushes
      // `authStore.isAuthenticated` visitors to /issues, and `isAuthenticated`
      // is pure local state untouched by a reset that happened server-side. A
      // user already signed in in another tab would otherwise be bounced into
      // /issues on a session the backend just revoked, never see the login
      // screen, and only be ejected when refresh() fails.
      await authStore.logout();
      sessionStore.reset();
      replace('/login');
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

<AuthLayout title={t('auth.reset.title')}>
  {#if !token || deadLink}
    <div class="panel" role="status">
      <p>{t('auth.reset.invalidLink')}</p>
      <p><a href="#/forgot-password">{t('auth.reset.requestNew')}</a></p>
    </div>
  {:else}
    <form onsubmit={submit} class="form">
      {#if error}<div class="alert" role="alert">{error}</div>{/if}
      <Input
        label={t('auth.password.new')}
        type="password"
        bind:value={newPassword}
        autocomplete="new-password"
        hint={rules.tooShort ? undefined : 'At least 8 characters.'}
        error={rules.tooShort ? 'Must be at least 8 characters.' : undefined}
        required
      />
      <Input
        label={t('auth.password.confirm')}
        type="password"
        bind:value={confirmPassword}
        autocomplete="new-password"
        error={rules.mismatch ? 'Passwords do not match.' : undefined}
        required
      />
      <Button
        type="submit"
        variant="primary"
        size="lg"
        fullWidth
        disabled={!rules.canSubmit}
        loading={submitting}
      >
        {t('auth.reset.submit')}
      </Button>
    </form>
  {/if}

  {#snippet footer()}
    <span><a href="#/login">{t('auth.forgot.backToSignIn')}</a></span>
  {/snippet}
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
    font-size: 14px;
    line-height: 1.5;
  }
  .alert {
    padding: 10px 12px;
    border-radius: var(--radius);
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
    color: var(--error);
    font-size: 13px;
  }
</style>
