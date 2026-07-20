<script lang="ts">
  import { runSeeding, PRESETS, type PresetKey, type SeedProgress, type SeedSummary } from '../seeding';
  import type { Level } from '@sauron/browser';
  import { seedingSink, captureExampleError } from '../sauron';
  import { activity } from '../store.svelte';

  let { disabled = false }: { disabled?: boolean } = $props();

  let preset = $state<PresetKey>('medium');
  let running = $state(false);
  let progress = $state<SeedProgress | null>(null);
  let summary = $state<SeedSummary | null>(null);

  const presetKeys = Object.keys(PRESETS) as PresetKey[];
  const pct = $derived(progress ? Math.round((progress.done / progress.total) * 100) : 0);

  const LEVELS: Level[] = ['fatal', 'error', 'warning', 'info', 'debug'];
  const LEVEL_COLOR: Record<Level, string> = {
    fatal: '#b4232a',
    error: '#e5484d',
    warning: '#f5a623',
    info: '#4f9bff',
    debug: '#8a94a6',
  };

  async function run() {
    if (running || disabled) return;
    running = true;
    summary = null;
    progress = null;
    const visitors = PRESETS[preset].visitors;
    const runId = Date.now().toString(36);
    activity.push('system', 'Seeding started', `${PRESETS[preset].label} · ${visitors} synthetic visitors…`);
    try {
      const result = await runSeeding(seedingSink(), { visitors, runId }, (p) => {
        progress = p;
      });
      summary = result;
      activity.push(
        'event',
        'Seeding complete',
        `${result.errors} errors across ${result.issues} issues · ${result.events} events · ` +
          `${result.bigPayloads} big payloads · ${result.taggedErrors} tagged`,
      );
    } catch (err) {
      activity.push('error', 'Seeding failed', err instanceof Error ? err.message : String(err));
    } finally {
      running = false;
      progress = null;
    }
  }
</script>

<section class="seeding">
  <div class="head">
    <span class="dot" aria-hidden="true"></span>
    <h3>Seed demo data</h3>
    <span class="tag">seed</span>
  </div>
  <p class="desc">
    Fires a bulk, deliberately <strong>mixed</strong> stream of <strong>errors</strong> and
    <strong>events</strong> — every level (<code>debug…fatal</code>), grouped &amp; one-off issues,
    some with tags, some with big payloads, across many synthetic users and screens. Populates the
    dashboard's <strong>Issues</strong>, <strong>Events</strong>, <strong>Users</strong> and
    <strong>Screens</strong>.
  </p>

  <div class="controls">
    <div class="presets" role="group" aria-label="Volume">
      {#each presetKeys as key (key)}
        <button
          type="button"
          class="preset"
          class:active={preset === key}
          disabled={running || disabled}
          onclick={() => (preset = key)}
        >
          {PRESETS[key].label}
          <span class="count mono">{PRESETS[key].visitors}★</span>
        </button>
      {/each}
    </div>
    <button class="run" onclick={run} disabled={running || disabled}>
      {running ? 'Seeding…' : 'Seed'}
    </button>
    <button class="run ghost" type="button" onclick={captureExampleError} disabled={running || disabled}>
      Capture example error
    </button>
  </div>

  {#if running && progress}
    <div class="progress" role="progressbar" aria-valuenow={pct} aria-valuemin="0" aria-valuemax="100">
      <div class="bar" style="width:{pct}%"></div>
    </div>
    <p class="prog-text mono">
      {progress.done} / {progress.total} visitors · {progress.errors} errors · {progress.events} events
    </p>
  {/if}

  {#if summary && !running}
    <div class="results">
      <div class="stats">
        <div class="stat"><span class="n mono">{summary.errors}</span><span class="l">errors</span></div>
        <div class="stat"><span class="n mono">{summary.issues}</span><span class="l">issues</span></div>
        <div class="stat"><span class="n mono">{summary.events}</span><span class="l">events</span></div>
        <div class="stat"><span class="n mono">{summary.taggedErrors}</span><span class="l">tagged</span></div>
        <div class="stat"><span class="n mono">{summary.bigPayloads}</span><span class="l">big payloads</span></div>
      </div>

      <div class="levels" aria-label="Errors by level">
        {#each LEVELS as lvl (lvl)}
          {#if summary.levels[lvl] > 0}
            <span class="lvl">
              <span class="swatch" style="background:{LEVEL_COLOR[lvl]}"></span>
              {lvl}
              <span class="mono">{summary.levels[lvl]}</span>
            </span>
          {/if}
        {/each}
      </div>

      <p class="hint">
        Open the dashboard → Web Demo → <strong>Issues / Events / Users</strong>. Errors are grouped by
        fingerprint, so a re-run grows the existing issues' counts.
      </p>
    </div>
  {/if}
</section>

<style>
  .seeding {
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
  .desc code {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--text);
    background: var(--surface-3);
    padding: 1px 5px;
    border-radius: var(--radius-sm);
  }

  .controls {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }
  .presets {
    display: inline-flex;
    gap: 6px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    padding: 4px;
  }
  .preset {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    border-radius: var(--radius-sm);
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-muted);
    background: transparent;
    border: 1px solid transparent;
    transition: all 0.13s ease;
  }
  .preset:hover:not(:disabled):not(.active) {
    color: var(--text);
  }
  .preset.active {
    color: var(--text);
    background: var(--surface);
    border-color: var(--border-strong);
    box-shadow: var(--shadow-sm);
  }
  .preset .count {
    font-size: 10.5px;
    color: var(--text-faint);
    font-weight: 500;
  }
  .preset:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .run {
    padding: 8px 20px;
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
  .run.ghost {
    color: var(--text);
    background: var(--surface-2);
    border-color: var(--border-strong);
  }
  .run.ghost:hover:not(:disabled) {
    filter: none;
    background: var(--surface-3);
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
  .stats {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
  }
  .stat {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 14px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    min-width: 74px;
  }
  .stat .n {
    font-size: 17px;
    font-weight: 700;
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
  .stat .l {
    font-size: 10.5px;
    color: var(--text-faint);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .levels {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
  }
  .lvl {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11.5px;
    color: var(--text-muted);
    text-transform: capitalize;
  }
  .lvl .swatch {
    width: 9px;
    height: 9px;
    border-radius: 2px;
    flex: none;
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
</style>
