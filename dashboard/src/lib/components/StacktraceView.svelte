<script lang="ts">
  import { t } from '../i18n';
  import type { Frame, SymbolicatedFrame } from '../models';

  interface Props {
    frames: Frame[];
    symbolicated?: SymbolicatedFrame[] | null;
    // Verbatim obfuscated Dart trace, shown when there are no resolvable frames.
    rawTrace?: string | null;
  }

  let { frames, symbolicated = null, rawTrace = null }: Props = $props();

  const hasSymbolicated = $derived(
    Array.isArray(symbolicated) && symbolicated.some((f) => f.symbolicated),
  );

  // When symbolicated frames exist, show them by default; let the user flip to raw.
  let showRaw = $state(false);

  const active = $derived(
    hasSymbolicated && !showRaw ? (symbolicated as SymbolicatedFrame[]) : frames,
  );
  // Sentry convention: most recent call is shown first.
  const ordered = $derived([...active].reverse());

  // Long traces are mostly framework noise in the middle. Pin the 5 most recent
  // frames, the 5 deepest, and every in-app frame wherever it sits; whatever is
  // left forms contiguous runs that can fold away.
  const EDGE = 5;
  // A run of 1-2 frames isn't worth folding -- the toggle row would take as much
  // room as the frames it hides. This threshold is also why short traces never
  // collapse: 12 frames leave at most 2 unpinned, so 13 is the first foldable
  // length. There is deliberately no separate length constant to keep in sync.
  const MIN_RUN = 3;

  interface Chunk {
    // Run start index, used as the expansion key; -1 for always-visible chunks.
    key: number;
    frames: Frame[];
    collapsible: boolean;
  }

  // Group `ordered` into alternating visible / collapsible chunks.
  const chunks = $derived.by((): Chunk[] => {
    const len = ordered.length;
    const out: Chunk[] = [];
    let run: Frame[] = [];
    let runStart = 0;
    let visible: Frame[] = [];

    const flushRun = () => {
      if (run.length === 0) return;
      if (run.length >= MIN_RUN) {
        if (visible.length > 0) {
          out.push({ key: -1, frames: visible, collapsible: false });
          visible = [];
        }
        out.push({ key: runStart, frames: run, collapsible: true });
      } else {
        // Too short to fold -- fold it back into the visible neighbours.
        visible.push(...run);
      }
      run = [];
    };

    for (let i = 0; i < len; i++) {
      const frame = ordered[i];
      const pinned = i < EDGE || i >= len - EDGE || frame.in_app === true;
      if (pinned) {
        flushRun();
        visible.push(frame);
      } else {
        if (run.length === 0) runStart = i;
        run.push(frame);
      }
    }
    flushRun();
    if (visible.length > 0) out.push({ key: -1, frames: visible, collapsible: false });
    return out;
  });

  // Expansion is per run, not page-wide: in-app frames interleaved with vendor
  // frames yield several independent runs, and opening one must not open the rest.
  // Open runs are stored alongside the frame array they were computed against.
  // Run boundaries differ between symbolicated and minified frames, so keys from
  // one would silently mean a different run in the other; tying them to `src`
  // makes the state self-discarding on a `showRaw` flip or a fresh event, with no
  // reset effect to keep in sync.
  // `$state.raw`, not `$state`: plain `$state` deep-proxies what it stores, so
  // `open.src` would read back a *proxy* of the frame array and never `===` the
  // raw `active` we compare it against, permanently wedging every run shut.
  let open = $state.raw<{ src: Frame[] | null; keys: number[] }>({ src: null, keys: [] });
  const expanded = $derived(open.src === active ? open.keys : []);

  function toggle(key: number) {
    const keys = expanded.includes(key)
      ? expanded.filter((k) => k !== key)
      : [...expanded, key];
    open = { src: active, keys };
  }

  function loc(frame: Frame): string {
    const file = frame.filename ?? frame.module ?? frame.abs_path ?? '<unknown>';
    if (frame.lineno != null) {
      return frame.colno != null
        ? `${file}:${frame.lineno}:${frame.colno}`
        : `${file}:${frame.lineno}`;
    }
    return file;
  }

  function ctx(frame: Frame): SymbolicatedFrame | null {
    const f = frame as SymbolicatedFrame;
    return f.context_line != null || (f.pre_context?.length ?? 0) > 0 ? f : null;
  }
</script>

