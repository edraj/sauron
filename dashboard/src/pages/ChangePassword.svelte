<script lang="ts">
  import { t } from '../lib/i18n';
  import { replace } from 'svelte-spa-router';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import InlineNotice from '../lib/components/ui/InlineNotice.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import { authStore } from '../lib/stores/auth.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { errorMessage } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';

  let currentPassword = $state('');
  let newPassword = $state('');
  let confirmPassword = $state('');
  let saving = $state(false);
  let error = $state<string | null>(null);

  const tooShort = $derived(newPassword.length > 0 && newPassword.length < 8);
  const mismatch = $derived(confirmPassword.length > 0 && confirmPassword !== newPassword);
  const reused = $derived(
    newPassword.length > 0 && currentPassword.length > 0 && newPassword === currentPassword,
  );
  const canSubmit = $derived(
    !saving &&
      currentPassword.length > 0 &&
      newPassword.length >= 8 &&
      confirmPassword === newPassword &&
      newPassword !== currentPassword,
  );

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    error = null;
    saving = true;
    try {
      await authStore.applyPasswordChange(currentPassword, newPassword);
      toastStore.success(t('auth.change.toast.updated'));
      replace('/overview');
    } catch (err) {
      error = errorMessage(err);
    } finally {
      saving = false;
    }
  }

  // A button, not a link: signing out is an action. As a link, "open in new
  // tab" skipped the handler and opened /login while still signed in.
  async function signOut() {
    await authStore.logout();
    sessionStore.reset();
    replace('/login');
  }
</script>

<AuthLayout title={t('auth.change.title')} subtitle={t('auth.change.temporary')}>
  <form class="form" onsubmit={submit}>
    {#if error}<InlineNotice tone="error">{error}</InlineNotice>{/if}
    <Input
      label={t('auth.password.current')}
      type="password"
      bind:value={currentPassword}
      autocomplete="current-password"
      required
    />
    <Input
      label={t('auth.password.new')}
      type="password"
      bind:value={newPassword}
      autocomplete="new-password"
      hint={tooShort || reused ? undefined : t('auth.password.hint.minLength')}
      error={tooShort
        ? t('auth.password.error.tooShort')
        : reused
          ? t('auth.password.error.reused')
          : undefined}
      required
    />
    <Input
      label={t('auth.password.confirm')}
      type="password"
      bind:value={confirmPassword}
      autocomplete="new-password"
      error={mismatch ? t('auth.password.error.mismatch') : undefined}
      required
    />
    <Button type="submit" variant="primary" size="lg" fullWidth disabled={!canSubmit} loading={saving}>
      {t('auth.change.submit')}
    </Button>
  </form>

  {#snippet footer()}
    <Button variant="ghost" size="sm" onclick={signOut}>{t('auth.signOut')}</Button>
  {/snippet}
</AuthLayout>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
</style>
