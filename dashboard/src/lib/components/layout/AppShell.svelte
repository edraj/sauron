<script lang="ts">
  import type { Snippet } from 'svelte';
  import { onMount } from 'svelte';
  import { push } from 'svelte-spa-router';
  import Sidebar from './Sidebar.svelte';
  import Topbar from './Topbar.svelte';
  import Spinner from '../ui/Spinner.svelte';
  import EmptyState from '../ui/EmptyState.svelte';
  import Button from '../ui/Button.svelte';
  import { sessionStore } from '../../stores/session.svelte';
  import { errorMessage } from '../../api/client';

  interface Props {
    // When true, redirect to onboarding if the org has no projects.
    requireProject?: boolean;
    // When true (Issues/Events), a current app is required to render the page;
    // otherwise steer the user to onboarding (no projects) or Projects (no app).
    requireApp?: boolean;
    children: Snippet;
  }

  let { requireProject = true, requireApp = false, children }: Props = $props();

  let loadError = $state<string | null>(null);

  onMount(async () => {
    try {
      await sessionStore.load();
      if (!sessionStore.loaded) return;
      if (sessionStore.projects.length === 0) {
        if (requireProject || requireApp) push('/onboarding');
        return;
      }
      if (requireApp && !sessionStore.currentAppId) {
        push('/projects');
      }
    } catch (err) {
      loadError = errorMessage(err);
    }
  });
</script>

<div class="shell">
  <Sidebar />
  <Topbar />
  <main class="content">
    <div class="content-inner">
      {#if loadError}
        <EmptyState title="Couldn't load workspace" description={loadError} icon="triangle-alert">
          {#snippet action()}
            <Button variant="primary" onclick={() => location.reload()}>Retry</Button>
          {/snippet}
        </EmptyState>
      {:else if !sessionStore.loaded}
        <div class="shell-loading"><Spinner size={26} /></div>
      {:else}
        {@render children()}
      {/if}
    </div>
  </main>
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: var(--sidebar-w) 1fr;
    grid-template-rows: var(--topbar-h) 1fr;
    grid-template-areas:
      'sidebar topbar'
      'sidebar content';
    min-height: 100vh;
  }
  .content {
    grid-area: content;
    overflow-x: hidden;
  }
  .content-inner {
    max-width: var(--content-max);
    margin: 0 auto;
    padding: 28px 28px 64px;
    animation: fade-in 0.22s ease;
  }
  .shell-loading {
    display: grid;
    place-items: center;
    min-height: 50vh;
  }

  @media (max-width: 860px) {
    .shell {
      grid-template-columns: 1fr;
      grid-template-rows: auto var(--topbar-h) 1fr;
      grid-template-areas:
        'sidebar'
        'topbar'
        'content';
    }
    .content-inner {
      padding: 18px 16px 48px;
    }
  }
</style>
