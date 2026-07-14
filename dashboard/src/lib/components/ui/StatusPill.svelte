<script lang="ts">
  import type { MonitorStatus } from '../../models';
  let { status }: { status: MonitorStatus } = $props();
  const label: Record<MonitorStatus, string> = {
    up: 'Up', down: 'Down', paused: 'Paused', unknown: 'Pending',
  };
</script>

<span class="pill {status}">{label[status]}</span>

<style>
  .pill { display: inline-flex; align-items: center; padding: 2px 9px; border-radius: 999px;
    font-size: 12px; font-weight: 600; border: 1px solid var(--border); }
  .up { color: #16794a; background: color-mix(in srgb, #16794a 12%, transparent); border-color: color-mix(in srgb, #16794a 40%, transparent); }
  .down { color: #b42318; background: color-mix(in srgb, #b42318 12%, transparent); border-color: color-mix(in srgb, #b42318 40%, transparent); }
  .paused { color: var(--text-muted); background: var(--surface-2); }
  .unknown { color: var(--text-faint); background: var(--surface-2); }
</style>
