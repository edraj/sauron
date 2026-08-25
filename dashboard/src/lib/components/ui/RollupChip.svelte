<script lang="ts">
  // "as of HH:mm:ss" freshness chip for rollup-served analytics pages. Owns
  // the /rollups/status poll (on scope change + every 60 s) and publishes
  // readiness to rollupState so pages can ≈-mark their sketch-derived
  // figures. All decisions live in models/freshness.ts (pure, tested).
  import Badge from './Badge.svelte';
  import { getRollupStatus } from '../../api/rollups';
  import { rollupChip } from '../../models/freshness';
  import { rollupState } from '../../stores/rollups.svelte';
  import { sessionStore } from '../../stores/session.svelte';
  import type { RollupStatus } from '../../models';

  let status = $state<RollupStatus | null>(null);

  $effect(() => {
    const aid = sessionStore.currentAppId;
    void sessionStore.scopeKey;
    if (!aid) {
      status = null;
      rollupState.ready = false;
      return;
    }
    let alive = true;
    const fetchNow = () =>
      getRollupStatus(aid)
        .then((s) => {
          if (!alive) return;
          status = s;
          rollupState.ready = s.ready;
        })
        .catch(() => {
          // A 404 here just means an older API without rollups: no chip, no ≈.
          if (!alive) return;
          status = null;
          rollupState.ready = false;
        });
    fetchNow();
    const iv = setInterval(fetchNow, 60_000);
    return () => {
      alive = false;
      clearInterval(iv);
    };
  });

  const view = $derived(rollupChip(status));
</script>

{#if view}
  <span title={view.title}>
    <Badge tone={view.tone} size="sm">{view.label}</Badge>
  </span>
{/if}
