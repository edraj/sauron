<script lang="ts">
  import { t } from '../i18n';
  import Badge from './ui/Badge.svelte';
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
      ? t('symbolication.symbolicated')
      : status === 'partial'
        ? t('symbolication.partial')
        : status === 'no_artifacts'
          ? isDart
            ? t('symbolication.noSymbols')
            : t('symbolication.noSourceMaps')
          : status === 'pending'
            ? t('symbolication.pending')
            : t('symbolication.notApplicable'),
  );

  // Colour only where it means something: resolved is green, partly resolved
  // amber; every other state is a neutral fact.
  const tone = $derived(
    status === 'symbolicated' ? 'success' : status === 'partial' ? 'warning' : 'neutral',
  );

  // Only the dead-end state earns a tooltip: it is the one status the reader
  // can do something about.
  const hint = $derived(
    status === 'no_artifacts'
      ? isDart
        ? t('symbolication.hintSymbols')
        : t('symbolication.hintSourceMaps')
      : undefined,
  );
</script>

{#if status}
  <span class="sym" class:help={!!hint} title={hint}>
    <Badge {tone} size="sm">{label}</Badge>
  </span>
{/if}

<style>
  .sym {
    display: inline-flex;
    flex-shrink: 0;
  }
  .sym.help {
    cursor: help;
  }
</style>
