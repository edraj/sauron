<script lang="ts">
  import { PERMISSION_GROUPS, PERMISSION_LABELS } from '../../models/permissions';
  import type { Permission } from '../../models';

  interface Props {
    selected: Permission[];
    disabled?: boolean;
    onchange: (next: Permission[]) => void;
  }

  let { selected, disabled = false, onchange }: Props = $props();

  const selectedSet = $derived(new Set(selected));

  function toggle(permission: Permission) {
    if (disabled) return;
    const next = new Set(selectedSet);
    if (next.has(permission)) next.delete(permission);
    else next.add(permission);
    // Emit in catalog order so a role's stored array is stable regardless of
    // the order the boxes were clicked in.
    onchange(PERMISSION_GROUPS.flatMap((g) => g.permissions).filter((p) => next.has(p)));
  }
</script>

<div class="permission-picker" class:disabled>
  {#each PERMISSION_GROUPS as group (group.label)}
    <fieldset>
      <legend>{group.label}</legend>
      {#each group.permissions as permission (permission)}
        <label class="permission">
          <input
            type="checkbox"
            checked={selectedSet.has(permission)}
            {disabled}
            onchange={() => toggle(permission)}
          />
          <span class="name mono">{permission}</span>
          <span class="description muted">{PERMISSION_LABELS[permission]}</span>
        </label>
      {/each}
    </fieldset>
  {/each}
</div>

<style>
  .permission-picker {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 14px;
  }
  fieldset {
    display: flex;
    flex-direction: column;
    gap: 6px;
    border: none;
    padding: 0;
    margin: 0;
    min-width: 0;
  }
  legend {
    font-size: 11px;
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
    padding: 0;
    margin-bottom: 2px;
  }
  .permission {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 12.5px;
    color: var(--text-muted);
    cursor: pointer;
  }
  .permission input {
    accent-color: var(--primary);
    margin-top: 2px;
    flex-shrink: 0;
  }
  .name {
    color: var(--text);
    white-space: nowrap;
  }
  .description {
    font-size: 11.5px;
  }
  .permission-picker.disabled .permission {
    cursor: not-allowed;
    opacity: 0.75;
  }
</style>
