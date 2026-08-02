<script lang="ts">
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import { forgotPassword } from '../lib/api/auth';
  import { errorMessage, isNormalizedError } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';

  let email = $state('');
  let submitting = $state(false);
  let sent = $state(false);
  let unsupported = $state(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (submitting) return;
    submitting = true;
    try {
      await forgotPassword(email.trim());
    } catch (err) {
      // A 429 additionally toasts the server's message, and a 404 means the
      // dashboard was upgraded ahead of the server. Everything else is
      // swallowed on purpose: the API answers 200 for an unknown address, for a
      // deactivated account and for a deployment with no SMTP, so a UI that
      // reported any of those would become the oracle the API refuses to be.
      if (isNormalizedError(err) && err.status === 404) {
        unsupported = true;
        return;
      }
      if (isNormalizedError(err) && err.status === 429) {
        toastStore.error(errorMessage(err));
      }
    } finally {
      submitting = false;
      // Always the same panel, whatever happened.
      if (!unsupported) sent = true;
    }
  }
</script>

<AuthLayout title="Reset your password" subtitle="We'll email you a link to choose a new one.">
  {#if unsupported}
    <div class="panel" role="status">
      <p>
        This server does not support password reset yet — ask an administrator to finish the
        upgrade.
      </p>
    </div>
  {:else if sent}
    <div class="panel" role="status">
      <p>
        If an account exists for that address, we have sent a link to reset the password. The link
        expires in 1 hour.
      </p>
      <p class="muted">Nothing arrived? Check your spam folder, then try again in a little while.</p>
    </div>
  {:else}
    <form onsubmit={submit} class="form">
      <Input label="Email" type="email" bind:value={email} autocomplete="email" required />
      <Button type="submit" variant="primary" size="lg" fullWidth loading={submitting}>
        Email me a link
      </Button>
    </form>
  {/if}

  {#snippet footer()}
    <span><a href="#/login">Back to sign in</a></span>
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
  .muted {
    color: var(--text-muted);
    font-size: 13px;
  }
</style>
