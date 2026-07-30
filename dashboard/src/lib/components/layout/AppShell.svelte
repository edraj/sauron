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
    } catch (err) {
      loadError = errorMessage(err);
    }
  });

  // Derived, not latched: switching orgs from the Topbar mutates sessionStore without
  // remounting this component (Topbar calls setOrg with no route push), so a flag set
  // once in onMount would strand the member on the empty state in an org where they
  // actually have access.
  const needsScope = $derived(
    sessionStore.loaded && sessionStore.projects.length === 0 && (requireProject || requireApp),
  );
  // Onboarding asks the member to create a project, so only offer it to someone who
  // can actually create one — otherwise it is a dead end they cannot exit.
  const canOnboard = $derived(sessionStore.can('project:create'));
  // Set when the member has zero reachable projects AND can't create one —
  // onboarding would just ask them to create a project they have no
  // permission for, so show a dead-end state instead of redirecting there.
  const noAccess = $derived(needsScope && !canOnboard);

  $effect(() => {
    if (needsScope && canOnboard) push('/onboarding');
  });

  // Mirrors the onboarding effect above: only steer a requireApp page to /projects
  // once we're past the "no reachable projects" case entirely (onboarding/noAccess
  // above already own that case), so this never fires while noAccess holds.
  $effect(() => {
    if (sessionStore.loaded && !needsScope && requireApp && !sessionStore.currentAppId) {
      push('/projects');
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
      {:else if noAccess}
        <EmptyState
          title="No apps available"
          description="You don't have access to any app in this organization yet. Ask an administrator to grant you access."
          icon="lock"
        />
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
