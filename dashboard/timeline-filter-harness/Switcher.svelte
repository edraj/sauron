<script lang="ts">
  /**
   * Holds the session id in `$state` and hands it to `SessionDetail` as a prop,
   * so switching sessions REUSES the component instance.
   *
   * That is the condition the real router creates on `#/sessions/A` →
   * `#/sessions/B`, and it is the only way to see whether a filter set on one
   * session leaks into the next. Remounting the page per fixture would reset
   * everything for free and prove nothing.
   */
  import SessionDetail from '../src/pages/SessionDetail.svelte';

  const IDS = ['full', 'no-issues', 'empty'];
  let id = $state(new URLSearchParams(location.search).get('session') ?? 'full');
</script>

<div class="switcher">
  {#each IDS as candidate (candidate)}
    <button type="button" onclick={() => (id = candidate)} aria-pressed={id === candidate}>
      {candidate}
    </button>
  {/each}
</div>

<SessionDetail params={{ id }} />

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
