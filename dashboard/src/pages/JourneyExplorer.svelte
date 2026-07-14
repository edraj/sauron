<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import BarList from '../lib/components/BarList.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SankeyChart from '../lib/components/SankeyChart.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { getJourney } from '../lib/api/journeys';
  import { errorMessage } from '../lib/api/client';
  import type { Journey } from '../lib/models';

  const DEPTHS = [2, 3, 4, 5, 6, 7, 8];

  let sinceDays = $state(30);
  let depth = $state(5);

  let journey = $state<Journey | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load(appId: string, days: number, d: number) {
    loading = true;
    error = null;
    try {
      journey = await getJourney(appId, { since_days: days, depth: d });
    } catch (err) {
      error = errorMessage(err);
      journey = null;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    const d = depth;
    if (aid) void load(aid, days, d);
  });

  const entryPoints = $derived(
    journey
      ? journey.nodes
          .filter((n) => n.step === 0)
          .sort((a, b) => b.count - a.count)
          .map((n) => ({ name: n.event, count: n.count }))
      : [],
  );

  const topTransitions = $derived(
    journey ? [...journey.links].sort((a, b) => b.count - a.count).slice(0, 10) : [],
  );

  function retry() {
    const aid = sessionStore.currentAppId;
    if (aid) void load(aid, sinceDays, depth);
  }
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Journeys</h1>
      <p class="muted sub">Trace how users move through your product, one event at a time.</p>
    </div>
    <div class="controls">
      <div class="control">
        <span class="ctrl-label">Range</span>
        <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} />
      </div>
      <div class="control">
        <span class="ctrl-label">Depth</span>
        <div class="depths" role="tablist" aria-label="Journey depth">
          {#each DEPTHS as d (d)}
            <button
              class="depth"
              class:active={depth === d}
              onclick={() => (depth = d)}
              type="button"
              role="tab"
              aria-selected={depth === d}
            >
              {d}
            </button>
          {/each}
        </div>
      </div>
    </div>
  </div>

  {#if error && !journey}
    <Card>
      <EmptyState title="Couldn't load journeys" description={error} icon="triangle-alert">
        {#snippet action()}
          <Button variant="secondary" onclick={retry}>Retry</Button>
        {/snippet}
      </EmptyState>
    </Card>
  {:else if loading && !journey}
    <Card>
      <div class="center"><Spinner size={24} /></div>
    </Card>
  {:else if journey && journey.nodes.length === 0}
    <Card>
      <EmptyState
        title="Not enough event data to map journeys"
        description="Once users trigger a sequence of events in a session, their paths will appear here."
        icon="compass"
      />
    </Card>
  {:else if journey}
    <div class="journey-card">
      {#if loading}
        <div class="reloading"><Spinner size={16} /><span class="faint">Updating…</span></div>
      {/if}
      <Card title="User journeys">
        <SankeyChart {journey} height={480} />
        <p class="caption muted">
          Each column is the Nth event in a user's session; ribbons show how many users moved
          from one event to the next.
        </p>
      </Card>
    </div>

    <div class="grid">
      <Card title="Top entry points">
        {#if entryPoints.length === 0}
          <p class="faint empty-inline">No entry events in this range.</p>
        {:else}
          <p class="hint muted">The first event users fire when a session begins.</p>
          <BarList items={entryPoints} valueLabel="users" />
        {/if}
      </Card>

      <Card title="Top transitions" padding="none">
        {#if topTransitions.length === 0}
          <p class="faint empty-inline pad">No transitions between events yet.</p>
        {:else}
          <DataTable>
            {#snippet head()}
              <tr>
                <th>From</th>
                <th></th>
                <th>To</th>
                <th class="num">Users</th>
              </tr>
            {/snippet}
            {#snippet children()}
              {#each topTransitions as t, i (i)}
                <tr>
                  <td>
                    <span class="mono">{t.from_event}</span>
                    <span class="faint step-tag">step {t.from_step + 1}</span>
                  </td>
                  <td class="arrow faint">→</td>
                  <td><span class="mono">{t.to_event}</span></td>
                  <td class="num">{t.count.toLocaleString()}</td>
                </tr>
              {/each}
            {/snippet}
          </DataTable>
        {/if}
      </Card>
    </div>
  {/if}
</AppShell>

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 20px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }
  .controls {
    display: flex;
    align-items: flex-end;
    gap: 16px;
    flex-wrap: wrap;
  }
  .control {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .ctrl-label {
    font-size: 10.5px;
    font-weight: 620;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .depths {
    display: inline-flex;
    gap: 4px;
    padding: 4px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .depth {
    min-width: 30px;
    padding: 6px 9px;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: 12.5px;
    font-weight: 560;
    border-radius: var(--radius-sm);
    font-variant-numeric: tabular-nums;
  }
  .depth:hover {
    color: var(--text);
  }
  .depth.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .center {
    display: grid;
    place-items: center;
    min-height: 260px;
  }
  .journey-card {
    position: relative;
    margin-bottom: 18px;
  }
  .reloading {
    position: absolute;
    top: 14px;
    right: 18px;
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12px;
  }
  .caption {
    font-size: 12.5px;
    margin-top: 14px;
    max-width: 640px;
    line-height: 1.5;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1.4fr;
    gap: 18px;
    align-items: start;
  }
  .hint {
    font-size: 12px;
    margin-bottom: 12px;
  }
  .empty-inline {
    font-size: 13px;
  }
  .empty-inline.pad {
    padding: 18px;
  }
  .step-tag {
    margin-left: 8px;
    font-size: 11px;
  }
  .arrow {
    text-align: center;
    font-size: 14px;
  }

  @media (max-width: 900px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
