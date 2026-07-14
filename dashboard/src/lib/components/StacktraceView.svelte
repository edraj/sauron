<script lang="ts">
  import type { Frame } from '../models';

  interface Props {
    frames: Frame[];
  }

  let { frames }: Props = $props();

  // Sentry convention: most recent call is shown first.
  const ordered = $derived([...frames].reverse());

  function loc(frame: Frame): string {
    const file = frame.filename ?? frame.module ?? frame.abs_path ?? '<unknown>';
    if (frame.lineno != null) {
      return frame.colno != null
        ? `${file}:${frame.lineno}:${frame.colno}`
        : `${file}:${frame.lineno}`;
    }
    return file;
  }
</script>

{#if ordered.length === 0}
  <p class="muted empty">No stacktrace on this event.</p>
{:else}
  <div class="trace">
    {#each ordered as frame, i (i)}
      <div class="frame" class:in-app={frame.in_app}>
        <div class="marker" aria-hidden="true"></div>
        <div class="body">
          <div class="fn-line">
            <span class="fn">{frame.function ?? '<anonymous>'}</span>
            {#if frame.in_app}<span class="chip">in app</span>{/if}
          </div>
          <div class="loc mono">{loc(frame)}</div>
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .empty {
    padding: 8px 2px;
    font-size: 13px;
  }
  .trace {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .frame {
    display: flex;
    gap: 10px;
    padding: 11px 14px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .frame:last-child {
    border-bottom: none;
  }
  .frame.in-app {
    background: color-mix(in srgb, var(--primary-soft) 55%, var(--surface));
  }
  .marker {
    width: 3px;
    border-radius: 2px;
    background: var(--border-strong);
    flex-shrink: 0;
  }
  .frame.in-app .marker {
    background: var(--primary);
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .fn-line {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .fn {
    font-family: var(--font-mono);
    font-size: 13px;
    color: var(--text);
    font-weight: 500;
  }
  .frame.in-app .fn {
    color: var(--text);
  }
  .frame:not(.in-app) .fn {
    color: var(--text-muted);
  }
  .chip {
    font-size: 10px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--primary);
    background: var(--primary-soft);
    padding: 2px 6px;
    border-radius: var(--radius-pill);
  }
  .loc {
    font-size: 12px;
    color: var(--text-faint);
    overflow-x: auto;
    white-space: nowrap;
  }
</style>
