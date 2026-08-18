<script lang="ts">
  /**
   * The four surfaces this change touches, mounted against a stubbed API.
   *
   * Each is a real page: the toolbars, the FilterBar's chip builder, the
   * timeline's quick view and Issue detail's rail all run their own code paths
   * here, with only the server canned. That is the point — the defects this
   * work is exposed to (a control that renders but queries nothing, a chip that
   * encodes to a parameter axios drops, a modal that mounts with no data) are
   * all invisible to `svelte-check` and to the unit suite.
   */
  import SessionsList from '../src/pages/SessionsList.svelte';
  import UsersExplorer from '../src/pages/UsersExplorer.svelte';
  import SessionDetail from '../src/pages/SessionDetail.svelte';
  import IssueDetail from '../src/pages/IssueDetail.svelte';

  const PAGES = ['sessions', 'users', 'session-detail', 'issue-detail'] as const;
  type Page = (typeof PAGES)[number];
  let page = $state<Page>(
    ((new URLSearchParams(location.search).get('page') as Page) ?? 'sessions'),
  );
</script>

<div class="switcher">
  {#each PAGES as candidate (candidate)}
    <button type="button" onclick={() => (page = candidate)} aria-pressed={page === candidate}>
      {candidate}
    </button>
  {/each}
</div>

{#if page === 'sessions'}
  <SessionsList />
{:else if page === 'users'}
  <UsersExplorer />
{:else if page === 'session-detail'}
  <SessionDetail params={{ id: 'sess-harness-01' }} />
{:else}
  <IssueDetail params={{ id: 'issue-1' }} />
{/if}

<style>
  .switcher {
    position: fixed;
    right: 12px;
    bottom: 12px;
    z-index: 9999;
    display: flex;
    gap: 6px;
    padding: 6px;
    border-radius: 8px;
    background: #111;
    border: 1px solid #444;
  }
  .switcher button {
    font-size: 12px;
    padding: 4px 8px;
    color: #ccc;
    background: #222;
    border: 1px solid #555;
    border-radius: 5px;
  }
  .switcher button[aria-pressed='true'] {
    color: #fff;
    background: #0a5;
  }
</style>
