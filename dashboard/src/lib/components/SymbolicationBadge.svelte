<script lang="ts">
  import type { SymbolicationStatus } from '../models';

  interface Props {
    status?: SymbolicationStatus | null;
    /**
     * A Dart (Flutter AOT) event, which the wording follows: those resolve
     * against uploaded *debug symbols*, not source maps, so telling their
     * reader to upload a source map sends them to the wrong page.
     *
     * Callers derive it the one way it can be known on the wire — a
     * `debug_meta.raw_stacktrace` is only ever attached by the Flutter SDK.
     */
    isDart?: boolean;
  }

  let { status = null, isDart = false }: Props = $props();

  const label = $derived(
    status === 'symbolicated'
      ? 'Symbolicated'
      : status === 'partial'
        ? 'Partially symbolicated'
        : status === 'no_artifacts'
          ? isDart
            ? 'No symbols'
            : 'No source maps'
          : status === 'pending'
            ? 'Pending'
            : 'Not applicable',
  );

  // Only the dead-end state earns a tooltip: it is the one status the reader
  // can do something about.
  const hint = $derived(
    status === 'no_artifacts'
      ? `Upload ${isDart ? 'debug symbols' : 'source maps'} for this release to see original frames`
      : '',
  );
</script>

{#if status}
  <span
    class="sym-badge"
    class:ok={status === 'symbolicated'}
    class:partial={status === 'partial'}
    class:none={status === 'no_artifacts'}
    title={hint}
  >
    {label}
  </span>
{/if}

<style>
  .sym-badge {
    font-size: 10px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 2px 7px;
    border-radius: var(--radius-pill);
    color: var(--text-muted);
    background: var(--surface-2, var(--surface));
    border: 1px solid var(--border);
    white-space: nowrap;
  }
  .sym-badge.ok {
    color: var(--success, #30a46c);
    background: color-mix(in srgb, var(--success, #30a46c) 14%, transparent);
    border-color: transparent;
  }
  .sym-badge.partial {
    color: var(--warning, #f5a623);
    background: color-mix(in srgb, var(--warning, #f5a623) 16%, transparent);
    border-color: transparent;
  }
  .sym-badge.none {
    cursor: help;
  }
</style>
