<script lang="ts">
  import Icon from '../ui/Icon.svelte';
  import SearchInput from '../SearchInput.svelte';
  import {
    groupState,
    inCatalogOrder,
    matchesQuery,
    receiveSelection,
    type GroupState,
  } from '../../models/permission-picker';
  import { PERMISSION_GROUPS, PERMISSION_LABELS } from '../../models/permissions';
  import type { Permission } from '../../models';

  interface Props {
    selected: Permission[];
    disabled?: boolean;
    onchange: (next: Permission[]) => void;
  }

  let { selected, disabled = false, onchange }: Props = $props();

  /** Derived so it can never drift from the catalog — never hardcode 30. */
  const TOTAL = PERMISSION_GROUPS.flatMap((g) => g.permissions).length;

  const selectedSet = $derived(new Set(selected));

  let query = $state('');
  const searching = $derived(query.trim() !== '');
  const anyMatch = $derived(
    PERMISSION_GROUPS.some((g) => g.permissions.some((p) => matchesQuery(p, query))),
  );

  function computeOpenGroups(sel: Set<Permission>): Record<string, boolean> {
    const next: Record<string, boolean> = {};
    for (const g of PERMISSION_GROUPS) {
      next[g.label] = g.permissions.some((p) => sel.has(p));
    }
    return next;
  }

  /**
   * Per-group disclosure state, keyed by group label. This is the one real,
   * user-controlled toggle — deliberately its own `$state` rather than a
   * `$derived` of `selected`. If it were derived, unticking a group's last
   * checkbox would collapse the group the admin is mid-click in. Starts empty
   * (everything collapsed) and is populated by the mount run of the effect
   * below — reading `selected` here directly would only capture its initial
   * value (Svelte flags exactly that as `state_referenced_locally`) without
   * the effect's later re-runs on a genuine role switch.
   */
  let openGroups = $state<Record<string, boolean>>({});

  // Snapshot of openGroups taken the instant search starts, restored verbatim
  // the instant it ends, so typing into the search box never leaves a lasting
  // mark on the layout the admin had before.
  let preSearchOpen: Record<string, boolean> | null = null;
  let searchWasEmpty = true;

  // The array we last handed to `onchange` and are still expecting to see
  // echoed back as `selected`. Plain `let`, not `$state`: the effect below
  // must read it without taking a reactive dependency on it. See
  // `receiveSelection` for why content comparison — and consuming this
  // baseline exactly once — is what tells our own echo apart from the dialog
  // opening on a different role.
  let pendingEmit: Permission[] | null = null;

  // Recompute collapse state whenever `selected` is replaced wholesale — the
  // dialog opening on a different role — but never on the echo of our own
  // onchange. This effect only ever reads `selected`, so it cannot loop on
  // its own writes to `openGroups`.
  $effect(() => {
    const incoming = [...selected];
    const receipt = receiveSelection(pendingEmit, incoming);
    pendingEmit = receipt.pendingEmit;
    if (!receipt.recompute) return;
    openGroups = computeOpenGroups(new Set(incoming));
    // A freshly opened role starts with a clean search box, not whatever the
    // admin was typing while looking at the previous one.
    query = '';
    preSearchOpen = null;
    searchWasEmpty = true;
  });

  function emit(next: Set<Permission>) {
    // Catalog order so a role's stored array is stable regardless of the
    // order the boxes were clicked in.
    const ordered = inCatalogOrder(next);
    pendingEmit = ordered;
    onchange(ordered);
  }

  function toggle(permission: Permission) {
    if (disabled) return;
    const next = new Set(selectedSet);
    if (next.has(permission)) next.delete(permission);
    else next.add(permission);
    emit(next);
  }

  /**
   * The per-section select-all: acts only on whatever is currently visible —
   * every permission in the group normally, or just the search-matched ones
   * while filtering — so ticking it while a query narrows the list can never
   * silently touch a permission the admin cannot currently see. When the
   * query is empty `matchesQuery` matches everything, so "visible" and "the
   * whole group" are the same list and this needs no special-casing.
   */
  function setGroupChecked(visible: Permission[], checked: boolean) {
    if (disabled) return;
    const next = new Set(selectedSet);
    for (const p of visible) {
      if (checked) next.add(p);
      else next.delete(p);
    }
    emit(next);
  }

  function isGroupOpen(label: string): boolean {
    // Only ever consulted for a group that already passed the search filter,
    // so "searching" alone is enough to mean "auto-expanded".
    if (searching) return true;
    return openGroups[label] ?? false;
  }

  function toggleGroupOpen(label: string) {
    if (searching) return; // fully determined by the query while one is active
    openGroups = { ...openGroups, [label]: !(openGroups[label] ?? false) };
  }

  function handleSearchInput(value: string) {
    const isEmpty = value.trim() === '';
    if (searchWasEmpty && !isEmpty) {
      preSearchOpen = { ...openGroups };
    } else if (!searchWasEmpty && isEmpty && preSearchOpen) {
      openGroups = preSearchOpen;
      preSearchOpen = null;
    }
    searchWasEmpty = isEmpty;
  }

  interface CheckState {
    checked: boolean;
    indeterminate: boolean;
  }

  /**
   * Drives a checkbox entirely from computed state. Both properties are set
   * imperatively because `indeterminate` has no HTML attribute at all, so
   * templating it cannot be trusted to render the tri-state dash.
   *
   * This is only safe as the SOLE writer of the two properties — see
   * `onGroupClick` for why the native toggle has to be cancelled.
   */
  function syncCheck(node: HTMLInputElement, state: CheckState) {
    const apply = (s: CheckState) => {
      node.checked = s.checked;
      node.indeterminate = s.indeterminate;
    };
    apply(state);
    return { update: apply };
  }

  /**
   * Handles the per-group select-all on `click`, cancelling the browser's own
   * toggle rather than reacting to `change`.
   *
   * A click flips `checked` (and clears `indeterminate`) natively and
   * immediately. When the resulting computed state is UNCHANGED, Svelte sees
   * no change, skips the DOM write, and the browser's optimistic tick stays on
   * screen contradicting the real state. That is not hypothetical: with a
   * search filter active, ticking every shown permission can leave the group
   * still only partly selected — 'some' before, 'some' after — which rendered
   * a fully-checked header for a 3-of-5 group.
   *
   * Cancelling the activation is what closes it: the HTML spec's
   * "legacy-canceled-activation behavior" restores both checkedness and
   * indeterminate to their pre-click values, so the DOM can never drift from
   * `syncCheck`. Space-bar activation dispatches a click too, so the keyboard
   * path is covered by the same handler.
   */
  function onGroupClick(event: Event, visible: Permission[], shownState: GroupState) {
    event.preventDefault();
    setGroupChecked(visible, shownState !== 'all');
  }
