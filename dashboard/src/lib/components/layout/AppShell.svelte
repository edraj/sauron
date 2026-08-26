<script lang="ts">
  import { t } from '../../i18n';
  import type { Snippet } from 'svelte';
  import { onMount } from 'svelte';
  // Aliased: an unqualified `location` import shadows `window.location`, and
  // the two `location.reload()` retry handlers below would then be called on a
  // Svelte store instead of the browser global.
  import { push, location as routePath } from 'svelte-spa-router';
  import Sidebar from './Sidebar.svelte';
  import Topbar from './Topbar.svelte';
  import Skeleton from '../ui/Skeleton.svelte';
  import EmptyState from '../ui/EmptyState.svelte';
  import Button from '../ui/Button.svelte';
  import PermissionDenied from '../PermissionDenied.svelte';
  import { sessionStore } from '../../stores/session.svelte';
  import { canAccessPage, resolvePageAccess } from '../../models/page-access';
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
  // ...and only to someone with no reachable project ANYWHERE. `needsScope` looks
  // at the current org alone, so an account holding a grant in one org while
  // sitting on another empty one used to be redirected here — and `/onboarding`
  // renders no Topbar, so there was no org switcher to get back with and the
  // only exit was signing out, which restored the same stored org and bounced
  // them straight back in.
  const hasProjectSomewhere = $derived(sessionStore.reachableProjectCount > 0);
  const shouldOnboard = $derived(needsScope && canOnboard && !hasProjectSomewhere);
  // The current org is empty but the user has projects elsewhere. That is a
  // navigation problem, not a first-run one: render inside the normal shell so
  // the org switcher stays reachable rather than routing away.
  const emptyOrg = $derived(needsScope && hasProjectSomewhere);
  // Zero reachable projects AND can't create one — onboarding would just ask for
  // a project they have no permission to make, so show a dead end instead.
  const noAccess = $derived(needsScope && !canOnboard && !hasProjectSomewhere);

  // The page-level permission gate. Deliberately resolved here rather than as a
  // router condition: a failed condition fires `conditionsFailed`, which
  // navigates — and a deep link the user cannot open should keep its URL and
  // explain itself, not silently land them somewhere else.
  //
  // Gated on `loaded` so the check never runs against an empty grant list
  // mid-bootstrap, which would flash the denied state on every cold load.
  const pageAccess = $derived(resolvePageAccess($routePath));
  const pageDenied = $derived(sessionStore.loaded && !canAccessPage(pageAccess));

  $effect(() => {
    if (shouldOnboard) push('/onboarding');
  });

  // Mirrors the onboarding effect above: only steer a requireApp page to /projects
  // once we're past the "no reachable projects" case entirely (onboarding/noAccess
  // above already own that case), so this never fires while noAccess holds.
  $effect(() => {
    if (sessionStore.loaded && !needsScope && requireApp && !sessionStore.currentAppId) {
      push('/admin/projects');
    }
  });
</script>

<div class="shell">
  <Sidebar />
  <Topbar />
  <main class="content">
    <div class="content-inner">
      {#if loadError}
        <EmptyState title={t('shell.workspace.errorTitle')} description={loadError} icon="triangle-alert">
          {#snippet action()}
            <Button variant="primary" onclick={() => location.reload()}>{t('common.retry')}</Button>
          {/snippet}
        </EmptyState>
      {:else if !sessionStore.loaded}
        <Skeleton rows={6} />
      {:else if sessionStore.accessError}
        <!-- Ranked above every permission-derived state below. When the access
             fetch fails, `can()` answers false for everything, so the nav
             empties and every action locks — indistinguishable from a real
             no-grants account unless we say so here. -->
        <EmptyState
          title={t('shell.permissions.errorTitle')}
          description={t('shell.permissions.errorBody')}
          icon="triangle-alert"
        >
          {#snippet action()}
            <Button variant="primary" onclick={() => location.reload()}>{t('common.retry')}</Button>
          {/snippet}
        </EmptyState>
      {:else if emptyOrg}
        <!-- Ranked above `noAccess`: this member DOES have projects, just not
             in the org they are standing in. Rendered here rather than routed
             to onboarding so the Topbar — and with it the org switcher — stays
             on screen; that is the only thing that makes this state escapable. -->
        <EmptyState
          title={t('shell.emptyOrg.title')}
          description={t('shell.emptyOrg.body')}
          icon="folders"
        >
          {#snippet action()}
            {#if canOnboard}
              <Button variant="primary" onclick={() => push('/onboarding')}>
                {t('shell.emptyOrg.create')}
              </Button>
            {/if}
          {/snippet}
        </EmptyState>
      {:else if noAccess}
        <EmptyState
          title={t('shell.noApps.title')}
          description={t('shell.noApps.body')}
          icon="lock"
        />
      {:else if pageDenied && pageAccess}
        <!-- Ranked BELOW `noAccess` on purpose: a member with zero reachable
             projects is better served by "No apps available" than by a
             technically-correct "requires event:read". -->
        <PermissionDenied access={pageAccess} />
      {:else}
        {@render children()}
      {/if}
    </div>
  </main>
</div>

<style>
  .shell {
    display: grid;
    /* `minmax(0, 1fr)`, not `1fr`. A bare `1fr` is `minmax(auto, 1fr)`, so the
       content track refuses to shrink below its min-content width — one wide
       table or long unbroken string and the track outgrows the viewport.
       Left-to-right that overflows to the right and reads as a stray
       horizontal scrollbar; right-to-left the same overflow lands at negative
       x, putting the start of every row off the near edge of the screen. */
    grid-template-columns: var(--sidebar-w) minmax(0, 1fr);
    grid-template-rows: var(--topbar-h) 1fr;
    grid-template-areas:
      'sidebar topbar'
      'sidebar content';
    min-height: 100vh;
  }
  .content {
    grid-area: content;
    /* `clip`, NOT `hidden`. They clip identically, but `overflow-x: hidden`
       forces the *other* axis to compute as `auto` (CSS Overflow 3: a
       non-`visible` value on one axis blockifies `visible` on the other), which
       made this element a scroll container. It is a grid row sized `1fr` inside
       a `min-height: 100vh` shell, so it grows to fit its content and never
       actually scrolls — and every `position: sticky` descendant was resolving
       against THAT box instead of the viewport, which is why the Docs table of
       contents and the admin rail sat still while the page scrolled past them.
       `clip` is the one value that clips without establishing a scroll
       container, so `overflow-y` stays `visible`. */
    overflow-x: clip;
  }
  .content-inner {
    max-width: var(--content-max);
    margin: 0 auto;
    padding: 28px 28px 64px;
    animation: fade-in 0.22s ease;
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
