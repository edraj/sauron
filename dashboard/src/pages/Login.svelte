<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import { authStore } from '../lib/stores/auth.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { errorMessage } from '../lib/api/client';

  let email = $state('');
  let password = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (submitting) return;
    error = null;
    submitting = true;
    try {
      await authStore.login({ email: email.trim(), password });
      await sessionStore.load(true);
      // First-time / project-less accounts land on onboarding.
      push(sessionStore.projects.length === 0 ? '/onboarding' : '/overview');
    } catch (err) {
      error = errorMessage(err);
    } finally {
      submitting = false;
    }
  }
</script>

<AuthLayout title="Sign in" subtitle="Welcome back. Watch every error and event.">
  <form onsubmit={submit} class="form">
    {#if error}<div class="alert" role="alert">{error}</div>{/if}
    <Input
      label="Email"
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
      placeholder="••••••••"
      autocomplete="current-password"
      required
    />
    <Button type="submit" variant="primary" size="lg" fullWidth loading={submitting}>
      Sign in
    </Button>
  </form>

  {#snippet footer()}
    <span>New to Sauron? <a href="#/register">Create an account</a></span>
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
</style>
