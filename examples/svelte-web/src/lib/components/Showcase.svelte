<script lang="ts">
  import { runShowcase, MAX_USERS, DEFAULT_USERS } from '../showcase';
  import type { ShowcaseProgress, ShowcaseSummary } from '../showcase';
  import { sauronSink } from '../sauron';
  import { activity } from '../store.svelte';

  let { disabled = false }: { disabled?: boolean } = $props();

  let users = $state(DEFAULT_USERS);
  let running = $state(false);
  let progress = $state<ShowcaseProgress | null>(null);
  let summary = $state<ShowcaseSummary | null>(null);

  const pct = $derived(progress ? Math.round((progress.done / progress.total) * 100) : 0);
  const funnelMax = $derived(summary ? Math.max(1, summary.funnel[0].count) : 1);

  async function run() {
    if (running || disabled) return;
    running = true;
    summary = null;
    progress = null;
    const count = Math.max(1, Math.min(MAX_USERS, Math.floor(users) || DEFAULT_USERS));
    const runId = Date.now().toString(36);
    activity.push('system', 'Showcase started', `Simulating ${count} synthetic users…`);
    try {
      const result = await runShowcase(sauronSink(), { users: count, runId }, (p) => {
        progress = p;
      });
      summary = result;
      const completed = result.funnel.at(-1)?.count ?? 0;
      activity.push(
        'event',
        'Showcase complete',
        `${result.users} users · ${result.events} events · ${result.transactions} transactions · ${completed} completed checkout`,
      );
    } catch (err) {
      activity.push('error', 'Showcase failed', err instanceof Error ? err.message : String(err));
    } finally {
      running = false;
      progress = null;
    }
  }
</script>

<section class="showcase">
  <div class="head">
    <span class="dot" aria-hidden="true"></span>
    <h3>Run showcase</h3>
    <span class="tag">cohort</span>
  </div>
  <p class="desc">
    Drives the SDK through a synthetic e-commerce cohort — many users with realistic drop-off,
    branching paths and a spread of performance transactions. Populates the dashboard's
    <strong>Funnels</strong>, <strong>Journeys</strong> and <strong>Performance</strong> screens.
  </p>

  <div class="controls">
    <label class="field">
      <span>Users</span>
      <input type="number" min="1" max={MAX_USERS} bind:value={users} disabled={running || disabled} />
    </label>
    <button class="run" onclick={run} disabled={running || disabled}>
      {running ? 'Simulating…' : 'Run showcase'}
    </button>
  </div>

  {#if running && progress}
    <div class="progress" role="progressbar" aria-valuenow={pct} aria-valuemin="0" aria-valuemax="100">
      <div class="bar" style="width:{pct}%"></div>
    </div>
    <p class="prog-text mono">
      {progress.done} / {progress.total} users · {progress.events} events · {progress.transactions} txns
    </p>
  {/if}

  {#if summary && !running}
    <div class="results">
      <div class="funnel" aria-label="Funnel result">
        {#each summary.funnel as step (step.name)}
          <div class="frow">
            <span class="fname mono truncate">{step.name}</span>
            <div class="ftrack">
              <div class="ffill" style="width:{Math.round((step.count / funnelMax) * 100)}%"></div>
            </div>
            <span class="fcount mono">{step.count}</span>
          </div>
        {/each}
      </div>
      <p class="hint">
        Sent {summary.events} events + {summary.transactions} transactions across {summary.users} users.
        Open the dashboard → Web Demo → <strong>Funnels / Journeys / Performance</strong>.
      </p>
    </div>
  {/if}
</section>

<style>
  .showcase {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 18px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-left: 3px solid var(--primary);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-sm);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--primary);
    box-shadow: 0 0 0 4px var(--primary-soft);
    flex: none;
  }
  h3 {
    font-size: 14px;
    font-weight: 600;
    letter-spacing: -0.01em;
    flex: 1;
  }
  .tag {
    font-family: var(--font-mono);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--primary);
    background: var(--primary-soft);
    padding: 2px 7px;
    border-radius: var(--radius-pill);
    flex: none;
  }
  .desc {
    color: var(--text-muted);
    font-size: 12.5px;
    line-height: 1.5;
  }
  .desc strong {
    color: var(--text);
    font-weight: 600;
  }

  .controls {
    display: flex;
    align-items: flex-end;
    gap: 12px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
    color: var(--text-muted);
  }
  .field input {
    width: 92px;
    padding: 7px 10px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-mono);
  }
  .field input:focus {
    outline: none;
    border-color: var(--primary);
  }
  .run {
    padding: 8px 16px;
    border-radius: var(--radius);
    font-size: 12.5px;
    font-weight: 600;
    color: #fff;
    background: var(--primary);
    border: 1px solid var(--primary);
    transition: filter 0.15s ease;
  }
  .run:hover:not(:disabled) {
    filter: brightness(1.08);
  }
  .run:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .progress {
    height: 7px;
    background: var(--surface-3);
    border-radius: var(--radius-pill);
    overflow: hidden;
  }
  .bar {
    height: 100%;
    background: var(--primary);
    transition: width 0.2s ease;
  }
  .prog-text {
    font-size: 11.5px;
    color: var(--text-muted);
  }

  .results {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding-top: 4px;
  }
  .funnel {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .frow {
    display: grid;
    grid-template-columns: 150px 1fr 44px;
    align-items: center;
    gap: 10px;
  }
  .fname {
    font-size: 11.5px;
    color: var(--text-muted);
  }
  .ftrack {
    height: 16px;
    background: var(--surface-3);
    border-radius: var(--radius-sm);
    overflow: hidden;
  }
  .ffill {
    height: 100%;
    background: color-mix(in srgb, var(--primary) 70%, transparent);
    border-radius: var(--radius-sm);
    transition: width 0.3s ease;
  }
  .fcount {
    font-size: 12px;
    font-weight: 600;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .hint {
    font-size: 11.5px;
    color: var(--text-faint);
    line-height: 1.5;
  }
  .hint strong {
    color: var(--text-muted);
    font-weight: 600;
  }

  .mono {
    font-family: var(--font-mono);
  }
  .truncate {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