</script>

<div class="permission-picker" class:disabled>
  <div class="toolbar">
    <SearchInput
      bind:value={query}
      oninput={handleSearchInput}
      placeholder="Search permissions…"
    />
    <span class="count muted">{selectedSet.size} of {TOTAL} selected</span>
  </div>

  <div class="groups">
    {#each PERMISSION_GROUPS as group (group.label)}
      {@const matches = group.permissions.filter((p) => matchesQuery(p, query))}
      {#if matches.length > 0}
        {@const open = isGroupOpen(group.label)}
        {@const filtered = matches.length < group.permissions.length}
        <!-- Two states, deliberately. The checkbox must describe the GROUP, or
             a search that happens to match only the selected permissions
             renders the header fully checked while unselected ones sit hidden
             behind the filter (real case: a role holding the three `member:*`
             permissions, searched for "member", is genuinely 3 of 5). The
             click direction is decided by the VISIBLE subset instead, so
             "select all" never silently reaches a permission off screen. With
             no query the two lists are identical and these coincide. -->
        {@const state = groupState(group.permissions, selectedSet)}
        {@const shownState = groupState(matches, selectedSet)}
        {@const groupChecked = group.permissions.filter((p) => selectedSet.has(p)).length}
        {@const shownChecked = matches.filter((p) => selectedSet.has(p)).length}
        <div class="group">
          <div class="row">
            <button
              type="button"
              class="twisty"
              aria-expanded={open}
              aria-label={`${open ? 'Collapse' : 'Expand'} ${group.label}`}
              disabled={searching}
              onclick={() => toggleGroupOpen(group.label)}
            >
              <Icon name={open ? 'chevron-down' : 'chevron-right'} size={13} />
            </button>
            <label
              class="node"
              title={filtered
                ? `Applies to the ${matches.length} shown permission${matches.length === 1 ? '' : 's'} only`
                : undefined}
            >
              <input
                type="checkbox"
                use:syncCheck={{ checked: state === 'all', indeterminate: state === 'some' }}
                {disabled}
                onclick={(e) => onGroupClick(e, matches, shownState)}
              />
              <span class="g-name">{group.label}</span>
              <span class="g-hint">
                {#if filtered}
                  {shownChecked} of {matches.length} shown · {groupChecked} of {group.permissions
                    .length} total
                {:else}
                  {groupChecked}/{group.permissions.length}
                {/if}
              </span>
            </label>
          </div>

          {#if open}
            <div class="permissions">
              {#each matches as permission (permission)}
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
            </div>
          {/if}
        </div>
      {/if}
    {/each}

    {#if searching && !anyMatch}
      <p class="empty">No permissions match "{query.trim()}".</p>
    {/if}
  </div>
</div>

<style>
  .permission-picker {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-width: 0;
  }
  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }
  .count {
    font-size: 12px;
    white-space: nowrap;
  }
  .groups {
    display: flex;
    flex-direction: column;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    padding: 4px 10px;
    max-height: 320px;
    overflow-y: auto;
  }
  .group {
    border-bottom: 1px solid var(--border);
  }
  .group:last-child {
    border-bottom: none;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 4px;
    min-height: 32px;
  }
  .twisty {
    display: inline-grid;
    place-items: center;
    width: 20px;
    height: 20px;
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    flex-shrink: 0;
  }
  .twisty:hover:not(:disabled) {
    color: var(--text);
  }
  .twisty:disabled {
    cursor: default;
    opacity: 0.4;
  }
  .node {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    flex: 1;
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
    cursor: pointer;
    padding: 7px 0;
  }
  .node:has(input:disabled) {
    cursor: default;
  }
  .node input {
    accent-color: var(--primary);
    flex-shrink: 0;
  }
  .g-hint {
    font-size: 11.5px;
    font-weight: 500;
    color: var(--text-faint);
    margin-left: auto;
    padding-right: 4px;
    white-space: nowrap;
  }
  .permissions {
    display: flex;
    flex-direction: column;
    gap: 7px;
    padding: 2px 0 10px 28px;
  }
  .permission {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 12.5px;
    color: var(--text-muted);
    cursor: pointer;
  }
  .permission:has(input:disabled) {
    cursor: default;
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
  .empty {
    font-size: 12.5px;
    color: var(--text-faint);
    padding: 10px 2px;
  }
  .permission-picker.disabled .permission {
    opacity: 0.75;
  }
</style>
