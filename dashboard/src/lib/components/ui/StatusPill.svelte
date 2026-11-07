<script lang="ts">
  import { t } from '../../i18n';
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
  // `$derived` so a language switch reaches the pill; the map stays exhaustive
  // over `MonitorStatus` so a new status cannot ship without a label.
  const label: Record<MonitorStatus, string> = $derived({
    up: t('monitor.status.up'),
    down: t('monitor.status.down'),
    paused: t('monitor.status.paused'),
    unknown: t('monitor.status.unknown'),
  });
  // Also `$derived`: reading a derived value into a plain const captures its
  // first evaluation, which would pin the pill to the language it mounted in.
  const labelFor: Readonly<Record<string, string | undefined>> = $derived(label);
</script>

<span class="pill {status}">{labelFor[status] ?? status}</span>

<style>
  /* The neutral colours here are the FALLBACK for an unrecognised status, not
     decoration: only .up/.down/.paused/.unknown are styled below, so without
     them a state this component does not know renders as an empty outline and
     reads as a broken row. Those four are single-class selectors declared
     after this rule, so they override it. */
  .pill {
    display: inline-flex;
    align-items: center;
    height: 20px;
    padding: 0 7px;
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    color: var(--text-muted);
    background: var(--surface-2);
    white-space: nowrap;
  }
  .up {
    color: var(--success);
    background: var(--success-soft);
  }
  .down {
    color: var(--error);
    background: var(--error-soft);
  }
  .paused {
    color: var(--text-muted);
    background: var(--surface-2);
  }
  .unknown {
    color: var(--text-faint);
    background: var(--surface-2);
  }
</style>
