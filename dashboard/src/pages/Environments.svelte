<script lang="ts">
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import CopyButton from '../lib/components/ui/CopyButton.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import Modal from '../lib/components/ui/Modal.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { lockedBy } from '../lib/models/page-access';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { errorMessage, isNormalizedError } from '../lib/api/client';
  import { buildDsn } from '../lib/utils/format';
  import { listApps } from '../lib/api/apps';
  import {
    listProjectEnvironments,
    listEnvironments,
    createProjectEnvironment,
    renameProjectEnvironment,
    retireProjectEnvironment,
    updateAppEnvironment,
    rotateAppEnvironmentKey,
  } from '../lib/api/environments';
  import type { App, AppEnvironment, AppEnvironmentRow, ProjectEnvironment } from '../lib/models';

  // Project-wide counterpart to EnvironmentsCard.svelte (per-app, now removed
  // from SettingsApp — see the task report). Two ids matter, same as there,
  // and must not be confused:
  //
  //   `ProjectEnvironment.id`  the CATALOGUE entry (`environments` table),
  //                            owned by the project. Its NAME, and whether it
  //                            exists at all. Shared by every app in it.
  //   `AppEnvironmentRow.id`   one app's ENROLLMENT in a catalogue entry
  //                            (`app_environments` table). Its key, its mute
  //                            switch, its default flag, its DSN. Scoped to
  //                            that one app alone.
  //
  // So create / rename / retire go to the project catalogue and change what
  // every app in the project sees; mute / promote / rotate go to
  // `/v1/app-environments/{id}` and touch one app's row alone — hence the two
  // scopes below (`level: 'project'` vs `level: 'app'`). Because this page,
  // unlike the card it replaces, spans every app in the project at once, the
  // two app-scoped locks are computed PER ROW in the template rather than
  // hoisted here: hoisting them to one appId would gate one app's controls on
  // a different app's permissions.
  let apps = $state<App[]>([]);
  let catalogue = $state<ProjectEnvironment[]>([]);
  // Enrollment rows for EVERY app in the project, flattened into one list.
  // One entry per (app, environment) pair.
  let enrollments = $state<AppEnvironment[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let showRetired = $state(false);
  // Holds either a catalogue id (rename/retire in flight) or an enrollment id
  // (mute/promote/rotate in flight) — the two id spaces never collide, so one
  // field is enough to disable the one button currently acting.
  let busyId = $state<string | null>(null);

  let creating = $state(false);
  let newName = $state('');
  let createBusy = $state(false);

  let renaming = $state<ProjectEnvironment | null>(null);
  let renameValue = $state('');
  let renameBusy = $state(false);

  let confirmRotate = $state<AppEnvironment | null>(null);
  let confirmRetire = $state<ProjectEnvironment | null>(null);

  // Whether the last `load()` could read the project-wide CATALOGUE
  // (`listProjectEnvironments`). Starts `true` so the ordinary render path is
  // used until `load()` says otherwise, and is set from that call's own
  // response — not inferred from `catalogue.length === 0`, which cannot tell
  // "no environments yet" from "cannot see the catalogue" (the same reasoning
  // `appListComplete` below applies to the app list).
  //
  // `list_project_environments` (environments.rs:196) authorizes at the
  // PROJECT via `authorize_project`, which — unlike the per-app enrollment
  // endpoints this page also calls — is NOT reach-based. This page's
  // PAGE_ACCESS gate is `env:read`@`level:'app'` (the widest level, same
  // reasoning as `/admin/projects`), specifically so a purely app-scoped
  // `env:read` grant can reach this page at all; this is the boundary that
  // member then lands on once here. See page-access.ts:80.
  let catalogueReadable = $state(true);

  const projectId = $derived(sessionStore.currentProjectId);

  // Catalogue operations (create / rename / retire) hit the PROJECT and
  // change what every app in it sees. Mute / promote / rotate hit
  // /v1/app-environments/{id} and are per-app — computed per row below, not
  // here. See environments.rs:213,285,323 (project) vs :444,525 (app). The
  // explicit `level` keeps `can()` from also OR-ing in the currently
  // selected app, which would light up a catalogue button a purely
  // app-scoped grant then gets a 403 from.
  const createLock = $derived(lockedBy('env:create', { project: projectId, level: 'project' }));
  const renameLock = $derived(lockedBy('env:update', { project: projectId, level: 'project' }));
  const retireLock = $derived(lockedBy('env:delete', { project: projectId, level: 'project' }));

  // When the catalogue is unreadable, synthesize a read-only stand-in from the
  // enrollment rows the member CAN read — exactly the shape the deleted
  // EnvironmentsCard.svelte used (it built its whole view from
  // `listEnvironments(appId)` alone and never called the catalogue at all).
  // `project_id`/`updated_at` are filled but never read by the template below
  // (only id/name/created_at/retired_at are); kept ProjectEnvironment-shaped
  // rather than a narrower ad hoc type so `renaming`/`confirmRetire` below
  // don't need a second type — moot in practice since Rename/Retire are only
  // ever offered (see the template) when `catalogueReadable`, at which point
  // `catalogueLike` IS `catalogue` unchanged.
  const catalogueLike = $derived(
    catalogueReadable
      ? catalogue
      : Array.from(
          new Map(
            enrollments.map((r) => [
              r.environment_id,
              {
                id: r.environment_id,
                project_id: projectId ?? '',
                name: r.name,
                created_at: r.created_at,
                retired_at: r.retired_at,
                updated_at: r.updated_at,
              } satisfies ProjectEnvironment,
            ]),
          ).values(),
        ),
  );

  const activeCatalogue = $derived(catalogueLike.filter((e) => !e.retired_at));
  const retiredCatalogue = $derived(catalogueLike.filter((e) => e.retired_at));

  // Whether `apps` — and therefore `enrollments` — is this project's COMPLETE
  // set, or possibly a silently filtered subset.
  //
  // `listApps` (projects.rs:169-214) is reach-based: a caller lacking
  // `app:read` gets HTTP 200 and a filtered list — often `[]` — never a 403.
  // (It 403s only when the caller holds no grant at all in the org.) This page
  // is gated on `env:read` at project level, and `app:read` sits in a
  // different permission group entirely (permissions.ts:56-62), so a role that
  // may administer environments but not list apps is one checkbox away in the
  // Roles editor — not an exotic case.
  //
  // The backend returns the project's complete app list only on its fast path,
  // `reach.org || reach.projects.contains(&project_id)` (projects.rs:187).
  // `level: 'project'` mirrors that set exactly: it zeroes `app` and `env` in
  // `can()`, so only org- and project-scoped grants can satisfy it — precisely
  // the two the fast path accepts. Anything narrower (an app- or env-scoped
  // `app:read`) also returns 200, but filtered to a SUBSET: just as blind, and
  // far easier to miss than an empty list.
  const appListComplete = $derived(
    sessionStore.can('app:read', { project: projectId, level: 'project' }),
  );

  // Whether every app's enrollment fetch actually succeeded. Separate from
  // `appListComplete`, which is a permission prediction: this one records what
  // the fan-out really returned. The two come apart for a member holding
  // project-scoped `app:read` but only app-scoped `env:read` — they see all
  // three apps and get 403 on two of the three `listEnvironments` legs.
  // `enrollments` is then a subset for a reason no permission check can see,
  // which is exactly the state `isDefaultSomewhere` must not be trusted in.
  //
  // It is also SAID OUT LOUD, in its own copy in the template — recording the
  // loss without surfacing it just moved the silent-partial from the fan-out
  // into a variable. The permission note keyed on `appListComplete` cannot
  // stand in for it: "you cannot see all apps" and "some apps could not be
  // loaded" are different claims, and only one of them is fixed by reloading.
  let enrollmentsComplete = $state(true);

  // The real precondition for believing a `false` out of `isDefaultSomewhere`:
  // the app list is whole AND every app answered.
  const enrollmentViewComplete = $derived(appListComplete && enrollmentsComplete);

  const appById = $derived.by(() => {
    const map: Record<string, App> = {};
    for (const a of apps) map[a.id] = a;
    return map;
  });

  function appNameFor(appId: string): string {
    return appById[appId]?.name ?? 'this app';
  }

  function rowsFor(environmentId: string): AppEnvironment[] {
    return enrollments.filter((r) => r.environment_id === environmentId);
  }

  // An environment cannot be retired while it is any app's default, nor while
  // it is the project's only live one — the backend's two 409 preconditions
  // (environments.rs:341-352: `count_active_project_environments <= 1`, then
  // `apps_defaulting_to_environment > 0`). EnvironmentsCard could only ever
  // see its OWN app's default flag and said so out loud; this page can see
  // every app's enrollment and mirror the check properly.
  //
  // ONLY VALID WHEN `appListComplete`. This answers "is it default for any app
  // I can see", which equals the backend's "for any app" only if I can see
  // them all. With a filtered `apps` list it returns `false` for an
  // environment that IS some hidden app's default — a fail-open answer. The
  // sole caller therefore skips it entirely when the list is incomplete rather
  // than trusting a `false` it has no standing to give; see `canRetire`.
  function isDefaultSomewhere(environmentId: string): boolean {
    return enrollments.some((r) => r.environment_id === environmentId && r.is_default);
  }

  async function load(pid: string) {
    loading = true;
    error = null;
    try {
      const projectApps = await listApps(pid);
      apps = projectApps;
      // `allSettled`, NOT `all` — the pattern already used at
      // IssueDetail.svelte:98. This page is reachable with an APP-scoped
      // `env:read`, so a member with partial reach gets 403 on the apps they
      // don't hold. Under `all` one such leg rejected the whole fan-out and
      // blanked the entire page (and, because the promise was awaited after the
      // catalogue block, an early throw there left it rejecting unhandled).
      // Degrade to the apps that did answer; `enrollmentsComplete` records that
      // the view is partial so `canRetire` stops trusting it.
      const enrollmentsPromise = Promise.allSettled(
        projectApps.map((a) => listEnvironments(a.id, true)),
      );

      // Isolated from the `try` above it: this is the ONE call in `load()` a
      // purely app-scoped `env:read` grant cannot make (see the comment on
      // `catalogueReadable`). Catching its 403 HERE, rather than letting it
      // reject the same way every other call in this function does, is what
      // keeps that member's own enrollment rows on screen instead of a blank
      // error card — degrade this one piece, not the whole page.
      try {
        catalogue = await listProjectEnvironments(pid, true);
        catalogueReadable = true;
      } catch (err) {
        if (isNormalizedError(err) && err.status === 403) {
          catalogue = [];
          catalogueReadable = false;
        } else {
          throw err;
        }
      }

      const settled = await enrollmentsPromise;
      enrollments = settled.flatMap((r) => (r.status === 'fulfilled' ? r.value : []));
      enrollmentsComplete = settled.every((r) => r.status === 'fulfilled');
    } catch (err) {
      error = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const pid = sessionStore.currentProjectId;
    if (pid) void load(pid);
  });

  /**
   * Refetch the catalogue and every app's enrollments without touching
   * loading/error, reporting success rather than throwing. Used after
   * create, whose result this page cannot reconstruct locally: the catalogue
   * POST returns only the catalogue row, and each app's freshly minted
   * enrollment (key, DSN) is created server-side as a side effect.
   */
  async function refetchAll(pid: string): Promise<boolean> {
    try {
      // Same partial-reach reasoning as `load()`: `allSettled` on the per-app
      // fan-out so one 403 does not discard the apps that did answer. The
      // catalogue leg stays on the outer `Promise.all` — this function is only
      // called after a successful catalogue POST, so a catalogue read failing
      // here is a genuine failure worth reporting as one.
      const [cat, perApp] = await Promise.all([
        listProjectEnvironments(pid, true),
        Promise.allSettled(apps.map((a) => listEnvironments(a.id, true))),
      ]);
      catalogue = cat;
      enrollments = perApp.flatMap((r) => (r.status === 'fulfilled' ? r.value : []));
      enrollmentsComplete = perApp.every((r) => r.status === 'fulfilled');
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Replace one enrollment row in place so the list doesn't jump while a row
   * is busy. The enrollment endpoints return the bare row with no `name` on
   * it (the name lives on the catalogue entry), so carry the existing one
   * across rather than blanking it.
   */
  function mergeEnrollment(updated: AppEnvironmentRow) {
    enrollments = enrollments.map((e) => (e.id === updated.id ? { ...updated, name: e.name } : e));
  }

  async function submitCreate() {
    const pid = projectId;
    if (!pid || createBusy || !newName.trim()) return;
    createBusy = true;
    try {
      const created = await createProjectEnvironment(pid, { name: newName.trim() });
      newName = '';
      creating = false;
      const refreshed = await refetchAll(pid);
      if (refreshed) {
        toastStore.success(`"${created.name}" added to every app in this project.`);
      } else {
        toastStore.success(
          `"${created.name}" added to every app in this project. Reload to see its ingest keys.`,
        );
      }
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      createBusy = false;
    }
  }

  async function submitRename() {
    const target = renaming;
    if (!target || !renameValue.trim()) return;
    busyId = target.id;
    renameBusy = true;
    try {
      const renamed = await renameProjectEnvironment(target.id, { name: renameValue.trim() });
      catalogue = catalogue.map((e) => (e.id === renamed.id ? renamed : e));
      enrollments = enrollments.map((r) =>
        r.environment_id === renamed.id ? { ...r, name: renamed.name } : r,
      );
      renaming = null;
      toastStore.success(`Renamed to "${renamed.name}" for every app in this project.`);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
      renameBusy = false;
    }
  }

  async function toggleIngest(row: AppEnvironment) {
    busyId = row.id;
    try {
      mergeEnrollment(await updateAppEnvironment(row.id, { ingest_enabled: !row.ingest_enabled }));
      const appLabel = appNameFor(row.app_id);
      toastStore.success(
        row.ingest_enabled ? `Ingest muted for ${appLabel}.` : `Ingest resumed for ${appLabel}.`,
      );
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function promote(row: AppEnvironment) {
    busyId = row.id;
    try {
      const promoted = await updateAppEnvironment(row.id, { is_default: true });
      // Only demote THIS SAME APP's previous default — "default" is a
      // per-app flag, and `enrollments` now spans every app in the project.
      // Scoping the demotion by `app_id` is what keeps a promote in one app
      // from touching another app's rows, which a straight port of
      // EnvironmentsCard's single-app "any other row with is_default" logic
      // would have done.
      enrollments = enrollments.map((e) =>
        e.id === promoted.id
          ? { ...promoted, name: e.name }
          : e.app_id === promoted.app_id && e.is_default
            ? { ...e, is_default: false }
            : e,
      );
      toastStore.success(`"${row.name}" is now ${appNameFor(row.app_id)}'s default environment.`);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function doRotate() {
    const target = confirmRotate;
    if (!target) return;
    busyId = target.id;
    try {
      mergeEnrollment(await rotateAppEnvironmentKey(target.id));
      confirmRotate = null;
      toastStore.success(
        `Key rotated for ${appNameFor(target.app_id)}. Update this environment’s DSN everywhere it's configured.`,
      );
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function doRetire() {
    const target = confirmRetire;
    if (!target) return;
    busyId = target.id;
    try {
      const retiredEnv = await retireProjectEnvironment(target.id);
      catalogue = catalogue.map((e) => (e.id === retiredEnv.id ? retiredEnv : e));
      enrollments = enrollments.map((r) =>
        r.environment_id === retiredEnv.id
          ? {
              ...r,
              name: retiredEnv.name,
              retired_at: retiredEnv.retired_at,
              ingest_enabled: false,
              is_default: false,
              updated_at: retiredEnv.updated_at,
            }
          : r,
      );
      confirmRetire = null;
      toastStore.success(`"${target.name}" retired project-wide. Its data stays queryable.`);
    } catch (err) {
      // A race with another admin can still trip the backend's "last live"
      // or "still some app's default" 409 even though isDefaultSomewhere
      // hides the button for those cases locally. Surface it rather than
      // failing silently.
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }
</script>

<AdminShell requireProject>
  <div class="head">
    <div>
      <h1 class="page-title">Environments</h1>
      <p class="muted sub">
        Defined by {sessionStore.currentProject?.name ?? 'this project'} and shared by every app in
        it — creating, renaming or retiring one below changes it for all of them. Each app's ingest
        key, mute switch and default stay its own, set per app below.
      </p>
    </div>
    {#if catalogueReadable}
      <Button variant="primary" lockedReason={createLock} onclick={() => (creating = true)}>
        New environment
      </Button>
    {/if}
  </div>

  {#if loading}
    <div class="center"><Spinner size={26} /></div>
  {:else if error}
    <Card><p class="err-msg">{error}</p></Card>
  {:else if catalogueReadable && catalogue.length === 0}
    <EmptyState
      title="No environments yet"
      description="Create one to start separating dev, staging and production traffic. Every app in this project is enrolled automatically, each with its own ingest key."
      icon="layers"
    >
      {#snippet action()}
        <Button variant="primary" lockedReason={createLock} onclick={() => (creating = true)}>
          New environment
        </Button>
      {/snippet}
    </EmptyState>
  {:else if !catalogueReadable && catalogueLike.length === 0}
    <EmptyState
      title="No environments visible"
      description="The project-wide catalogue needs project-level View environments (env:read), which your role doesn't grant here, and none of your apps are enrolled anywhere you can see. Ask an organization owner for access."
      icon="layers"
    />
  {:else}
    {#if !catalogueReadable}
      <!-- appListComplete's counterpart at the catalogue level: this member's
           `env:read` is app-scoped only (page-access.ts:80 widens the page
           gate to admit exactly this member), so `authorize_project` refuses
           the catalogue read (environments.rs:196) and `load()` caught its
           403 rather than failing the whole page. Show what IS readable —
           this member's own apps' enrollments — and say why the rest is
           missing rather than silently rendering a partial catalogue as if
           it were the whole one. -->
      <p class="muted catalogue-note">
        The project-wide catalogue needs project-level <strong>View environments</strong>
        (env:read) — showing only the environments your apps are enrolled in. Renaming and
        retiring are catalogue-level actions and aren't available from here.
      </p>
    {/if}
    <div class="env-list">
      {#each activeCatalogue as env (env.id)}
        {@const rows = rowsFor(env.id)}
        <!-- Mirrors the backend's two 409 preconditions, but only claims the
             second when it can actually see every app. Blind, it offers Retire
             and lets the 409 speak — an honest 409 beats a guard that quietly
             assumes a view it does not have. -->
        {@const canRetire =
          activeCatalogue.length > 1 && (!enrollmentViewComplete || !isDefaultSomewhere(env.id))}
        <Card padding="none">
          {#snippet header()}
            <div class="env-title">
              <span class="name">{env.name}</span>
              <span class="when muted">created <TimeValue value={env.created_at} /></span>
            </div>
          {/snippet}
          {#snippet actions()}
            <!-- Rename/Retire are catalogue-level (project scope) actions —
                 gated on `catalogueReadable`, not just their locks, because a
                 degraded `env` here is synthesized from this member's own
                 enrollments (catalogueLike), not the real catalogue, and
                 offering catalogue-wide mutations off a partial view is the
                 wrong shape regardless of what the caller's grants allow. -->
            {#if catalogueReadable}
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                lockedReason={renameLock}
                title="Renames this environment for every app in the project"
                onclick={() => {
                  renaming = env;
                  renameValue = env.name;
                }}
              >
                Rename
              </Button>
              <!-- `canRetire` is a business rule, not a permission: an
                   environment that cannot be retired by anyone right now
                   (last live, or still some app's default) should not render a
                   locked button implying a missing grant. -->
              {#if canRetire}
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={busyId === env.id}
                  lockedReason={retireLock}
                  title="Retires this environment for every app in the project"
                  onclick={() => (confirmRetire = env)}
                >
                  Retire
                </Button>
              {/if}
            {/if}
          {/snippet}

          {#if rows.length > 0}
            <div class="app-rows">
              {#each rows as row (row.id)}
                {@const updateLock = lockedBy('env:update', { app: row.app_id, level: 'app' })}
                {@const rotateLock = lockedBy('env:rotate_key', { app: row.app_id, level: 'app' })}
                <div class="app-row" class:muted-row={!row.ingest_enabled}>
                  <div class="app-row-head">
                    <span class="app-name">{appNameFor(row.app_id)}</span>
                    {#if row.is_default}<Badge tone="info" size="sm">Default</Badge>{/if}
                    {#if !row.ingest_enabled}<Badge tone="warning" size="sm">Muted</Badge>{/if}
                  </div>

                  <div class="dsn">
                    <code>{buildDsn(row.public_key, row.id)}</code>
                    <CopyButton value={buildDsn(row.public_key, row.id)} size="sm" />
                  </div>

                  <div class="row-actions">
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={busyId === row.id}
                      lockedReason={updateLock}
                      onclick={() => toggleIngest(row)}
                    >
                      {row.ingest_enabled ? 'Mute ingest' : 'Resume ingest'}
                    </Button>
                    {#if !row.is_default}
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={busyId === row.id}
                        lockedReason={updateLock}
                        onclick={() => promote(row)}
                      >
                        Make default
                      </Button>
                    {/if}
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={busyId === row.id}
                      lockedReason={rotateLock}
                      onclick={() => (confirmRotate = row)}
                    >
                      Rotate key
                    </Button>
                  </div>
                </div>
              {/each}
              {#if !appListComplete}
                <!-- Rows are present but the list may be partial: an app- or
                     env-scoped `app:read` returns 200 with a SUBSET. Say so,
                     rather than letting a filtered list read as the whole
                     project. -->
                <p class="no-apps muted partial">
                  Only the apps you can see are listed. Others in this project may be enrolled
                  too — showing them needs <strong>View apps</strong> (app:read).
                </p>
              {/if}
              {#if !enrollmentsComplete}
                <!-- A DIFFERENT statement from the one above, and deliberately
                     worded as one. `appListComplete` is a permission
                     prediction ("you are not allowed to see every app");
                     `enrollmentsComplete` records a fetch OUTCOME ("some app's
                     enrollment request failed"). `Promise.allSettled` on the
                     fan-out is what keeps one app's 403 from blanking the page,
                     but it also means a failed leg is dropped silently — so
                     without this the member reads a confidently incomplete
                     list. Reload rather than a retry button: `load()` is keyed
                     on the project effect, and there is no partial refetch. -->
                <p class="no-apps muted partial">
                  Some apps could not be loaded — their ingest keys failed to fetch, so apps that
                  <em>are</em> enrolled here may be missing from this list. Reload the page to try
                  again.
                </p>
              {/if}
            </div>
          {:else if !appListComplete}
            <!-- The empty body here is OUR blindness, not an empty project.
                 `listApps` answers a caller without `app:read` with 200 and an
                 empty list, so the old "No apps enrolled" copy stated as fact
                 something we never learned. -->
            <p class="no-apps muted">
              Per-app ingest keys aren't shown — listing this project's apps needs the
              <strong>View apps</strong> (app:read) permission, which your role doesn't grant
              here. The environments themselves are shown in full.
            </p>
          {:else if apps.length === 0}
            <p class="no-apps muted">
              No apps in this project yet. Create one and it is enrolled here automatically, with
              its own ingest key.
            </p>
          {:else if !enrollmentsComplete}
            <!-- Zero rows AND a failed fan-out leg: "No apps enrolled" would be
                 a claim we never learned. This is the empty-body counterpart of
                 the partial note above — same distinction between a permission
                 prediction and a fetch outcome. -->
            <p class="no-apps muted">
              Ingest keys could not be loaded for this project's apps — the request failed, so
              whether anything is enrolled here is unknown. Reload the page to try again.
            </p>
          {:else}
            <p class="no-apps muted">No apps enrolled in this environment yet.</p>
          {/if}
        </Card>
      {/each}
    </div>

    {#if retiredCatalogue.length > 0}
      <div class="retired-toggle">
        <Button variant="ghost" size="sm" onclick={() => (showRetired = !showRetired)}>
          <Icon name={showRetired ? 'chevron-down' : 'chevron-right'} size={14} />
          {retiredCatalogue.length} retired
        </Button>
      </div>
      {#if showRetired}
        <ul class="env-list retired">
          {#each retiredCatalogue as env (env.id)}
            <li class="env">
              <div class="retired-head">
                <span class="name">{env.name}</span>
                <Badge tone="neutral" size="sm">Retired</Badge>
                <span class="when muted">retired <TimeValue value={env.retired_at} /></span>
              </div>
              <p class="muted note">
                Ingest is off and its key no longer works. Existing data stays queryable.
              </p>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}
  {/if}

  <Modal bind:open={creating} title="New project environment" size="sm">
    <Input
      label="Name"
      bind:value={newName}
      placeholder="staging"
      hint="Added to every app in this project, each with its own ingest key. Lowercase and short works best — this appears in every filter."
    />
    {#snippet footer()}
      <Button variant="secondary" disabled={createBusy} onclick={() => (creating = false)}>
        Cancel
      </Button>
      <Button loading={createBusy} disabled={!newName.trim()} onclick={submitCreate}>
        Create
      </Button>
    {/snippet}
  </Modal>

  <Modal
    open={renaming !== null}
    title="Rename environment"
    size="sm"
    onclose={() => (renaming = null)}
  >
    <Input
      label="Name"
      bind:value={renameValue}
      hint="This name belongs to the project — renaming it renames it for every app in the project."
    />
    {#snippet footer()}
      <Button variant="secondary" onclick={() => (renaming = null)} disabled={renameBusy}>
        Cancel
      </Button>
      <Button loading={renameBusy} disabled={!renameValue.trim()} onclick={submitRename}>
        Save
      </Button>
    {/snippet}
  </Modal>

  <ConfirmDialog
    open={confirmRotate !== null}
    title="Rotate ingest key?"
    message={`Anything ${appNameFor(confirmRotate?.app_id ?? '')} reports to "${confirmRotate?.name ?? ''}" stops until its DSN is updated. There is no grace period.`}
    confirmLabel="Rotate"
    loading={busyId !== null && busyId === confirmRotate?.id}
    onconfirm={doRotate}
    oncancel={() => (confirmRotate = null)}
  />

  <ConfirmDialog
    open={confirmRetire !== null}
    title="Retire environment for the whole project?"
    message={`"${confirmRetire?.name ?? ''}" will be removed from every app in this project. All of their keys for it stop working immediately and it leaves every picker. Existing data stays queryable and is archived to cold storage on the normal schedule. This cannot be undone.`}
    confirmLabel="Retire"
    danger
    loading={busyId !== null && busyId === confirmRetire?.id}
    onconfirm={doRetire}
    oncancel={() => (confirmRetire = null)}
  />
</AdminShell>

<style>
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 18px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 4px;
    max-width: 64ch;
    line-height: 1.55;
  }
  .center {
    display: grid;
    place-items: center;
    padding: 80px;
  }
  .err-msg {
    color: var(--error);
    font-size: 13.5px;
  }
  .catalogue-note {
    font-size: 13px;
    line-height: 1.55;
    margin-bottom: 14px;
  }
  .env-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .env-title {
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex-wrap: wrap;
  }
  .name {
    font-weight: 600;
    font-size: 14.5px;
  }
  .when {
    font-size: 0.8rem;
  }
  .app-rows {
    display: flex;
    flex-direction: column;
  }
  .app-row {
    padding: 12px 18px;
    border-bottom: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .app-row:last-child {
    border-bottom: none;
  }
  .muted-row {
    opacity: 0.7;
  }
  .app-row-head {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .app-name {
    font-weight: 560;
    font-size: 13.5px;
  }
  .dsn {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .dsn code {
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    white-space: nowrap;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    padding: 6px 8px;
    font-size: 0.8rem;
  }
  .row-actions {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
  }
  .no-apps {
    font-size: 13px;
    padding: 14px 18px;
    line-height: 1.55;
  }
  /* Sits under real rows rather than replacing them, so it needs the same
     separator every row above it carries. */
  .no-apps.partial {
    border-top: 1px solid var(--border);
  }
  .retired-toggle {
    margin-top: 14px;
  }
  .retired {
    margin-top: 8px;
  }
  .env {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .retired-head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .note {
    font-size: 0.8rem;
    margin: 0;
  }
</style>
