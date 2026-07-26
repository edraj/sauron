<script lang="ts">
  import { replace } from 'svelte-spa-router';
  import Button from '../lib/components/ui/Button.svelte';
  import Card from '../lib/components/ui/Card.svelte';
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
      toastStore.success('Password updated.');
      replace('/overview');
    } catch (err) {
      error = errorMessage(err);
    } finally {
      saving = false;
    }
  }

  async function signOut() {
    await authStore.logout();
    sessionStore.reset();
    replace('/login');
  }
</script>

<div class="cp">
  <header class="cp-top">
    <div class="brand">
      <span class="mark" aria-hidden="true"><span class="eye"></span></span>
      <span class="name">Sauron</span>
    </div>
    <button class="link" onclick={signOut}>Sign out</button>
  </header>

  <div class="cp-body">
    <div class="intro">
      <h1>Choose a password</h1>
      <p class="lead">
        Your account was created with a temporary password. Choose your own before continuing.
      </p>
    </div>

    <Card>
      <form class="cp-form" onsubmit={submit}>
        {#if error}<div class="alert" role="alert">{error}</div>{/if}
        <Input
          label="Current password"
          type="password"
          bind:value={currentPassword}
          autocomplete="current-password"
          required
        />
        <Input
          label="New password"
          type="password"
          bind:value={newPassword}
          autocomplete="new-password"
          hint={tooShort || reused ? undefined : 'At least 8 characters.'}
          error={tooShort
            ? 'Must be at least 8 characters.'
            : reused
              ? 'Must be different from your current password.'
              : undefined}
          required
        />
        <Input
          label="Confirm new password"
          type="password"
          bind:value={confirmPassword}
          autocomplete="new-password"
          error={mismatch ? 'Passwords do not match.' : undefined}
          required
        />
        <Button type="submit" variant="primary" size="lg" fullWidth disabled={!canSubmit} loading={saving}>
          Update password
        </Button>
      </form>
    </Card>
  </div>
</div>

<style>
  .cp {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
  }
  .cp-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 24px;
    border-bottom: 1px solid var(--border);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .mark {
    width: 28px;
    height: 28px;
    border-radius: 8px;
    background: radial-gradient(circle at 50% 45%, #ffe08a 0%, #f5a623 45%, #e0524a 100%);
    display: grid;
    place-items: center;
  }
  .eye {
    width: 5px;
    height: 15px;
    background: #0a0c10;
    border-radius: 50%;
  }
  .name {
    font-weight: 700;
    font-size: 16px;
  }
  .link {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 13px;
  }
  .link:hover {
    color: var(--text);
    text-decoration: underline;
  }
  .cp-body {
    flex: 1;
    width: 100%;
    max-width: 460px;
    margin: 0 auto;
    padding: 48px 22px 64px;
    display: flex;
    flex-direction: column;
    gap: 18px;
    animation: fade-in 0.25s ease;
  }
  .intro h1 {
    font-size: 25px;
  }
  .lead {
    color: var(--text-muted);
    margin-top: 8px;
    font-size: 14px;
  }
  .cp-form {
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
</style>
