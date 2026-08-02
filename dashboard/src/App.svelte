<script lang="ts">
  import { onMount } from 'svelte';
  import Router, { location, push } from 'svelte-spa-router';
  import { routes } from './routes';
  import { authStore } from './lib/stores/auth.svelte';
  import Toast from './lib/components/ui/Toast.svelte';
  import Spinner from './lib/components/ui/Spinner.svelte';

  let booted = $state(false);

  // '/reset-password' is DELIBERATELY absent. This array feeds the $effect
  // below that pushes authenticated users to /issues, and a logged-in user
  // clicking their own reset link would be bounced off it before they could use
  // it. It is neither listed here nor guarded in routes.ts, so it simply
  // renders for everyone. '/forgot-password' IS listed: an authenticated user
  // who lands there wants Change password instead.
  //
  // Note `$location` from svelte-spa-router excludes the query string, so the
  // comparison is '/reset-password' even with ?token=… — which is exactly why
  // omitting it here is load-bearing rather than accidental.
  const PUBLIC_ROUTES = ['/login', '/register', '/forgot-password'];

  onMount(async () => {
    await authStore.boot();
    booted = true;
  });

  // Once booted, keep authenticated users out of the login/register pages.
  $effect(() => {
    if (!booted) return;
    if (authStore.isAuthenticated && PUBLIC_ROUTES.includes($location)) {
      push('/issues');
    }
  });

  function onConditionsFailed() {
    // A guarded route rejected the visitor. Tell apart "not logged in" (send
    // to /login) from "logged in but owes a password change" (send to
    // /change-password) — routes.ts's passwordCurrent() reports the latter
    // without navigating itself, so this is the one place that decides,
    // rather than two navigations racing each other.
    if (authStore.isAuthenticated && authStore.mustChangePassword) {
      push('/change-password');
    } else {
      push('/login');
    }
  }
</script>

{#if !booted}
  <div class="boot">
    <div class="boot-mark" aria-hidden="true"><span class="eye"></span></div>
    <Spinner size={22} />
    <span class="boot-text">Loading Sauron…</span>
  </div>
{:else}
  <Router {routes} on:conditionsFailed={onConditionsFailed} />
{/if}

<Toast />

<style>
  .boot {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 16px;
  }
  .boot-mark {
    width: 44px;
    height: 44px;
    border-radius: 12px;
    background: radial-gradient(circle at 50% 45%, #ffe08a 0%, #f5a623 45%, #e0524a 100%);
    display: grid;
    place-items: center;
    box-shadow: 0 0 30px rgba(240, 120, 60, 0.4);
  }
  .eye {
    width: 7px;
    height: 22px;
    background: var(--bg);
    border-radius: 50%;
  }
  .boot-text {
    color: var(--text-faint);
    font-size: 13px;
  }
</style>