{#if ordered.length === 0 && rawTrace}
  <pre class="raw-trace mono">{rawTrace}</pre>
{:else if ordered.length === 0}
  <p class="muted empty">{t('ui.empty.stacktrace')}</p>
{:else}
  {#if hasSymbolicated}
    <div class="toolbar">
      <button
        type="button"
        class="toggle"
        onclick={() => (showRaw = !showRaw)}
        aria-pressed={showRaw}
      >
        {showRaw ? 'Show original' : 'Show minified'}
      </button>
    </div>
  {/if}
  <div class="trace">
    {#each chunks as chunk, ci (ci)}
      {@const isOpen = expanded.includes(chunk.key)}
      {#if chunk.collapsible}
        <button
          type="button"
          class="fold"
          onclick={() => toggle(chunk.key)}
          aria-expanded={isOpen}
        >
          <span class="dots" aria-hidden="true">⋯</span>
          {isOpen ? 'Hide' : 'Show'}
          {chunk.frames.length} frame{chunk.frames.length === 1 ? '' : 's'}
        </button>
      {/if}
      {#if !chunk.collapsible || isOpen}
        {#each chunk.frames as frame, i (i)}
          {@const c = ctx(frame)}
          <div class="frame" class:in-app={frame.in_app}>
            <div class="marker" aria-hidden="true"></div>
            <div class="body">
              <div class="fn-line">
                <span class="fn">{frame.function ?? '<anonymous>'}</span>
                {#if frame.in_app}<span class="chip">in app</span>{/if}
              </div>
              <div class="loc mono">{loc(frame)}</div>
              {#if c}
                <div class="context mono">
                  {#each c.pre_context ?? [] as line, j (j)}
                    <div class="ctx-line">
                      <span class="ln">{(c.context_start_line ?? 1) + j}</span><span class="src">{line}</span>
                    </div>
                  {/each}
                  <div class="ctx-line crash">
                    <span class="ln">{frame.lineno ?? ''}</span><span class="src">{c.context_line}</span>
                  </div>
                  {#each c.post_context ?? [] as line, j (j)}
                    <div class="ctx-line">
                      <span class="ln">{(frame.lineno ?? 0) + j + 1}</span><span class="src">{line}</span>
                    </div>
                  {/each}
                </div>
              {/if}
            </div>
          </div>
        {/each}
      {/if}
    {/each}
  </div>
{/if}

<style>
  .empty {
    padding: 8px 2px;
    font-size: 13px;
  }
  .raw-trace {
    margin: 0;
    padding: 12px 14px;
    font-size: 12px;
    line-height: 1.5;
    color: var(--text-muted);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow-x: auto;
    white-space: pre;
  }
  .toolbar {
    display: flex;
    justify-content: flex-end;
    margin-bottom: 8px;
  }
  .toggle {
    font-size: 12px;
    font-weight: 600;
    color: var(--primary);
    background: var(--primary-soft);
    border: 1px solid var(--border);
    padding: 4px 10px;
    border-radius: var(--radius-pill);
    cursor: pointer;
  }
  .toggle:hover {
    background: color-mix(in srgb, var(--primary-soft) 70%, var(--primary));
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
  /* Sits in the frame stack, so it inherits the same rhythm and dividers. The
     global reset only sets font/cursor -- an unstyled button renders gray here. */
  .fold {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 8px 14px;
    font-size: 12px;
    font-weight: 600;
    text-align: start;
    color: var(--text-muted);
    background: var(--surface-2);
    border: none;
    border-bottom: 1px solid var(--border);
    cursor: pointer;
  }
  .fold:last-child {
    border-bottom: none;
  }
  .fold:hover {
    color: var(--text);
    background: color-mix(in srgb, var(--primary-soft) 45%, var(--surface-2));
  }
  .fold .dots {
    font-size: 14px;
    line-height: 1;
    color: var(--text-faint);
    letter-spacing: 0.08em;
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
    flex: 1;
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
  .context {
    margin-top: 6px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow-x: auto;
    background: var(--bg);
  }
  .ctx-line {
    display: flex;
    font-size: 12px;
    white-space: pre;
    line-height: 1.5;
  }
  .ctx-line .ln {
    flex: 0 0 auto;
    width: 42px;
    text-align: end;
    padding-inline-end: 10px;
    color: var(--text-faint);
    user-select: none;
    border-inline-end: 1px solid var(--border);
  }
  .ctx-line .src {
    padding-inline-start: 10px;
    color: var(--text-muted);
  }
  .ctx-line.crash {
    background: color-mix(in srgb, var(--danger, #e5484d) 16%, transparent);
  }
  .ctx-line.crash .src {
    color: var(--text);
    font-weight: 600;
  }
</style>
