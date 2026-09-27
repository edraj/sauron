<!--
  Mounts ONE detail page against a stubbed API whose sections answer at
  different speeds, so "the header paints before the heavy cards" is something
  you can watch rather than infer.

    ?page=issue | device | monitor     which page to mount
    ?slow=<ms>                          hold every HEAVY section back this long
    ?fail=<section>                     make one section answer 500

  Against a local stub every section lands within a millisecond of the others,
  the skeletons are on screen for a single frame, and a page that waits for
  everything is indistinguishable from one that does not. `slow` is what
  separates them; `fail` shows that one section failing leaves the rest of the
  page standing.

  The Session and User pages have harnesses of their own
  (`timeline-filter-harness`, `person-harness`), which take `?slow=` too.
-->
<script lang="ts">
  import IssueDetail from '../src/pages/IssueDetail.svelte';
  import DeviceDetail from '../src/pages/DeviceDetail.svelte';
  import MonitorDetail from '../src/pages/MonitorDetail.svelte';

  const page = new URLSearchParams(location.search).get('page') ?? 'issue';
</script>

<main class="harness-main">
  {#if page === 'device'}
    <DeviceDetail params={{ key: encodeURIComponent('pixel-8/android-15') }} />
  {:else if page === 'monitor'}
    <MonitorDetail params={{ id: 'mon-1' }} />
  {:else}
    <IssueDetail params={{ id: 'issue-1' }} />
  {/if}
</main>

<style>
  .harness-main {
    padding: 24px 28px;
    max-width: 1280px;
  }
</style>
