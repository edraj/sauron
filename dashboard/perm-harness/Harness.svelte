<!--
  Verifies what no static gate can see: that a locked control is FOCUSABLE and
  hands the reason to a keyboard user, that its click really is suppressed, and
  that a locked nav item renders at all.

  Three grant sets side by side, because the interesting cases differ per role:
  the sidebar only locks for a custom role, while the admin rail locks for every
  preset below Owner.
-->
<script lang="ts">
  import Sidebar from '../src/lib/components/layout/Sidebar.svelte';
  import Button from '../src/lib/components/ui/Button.svelte';
  import { adminNavLocks } from '../src/lib/models/admin-nav';
  import { lockTip } from '../src/lib/actions/lock-tip';
  import Icon from '../src/lib/components/ui/Icon.svelte';
  import { sessionStore } from '../src/lib/stores/session.svelte';
  import type { Permission } from '../src/lib/models';

  const VIEWER: Permission[] = [
    'issue:read', 'event:read', 'monitor:read', 'app:read',
    'env:read', 'project:read', 'member:read',
  ];
  // Deliberately missing monitor:read and event:read so the SIDEBAR locks too —
  // no preset role produces that, which is exactly the point: custom roles can.
  const NARROW: Permission[] = ['issue:read', 'app:read', 'member:read'];

  let perms = $state<Permission[]>(VIEWER);

  // The store is seeded directly rather than through load(): this harness
  // verifies rendering, and a real bootstrap would need the whole API.
  $effect(() => {
    sessionStore.loaded = true;
    sessionStore.currentOrgId = 'org1';
    sessionStore.currentProjectId = 'proj1';
    sessionStore.currentAppId = 'app1';
    sessionStore.currentEnvId = null;
    sessionStore.access = {
      permissions: perms,
      grants: [{ scope_type: 'org', scope_id: 'org1', permissions: perms }],
    };
  });

  let clicks = $state(0);
  let submits = $state(0);
  const rail = $derived(adminNavLocks());
</script>

<div class="wrap">
  <Sidebar />
  <main>
    <h1>Locked nav &amp; locked actions</h1>

    <div class="row">
      <button id="set-viewer" onclick={() => (perms = VIEWER)}>Viewer grants</button>
      <button id="set-narrow" onclick={() => (perms = NARROW)}>Narrow custom role</button>
      <span id="perm-count">{perms.length} permissions</span>
    </div>

    <h2>Buttons</h2>
    <div class="row">
      <Button id="unlocked" variant="primary" onclick={() => clicks++}>Unlocked action</Button>
      <Button variant="primary" lockedReason={'org:manage'} onclick={() => clicks++}>
        Locked action
      </Button>
      <Button variant="secondary" disabled onclick={() => clicks++}>Plain disabled</Button>
      <span id="click-count">clicks: {clicks}</span>
    </div>

    <h2>Raw locked control (the 13-call-site class)</h2>
    <div class="row">
      <button id="raw-locked" type="button" use:lockTip={'member:manage'} onclick={() => clicks++}>
        Raw button, locked
      </button>
      <button id="raw-open" type="button" onclick={() => clicks++}>Raw button, open</button>
    </div>

    <h2>Form submission</h2>
    <!-- The case `aria-disabled` does NOT cover on its own: a locked
         `type="submit"` still submits its form unless the click's default
         action is prevented. Enter on a focused submit button fires a click
         too, so preventing it there covers the keyboard path as well. -->
    <form id="lock-form" onsubmit={(e) => { e.preventDefault(); submits++; }}>
      <Button type="submit" variant="primary" lockedReason={'org:manage'}>Locked submit</Button>
      <Button type="submit" variant="secondary">Unlocked submit</Button>
      <span id="submit-count">submits: {submits}</span>
    </form>

    <h2>Admin rail ({rail.filter((i) => i.locked).length} of {rail.length} locked)</h2>
    <nav class="rail">
      {#each rail as item (item.href)}
        {#if item.locked}
          <button type="button" class="item locked" use:lockTip={item.locked}>
            <Icon name={item.icon} size={15} />
            <span>{item.label}</span>
            <span class="lk" aria-hidden="true"><Icon name="lock" size={12} /></span>
          </button>
        {:else}
          <a href={`#${item.href}`} class="item"><Icon name={item.icon} size={15} /><span>{item.label}</span></a>
        {/if}
      {/each}
    </nav>
  </main>
</div>

<style>
  /* Sidebar.svelte positions itself with `grid-area: sidebar`, so the harness
     has to declare that area or the rail is auto-placed off in a phantom row —
     which put it at top 1122 / left 1042 in a 720px viewport and made every
     tooltip correctly decline to open for an offscreen trigger. */
  .wrap {
    display: grid;
    grid-template-columns: var(--sidebar-w, 240px) minmax(0, 1fr);
    grid-template-areas: 'sidebar main';
    min-height: 100vh;
  }
  main { grid-area: main; padding: 24px 28px; }
  h1 { font-size: 19px; margin: 0 0 18px; }
  h2 { font-size: 14px; margin: 26px 0 10px; color: var(--text-muted); }
  .row { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; margin-bottom: 8px; }
  .rail { display: flex; flex-direction: column; gap: 2px; width: 220px; }
  .item {
    display: flex; align-items: center; gap: 9px; padding: 7px 10px;
    border-radius: var(--radius); font-size: 13px; color: var(--text-muted); text-decoration: none;
  }
  .item.locked {
    width: 100%; border: 0; background: none; font: inherit; font-size: 13px;
    text-align: start; cursor: not-allowed; opacity: 0.5;
  }
  .item.locked:focus-visible { outline: 2px solid var(--primary); outline-offset: -2px; opacity: 0.75; }
  .lk { margin-inline-start: auto; display: grid; place-items: center; }
</style>
