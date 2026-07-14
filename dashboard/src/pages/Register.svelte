<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import { authStore } from '../lib/stores/auth.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { errorMessage } from '../lib/api/client';

  let name = $state('');
  let email = $state('');
  let password = $state('');
  let orgName = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (submitting) return;
    error = null;
    submitting = true;
    try {
      await authStore.register({
        email: email.trim(),
        password,
        name: name.trim() || undefined,
        org_name: orgName.trim(),
      });
      await sessionStore.load(true);
      push('/onboarding');
    } catch (err) {
      error = errorMessage(err);
    } finally {
      submitting = false;
    }
  }
</script>

<AuthLayout title="Create your account" subtitle="Spin up a workspace in seconds.">
  <form onsubmit={submit} class="form">
    {#if error}<div class="alert" role="alert">{error}</div>{/if}
    <Input label="Name" bind:value={name} placeholder="Ada Lovelace" autocomplete="name" />
    <Input
      label="Work email"
      type="email"
      bind:value={email}
      placeholder="you@company.com"
      autocomplete="email"
      required
    />
    <Input
      label="Password"
      type="password"
      bind:value={password}
      placeholder="At least 8 characters"
      autocomplete="new-password"
      required
    />
    <Input
      label="Organization name"
      bind:value={orgName}
      placeholder="Acme Inc."
      required
    />
    <Button type="submit" variant="primary" size="lg" fullWidth loading={submitting}>
      Create account
    </Button>
  </form>

  {#snippet footer()}
    <span>Already have an account? <a href="#/login">Sign in</a></span>
  {/snippet}
</AuthLayout>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: 15px;
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
