<!--
  What the result set below this line LEAVES OUT.

  Both facts here are ones the rows themselves cannot show: a window the
  planner narrowed to keep the query affordable, and a payload search that
  matched fewer columns than the reader's permissions suggest. The backend
  computes both carefully and, until this component, no page read either — so a
  query narrowed from 365 days to 30 rendered as a complete answer.

  `total_is_capped` deliberately does NOT appear here: `CursorPagination`
  already renders it as a `+` on the count, and a second surface for the same
  fact is noise.
-->
<script lang="ts">
  import Icon from '../ui/Icon.svelte';
  import { disclosuresFor } from './disclosures';
  import type { ClampInfo } from '../../api/search';

  interface Props {
    clamped?: ClampInfo | null;
    payloadSearched?: boolean | null;
  }
  let { clamped = null, payloadSearched = null }: Props = $props();

  const items = $derived(disclosuresFor(clamped, payloadSearched));
</script>

{#each items as d (d.text)}
  <p class="disclosure {d.tone}">
    <Icon name={d.tone === 'warning' ? 'triangle-alert' : 'info'} size={14} />
    <span>{d.text}</span>
  </p>
{/each}

<style>
  .disclosure {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 12px;
    padding: 8px 12px;
    border-radius: var(--radius);
    font-size: 12.5px;
  }
  .disclosure.warning {
    background: var(--warning-soft);
    color: var(--warning);
  }
  .disclosure.info {
    background: var(--info-soft);
    color: var(--info);
  }
</style>
