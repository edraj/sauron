<script lang="ts">
  import { t } from '../lib/i18n';
  import { push } from 'svelte-spa-router';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import { authStore } from '../lib/stores/auth.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { errorMessage } from '../lib/api/client';
  import { isPasswordResetRequired } from '../lib/models/password-reset';

  let email = $state('');
  let password = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);
  /** The address the caller typed, held only once a reset refusal proves they
      know its password. */
  let resetRequiredFor = $state<string | null>(null);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (submitting) return;
    error = null;
    submitting = true;
    try {
      await authStore.login({ email: email.trim(), password });
      if (authStore.mustChangePassword) {
        push('/change-password');
        return;
      }
      await sessionStore.load(true);
      // First-time accounts land on onboarding — but only if they can actually
      // complete it. A member scoped to a single app or project sees no projects
      // here for a completely different reason, and onboarding would ask them to
      // create one they have no permission to create. Send them to the app shell,
      // which resolves their reachable app or shows the no-access state.
      const canOnboard = sessionStore.projects.length === 0 && sessionStore.can('project:create');
      push(canOnboard ? '/onboarding' : '/overview');
    } catch (err) {
      // Rendering this as a red form error is not enough. The target of an
      // admin-forced reset would otherwise see "an administrator reset this
      // password" in the same box as a typo'd password, from the same screen
      // they have just been told to stop using. The store branches the same way
      // on password_change_required.
      if (isPasswordResetRequired(err)) {
        resetRequiredFor = email.trim();
        return;
      }
      error = errorMessage(err);
    } finally {
      submitting = false;
    }
  }
</script>

<AuthLayout title={t('auth.login.submit')} subtitle={t('auth.login.subtitle')}>
  {#if resetRequiredFor}
    <div class="panel" role="status">
      <p>
        {t('auth.login.adminReset')}
        <strong>{resetRequiredFor}</strong> {t('prose.login.resetSent')}
      </p>
      <p class="muted">
        {t('auth.login.checkSpam')}
      </p>
    </div>
  {:else}
    <form onsubmit={submit} class="form">
      {#if error}<div class="alert" role="alert">{error}</div>{/if}
      <Input
        label={t('common.email')}
        type="email"
        bind:value={email}
        placeholder={t('auth.placeholder.email')}
        autocomplete="email"
        required
      />
      <Input
        label={t('common.password')}
        type="password"
        bind:value={password}
        placeholder="••••••••"
        autocomplete="current-password"
        required
      />
      <Button type="submit" variant="primary" size="lg" fullWidth loading={submitting}>
        {t('auth.login.submit')}
      </Button>
    </form>
  {/if}

  {#snippet footer()}
    <span>{t('auth.login.newHere')} <a href="#/register">{t('auth.login.createAccount')}</a></span>
    <span class="sep" aria-hidden="true">·</span>
    <span><a href="#/forgot-password">{t('auth.login.forgot')}</a></span>
  {/snippet}
</AuthLayout>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .alert {
    padding: 10px 12px;
    border-radius: var(--radius);
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
    color: var(--error);
    font-size: 13px;
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    font-size: 14px;
    line-height: 1.5;
  }
  .muted {
    color: var(--text-muted);
    font-size: 13px;
  }
  /* This is the only auth page whose footer carries two links, and `.auth-foot`
     is a plain block with no gap — so without a separator the two render 4px
     apart in the identical link colour, and "Create an account Forgot your
     password?" reads as one phrase with no clickable boundary. Measured. */
  .sep {
    margin: 0 6px;
    color: var(--text-muted);
  }
</style>
