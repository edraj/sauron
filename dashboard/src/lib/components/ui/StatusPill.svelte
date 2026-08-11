<script lang="ts">
  import type { MonitorStatus } from '../../models';

  /**
   * `status` is widened beyond `MonitorStatus` on purpose: the wire carries a
   * plain string and the backend can add a state before this union learns
   * about it. `MonitorStatus` is kept in the type so call sites still get
   * autocomplete and the four known values still read as the expected set.
   */
  let { status }: { status: MonitorStatus | (string & {}) } = $props();

  /**
   * Stays keyed on `MonitorStatus` so adding to the union is a type error
   * until a label is supplied — that exhaustiveness is worth keeping.
   * `labelFor` is the widened view used for the actual lookup, so an
   * unrecognised status falls through to its raw value rather than
   * disappearing.
   */
  const label: Record<MonitorStatus, string> = {
    up: 'Up', down: 'Down', paused: 'Paused', unknown: 'Pending',
  };
  const labelFor: Readonly<Record<string, string | undefined>> = label;
</script>

<span class="pill {status}">{labelFor[status] ?? status}</span>

<style>
  /* The neutral colours here are the FALLBACK for an unrecognised status, not
     decoration: only .up/.down/.paused/.unknown are styled below, so without
     them a state this component does not know renders as an empty outline and
     reads as a broken row. Those four are single-class selectors declared
     after this rule, so they override it. */
  .pill { display: inline-flex; align-items: center; padding: 2px 9px; border-radius: 999px;
    font-size: 12px; font-weight: 600; border: 1px solid var(--border);
    color: var(--text-muted); background: var(--surface-2); }
  .up { color: #16794a; background: color-mix(in srgb, #16794a 12%, transparent); border-color: color-mix(in srgb, #16794a 40%, transparent); }
  .down { color: #b42318; background: color-mix(in srgb, #b42318 12%, transparent); border-color: color-mix(in srgb, #b42318 40%, transparent); }
  .paused { color: var(--text-muted); background: var(--surface-2); }
  .unknown { color: var(--text-faint); background: var(--surface-2); }
</style>
