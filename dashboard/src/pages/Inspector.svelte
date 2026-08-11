<!--
  PII inspector. Four hand-rolled tabs (there is no Tabs primitive and adding
  one is out of scope): Findings / Policy / Scans / Audit.

  Every destructive control is gated on `pii:manage` AT THE CURRENT APP, and
  the sidebar entry's `show` is cosmetic — the endpoint's 403 is the real gate.
-->
<script lang="ts">
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import ClientPager from '../lib/components/ClientPager.svelte';
  import TimeValue from '../lib/components/TimeValue.svelte';
  import JsonTree from '../lib/components/JsonTree.svelte';
  import MaskDialog from '../lib/components/inspector/MaskDialog.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { lockedBy, lockTitle } from '../lib/models/page-access';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { errorMessage } from '../lib/api/client';
  import * as inspectorApi from '../lib/api/inspector';
  import { WEEKDAYS, COMMON_TIMEZONES, DST_RISK_HOURS } from '../lib/constants/inspectorSchedules';
  import {
    weekdayMaskToArray,
    weekdayArrayToMask,
    describeSchedule,
    nextRuns,
  } from '../lib/models/inspector-schedule';
  import { groupFindings, formatMatchCount, findingBadges } from '../lib/models/inspector-findings';
  import {
    csvFilename,
    UNREACHABLE_COPY,
    parseKeyInput,
    createPolicyBlockedReason,
    defaultEnvEnrollmentId,
  } from '../lib/models/inspector';
  import { DETECTORS, SUGGESTED_KEYS } from '../lib/constants/inspectorDetectors';
  import { setOffsetPage, setOffsetSort, type OffsetListState } from '../lib/models/list-state';
  import {
    FINDING_DEFAULT_SORT,
    MASK_DEFAULT_SORT,
    SCAN_DEFAULT_SORT,
    findingAccessor,
    maskActionAccessor,
    scanAccessor,
  } from '../lib/models/pii-inspector-sort';
  import { pageSlice } from '../lib/models/paginate';
  import { sortRows } from '../lib/models/sort-rows';
  import type { SortDir } from '../lib/models/sort';
  import type {
    EffectivePolicy,
    InspectorFinding,
    InspectorMaskAction,
    InspectorScan,
  } from '../lib/models';

  type Tab = 'findings' | 'policy' | 'scans' | 'audit';
  let tab = $state<Tab>('findings');

  let loading = $state(true);
  let error = $state('');
  // $state.raw, not $state: these are replaced wholesale on every reload and
  // deep-proxying them makes `===` never match a raw row, which breaks the
  // "is this the row I opened?" check in the expand map below.
  let effective = $state.raw<EffectivePolicy | null>(null);
  let scans = $state.raw<InspectorScan[]>([]);
  let findings = $state.raw<InspectorFinding[]>([]);
  let actions = $state.raw<InspectorMaskAction[]>([]);
  let coverageNote = $state('');
  let detectionCaveat = $state('');
  let expanded = $state<Record<string, boolean>>({});
  let revealed = $state<Record<string, unknown>>({});
  let maskTargetFinding = $state.raw<InspectorFinding | null>(null);
  let newKey = $state('');

  const appId = $derived(sessionStore.currentAppId);
  // Only the FALLBACK filename — the server's Content-Disposition wins.
  const appSlug = $derived(sessionStore.currentApp?.slug ?? 'app');
  // Every mutating inspector endpoint authorizes at the policy target, which
  // for this page is always the current app (inspector.rs:399,502,587,667,975).
  const manageLock = $derived(
    lockedBy('pii:manage', { app: sessionStore.currentAppId, level: 'app' }),
  );
  const policy = $derived(effective?.policy ?? null);
  const groups = $derived(groupFindings(findings));

  // --- sorting and paging ---------------------------------------------------
  // Every list on this page arrives whole, so both the sort and the pager run
  // here, over the SAME array each time: order the whole list first, then take
  // a window out of it. Sorting the window instead would reorder only what is
  // on screen while presenting itself as having ordered everything.
  //
  // `sortRows` copies, which matters even though these arrays are `$state.raw`
  // — raw means the array is handed around by identity, so an in-place sort
  // would reorder the very array `loadAll` stored and the polling effect reads.
  /** Rows per page, for all three tables. */
  const PAGE = 25;

  // The Findings tab renders ONE table per `source_table.source_column` group,
  // so its list state is per group KEY, not one object: each group is its own
  // table with its own header row, and sorting one must not reorder — or
  // re-page — its neighbours.
  const FINDING_INITIAL: OffsetListState = { sort: FINDING_DEFAULT_SORT, offset: 0 };
  let findingLists = $state<Record<string, OffsetListState>>({});
  const findingList = (groupKey: string): OffsetListState =>
    findingLists[groupKey] ?? FINDING_INITIAL;
  // Replaced, never mutated in place — the house rule for a Record inside
  // `$state` on this page (see `expanded` below).
  function onFindingSort(groupKey: string, key: string, columnDefault: SortDir) {
    findingLists = {
      ...findingLists,
      [groupKey]: setOffsetSort(findingList(groupKey), key, columnDefault),
    };
  }
  function setFindingPage(groupKey: string, offset: number) {
    findingLists = {
      ...findingLists,
      [groupKey]: setOffsetPage(findingList(groupKey), offset),
    };
  }

  let scanList = $state<OffsetListState>({ sort: SCAN_DEFAULT_SORT, offset: 0 });
  const scansSorted = $derived(sortRows(scans, scanAccessor(scanList.sort.key), scanList.sort.dir));
  const scanPage = $derived(pageSlice(scansSorted, scanList.offset, PAGE));
  function onScanSort(key: string, columnDefault: SortDir) {
    scanList = setOffsetSort(scanList, key, columnDefault);
  }

  let maskList = $state<OffsetListState>({ sort: MASK_DEFAULT_SORT, offset: 0 });
  const masksSorted = $derived(
    sortRows(actions, maskActionAccessor(maskList.sort.key), maskList.sort.dir),
  );
  const maskPage = $derived(pageSlice(masksSorted, maskList.offset, PAGE));
  function onMaskSort(key: string, columnDefault: SortDir) {
    maskList = setOffsetSort(maskList, key, columnDefault);
  }

  // --- create-policy form -------------------------------------------------
  // Only reachable when no policy covers this app; `create_policy` is
  // org-scoped on the wire (POST /v1/orgs/{org}/inspector/policies) with the
  // project / app / environment named in the body.
  type TargetType = 'project' | 'app' | 'app_env';
  const TARGET_TYPES: { value: TargetType; label: string; hint: string }[] = [
    { value: 'project', label: 'Project', hint: 'Covers every app in this project' },
    { value: 'app', label: 'App', hint: 'Covers this app, in every environment' },
    { value: 'app_env', label: 'Environment', hint: 'Covers one environment of this app' },
  ];
  let newTargetType = $state<TargetType>('app');
  let newEnvId = $state('');
  let newKeyInput = $state(SUGGESTED_KEYS.join(', '));
  let newDetectors = $state<string[]>([]);

  const orgId = $derived(sessionStore.currentOrgId);
  // Retired enrollments are hidden from every other picker in the app; a
  // policy scoped to one would inspect an environment that cannot ingest.
  const envOptions = $derived(sessionStore.environments.filter((e) => e.retired_at === null));
  const newKeys = $derived(parseKeyInput(newKeyInput));
  const newTargetId = $derived(
    newTargetType === 'project'
      ? sessionStore.currentProjectId
      : newTargetType === 'app'
        ? sessionStore.currentAppId
        : // `app_env` takes the ENROLLMENT id (`app_environments.id`), which is
          // what `AppEnvironment.id` holds — NOT its `environment_id`.
          //
          // Read STRAIGHT off the bound value, with no "or the first one"
          // fallback: a fallback here is what let a blank picker submit
          // production. The seeding effect below is what guarantees this is
          // non-empty, so the id sent is always the one on screen.
          newEnvId || null,
  );

  // Seed (and re-seed) the environment picker so its bound value always names
  // a real option. Writes only when the current value is NOT in the list, so
  // it converges after one pass instead of fighting the user's selection on
  // every keystroke elsewhere in the form.
  $effect(() => {
    if (envOptions.some((e) => e.id === newEnvId)) return;
    newEnvId = defaultEnvEnrollmentId(envOptions) ?? '';
  });
  const createBlocked = $derived(createPolicyBlockedReason(newTargetId, newKeys, newDetectors));

  // `quiet` exists because the poll below re-enters this function every three
  // seconds while a scan runs: without it the whole tab body would be replaced
  // by the spinner on every tick, so the one screen you watch progress on
  // spends most of its time showing nothing.
  async function loadAll(quiet = false) {
    if (!appId) return;
    if (!quiet) loading = true;
    error = '';
    try {
      effective = await inspectorApi.effectivePolicy(appId);
      actions = await inspectorApi.listAppMaskActions(appId);
      if (effective.policy) {
        scans = await inspectorApi.listScans(effective.policy.id);
        const latest = scans.find((s) => s.status === 'succeeded') ?? scans[0];
        if (latest) {
          const page = await inspectorApi.listFindings(latest.id);
          findings = page.findings;
          coverageNote = page.coverage === 'partial' ? page.coverage_note : '';
          detectionCaveat = page.detection_caveat;
        } else {
          findings = [];
        }
      } else {
        scans = [];
        findings = [];
      }
    } catch (e) {
      // `errorMessage`, not `e.message`: every rejection from `api` is a
      // NormalizedError PLAIN OBJECT (client.ts rejects with
      // `normalizeError(error)`), so `e instanceof Error` is false and the
      // banner would read "[object Object]" for the 403 that is this page's
      // real gate.
      error = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  // Every write and every download goes through here. A bare `await` inside an
  // onclick throws into an unhandled rejection, so a 403 — and `manageLock` is
  // only a client-side guess, since a grant can be scoped to one environment —
  // would be indistinguishable from a button that does nothing.
  async function act(fn: () => Promise<unknown>, reload = true): Promise<void> {
    try {
      await fn();
      if (reload) await loadAll();
    } catch (e) {
      toastStore.error(errorMessage(e));
    }
  }

  $effect(() => {
    // Re-read on app switch. `appId` is the only dependency on purpose: a
    // reload triggered by anything else would wipe a half-typed policy edit.
    if (appId) void loadAll();
  });

  // Poll only while something is in flight, and clear the interval in the
  // teardown — an interval that outlives the page keeps a dead component
  // fetching for as long as the tab is open.
  $effect(() => {
    const busy =
      scans.some((s) => s.status === 'queued' || s.status === 'running') ||
      actions.some((a) => ['preview', 'pending', 'running', 'cancelling'].includes(a.status));
    if (!busy) return;
    const id = setInterval(() => void loadAll(true), 3000);
    return () => clearInterval(id);
  });
</script>

<AdminShell requireApp>
  <div class="head">
    <h1>Privacy inspector</h1>
    {#if effective}
      <span class="muted">
        New events are masked within about {effective.enforcement_latency_secs} seconds of a change.
      </span>
    {/if}
  </div>

  <nav class="tabs" aria-label="Privacy inspector sections">
    <button class="tab" class:active={tab === 'findings'} onclick={() => (tab = 'findings')}>
      Findings <span class="count">{findings.length}</span>
    </button>
    <button class="tab" class:active={tab === 'policy'} onclick={() => (tab = 'policy')}>Policy</button>
    <button class="tab" class:active={tab === 'scans'} onclick={() => (tab = 'scans')}>
      Scans <span class="count">{scans.length}</span>
    </button>
    <button class="tab" class:active={tab === 'audit'} onclick={() => (tab = 'audit')}>
      Audit <span class="count">{actions.length}</span>
    </button>
  </nav>

  {#if error}
    <Card><p class="err">{error}</p></Card>
  {:else if loading}
    <Spinner />
  {:else if tab === 'findings'}
    <Card>
      <!-- Non-dismissible, always. Detection is best-effort: the prefilter
           greps the JSON TEXT for the quoted key name, so a key hidden by a
           unicode escape, base64 or URL encoding is not found. -->
      <p class="caveat"><Icon name="info" size={14} /> {detectionCaveat}</p>
      {#if coverageNote}
        <p class="caveat">Coverage is partial: {coverageNote}</p>
      {/if}
    </Card>
    {#if findings.length === 0}
      <EmptyState title="No findings" description="Run a scan from the Scans tab." />
    {:else}
      {#each groups as g (g.key)}
        <!-- Sort the whole group, THEN slice it — reversing these two lines
             would order only the rows already on screen. `gl` is this group's
             own sort/offset; `gSorted` is what the pager below measures. -->
        {@const gl = findingList(g.key)}
        {@const gSorted = sortRows(g.findings, findingAccessor(gl.sort.key), gl.sort.dir)}
        {@const gPage = pageSlice(gSorted, gl.offset, PAGE)}
        <Card>
          <h3>{g.table}.{g.column} <Badge>{formatMatchCount(g.total, true)} matches</Badge></h3>
          <DataTable>
            {#snippet head()}
              <tr>
                <SortableTh
                  key="path"
                  columnDefault="asc"
                  sort={gl.sort}
                  onsort={(k, d) => onFindingSort(g.key, k, d)}
                >
                  Path
                </SortableTh>
                <SortableTh
                  key="type"
                  columnDefault="asc"
                  sort={gl.sort}
                  onsort={(k, d) => onFindingSort(g.key, k, d)}
                >
                  Type
                </SortableTh>
                <SortableTh
                  key="matches"
                  class="num"
                  sort={gl.sort}
                  onsort={(k, d) => onFindingSort(g.key, k, d)}
                >
                  Matches
                </SortableTh>
                <SortableTh
                  key="last_seen"
                  sort={gl.sort}
                  onsort={(k, d) => onFindingSort(g.key, k, d)}
                >
                  Last seen
                </SortableTh>
                <!-- Badges and the Mask button: no value to order by. -->
                <th></th>
              </tr>
            {/snippet}
            {#snippet children()}
              {#each gPage.rows as f (f.id)}
                <tr
                  class="clickable"
                  onclick={() => (expanded = { ...expanded, [f.id]: !expanded[f.id] })}
                >
                  <td>{f.key_path || '(whole value)'}</td>
                  <td>{f.value_type}</td>
                  <td class="num">{formatMatchCount(f.match_count, f.match_count_exact)}</td>
                  <td><TimeValue value={f.last_seen_at} /></td>
                  <td>
                    <!-- Badge has no `title` prop, so the consequence text
                         hangs on a wrapping span instead. -->
                    {#each findingBadges(f) as b (b.label)}
                      <span title={b.title}><Badge>{b.label}</Badge></span>
                    {/each}
                    {#if !['devices', 'identities', 'workflows'].includes(f.source_table)}
                      <Button
                        variant="danger"
                        size="sm"
                        lockedReason={manageLock}
                        onclick={(e: MouseEvent) => {
                          e.stopPropagation();
                          // `groupFindings` narrows its rows to the structural
                          // FindingView, which drops scan_id/org_id/app_id —
                          // MaskDialog takes the whole InspectorFinding, so
                          // re-read the row rather than casting the narrowed one.
                          maskTargetFinding = findings.find((x) => x.id === f.id) ?? null;
                        }}
                      >
                        <Icon name="eye-off" size={14} /> Mask
                      </Button>
                    {/if}
                  </td>
                </tr>
                {#if expanded[f.id]}
                  <!-- A CSS grid with ARIA roles, NOT a nested <table>: a raw
                       table here sits inside DataTable's own tbody/td and picks
                       up its :global(tbody td) padding/white-space/alignment
                       rules by DOM descendance regardless of component
                       boundaries. Background, white-space and cursor are set
                       INLINE for the same reason. -->
                  <tr>
                    <td
                      colspan="5"
                      style="background: var(--surface-2); white-space: normal; cursor: default;"
                    >
                      <div class="detail" role="table" aria-label="Finding detail">
                        <div role="row">
                          <span role="cell">Redacted preview</span>
                          <span role="cell"><code>{f.sample_preview}</code></span>
                        </div>
                        <div role="row">
                          <span role="cell">Environment</span>
                          <span role="cell">{f.environment_id ?? f.env_scope}</span>
                        </div>
                        {#if revealed[f.id] !== undefined}
                          <JsonTree value={revealed[f.id]} expandTo={2} />
                        {:else}
                          <Button
                            size="sm"
                            onclick={async () => {
                              try {
                                const r = await inspectorApi.revealFinding(f.id);
                                revealed = { ...revealed, [f.id]: r.value };
                              } catch (e) {
                                toastStore.error(errorMessage(e));
                              }
                            }}
                          >
                            Reveal one value (recorded in the audit trail)
                          </Button>
                        {/if}
                      </div>
                    </td>
                  </tr>
                {/if}
              {/each}
            {/snippet}
          </DataTable>

          <!-- `total` is the length of the EXACT array handed to `pageSlice`
               above — `gSorted`, the same expression, not `findings.length`
               and not `g.total` (which is a sum of match counts, not a row
               count). A pager measuring a longer list than the one being
               sliced re-creates the enabled-Next-onto-an-empty-page bug that
               `Pagination.hasNext` was made a required prop to kill. The
               grouping IS the filter here, and it is already applied to the
               array both the slice and this total read. -->
          <ClientPager
            offset={gl.offset}
            limit={PAGE}
            total={gSorted.length}
            onchange={(o) => setFindingPage(g.key, o)}
          />
        </Card>
      {/each}
    {/if}
  {:else if tab === 'policy'}
    <Card>
      <h3>Inspection</h3>
      {#if !policy}
        <!-- This used to be a bare EmptyState reading "Create one from the
             organization settings" — a dead pointer: there is no org settings
             screen, and `createPolicy` had no call site anywhere in the
             dashboard, so no role could create a policy at all. The form is
             here, where the wall is. -->
        <EmptyState
          title="No policy covers this app"
          description="Nothing is being inspected for personal data yet. Create a policy to start."
        />
        <form
          class="create"
          onsubmit={(e: SubmitEvent) => {
            e.preventDefault();
            if (createBlocked || !orgId || !newTargetId) return;
            void act(async () => {
              await inspectorApi.createPolicy(orgId, {
                target_type: newTargetType,
                target_id: newTargetId,
                tracked_keys: newKeys,
                detectors: newDetectors,
              });
              // Only reset once the server has accepted it — clearing on a 409
              // ("a policy already exists for this target") would discard the
              // keys the user typed along with the error they need to act on.
              newKeyInput = SUGGESTED_KEYS.join(', ');
              newDetectors = [];
              newTargetType = 'app';
            });
          }}
        >
          <fieldset class="field">
            <legend>Scope</legend>
            <p class="caveat">
              The most specific policy covering an app wins whole. A narrower one subtracts its
              scope from the parent, which is how you exclude one noisy environment.
            </p>
            <div class="chips">
              {#each TARGET_TYPES as t (t.value)}
                <Button
                  size="sm"
                  variant={newTargetType === t.value ? 'primary' : 'ghost'}
                  lockedReason={manageLock}
                  disabled={t.value === 'app_env' && envOptions.length === 0}
                  title={t.value === 'app_env' && envOptions.length === 0
                    ? 'This app has no active environments'
                    : t.hint}
                  onclick={() => (newTargetType = t.value)}
                >
                  {t.label}
                </Button>
              {/each}
            </div>
          </fieldset>

          <fieldset class="field">
            <legend>Target</legend>
            {#if newTargetType === 'app_env'}
              <!-- `env.id` is the app_environments ENROLLMENT id, not the
                   catalogue `environment_id` — `validate_scope_in_org` matches
                   `app_environments.id` for app_env, and sending the catalogue
                   id would 404 with no hint as to why. -->
              <select
                class="sel"
                aria-label="Target environment"
                disabled={manageLock !== null}
                title={manageLock ? lockTitle(manageLock) : undefined}
                bind:value={newEnvId}
              >
                {#each envOptions as env (env.id)}
                  <option value={env.id}>{env.name}{env.is_default ? ' (default)' : ''}</option>
                {/each}
              </select>
            {:else}
              <p class="target">
                <Badge>{newTargetType === 'project' ? 'project' : 'app'}</Badge>
                {newTargetType === 'project'
                  ? (sessionStore.currentProject?.name ?? 'this project')
                  : (sessionStore.currentApp?.name ?? 'this app')}
              </p>
            {/if}
          </fieldset>

          <fieldset class="field">
            <legend>Tracked keys</legend>
            <p class="caveat">
              Literal key names, matched case-insensitively and exactly at any depth.
              <code>Email</code> matches <code>email</code>; <code>user_email</code> does not.
              Separate with commas or spaces.
            </p>
            <Input
              bind:value={newKeyInput}
              disabled={manageLock !== null}
              placeholder="email, phone, password, token"
            />
            {#if newKeys.length > 0}
              <div class="chips">
                {#each newKeys as k (k.key)}<Badge>{k.key}</Badge>{/each}
              </div>
            {/if}
          </fieldset>

          <fieldset class="field">
            <legend>Detectors</legend>
            <p class="caveat">
              Match by value SHAPE rather than key name — they find PII under a key you did not
              think to track. They read more rows than a key list does, so a scan takes longer.
            </p>
            <div class="dets">
              {#each DETECTORS as d (d.id)}
                <label class="det" title={d.hint}>
                  <input
                    type="checkbox"
                    disabled={manageLock !== null}
                    checked={newDetectors.includes(d.id)}
                    onchange={(e: Event) => {
                      const on = (e.currentTarget as HTMLInputElement).checked;
                      newDetectors = on
                        ? [...newDetectors, d.id]
                        : newDetectors.filter((x) => x !== d.id);
                    }}
                  />
                  <span>{d.label}</span>
                </label>
              {/each}
            </div>
          </fieldset>

          {#if createBlocked && manageLock === null}
            <p class="caveat blocked">{createBlocked}</p>
          {/if}
          <div class="actions">
            <Button
              type="submit"
              variant="primary"
              lockedReason={manageLock}
              disabled={createBlocked !== null}
            >
              Create policy
            </Button>
            <span class="caveat">
              Scanning is scheduled separately — a new policy runs when you start a scan.
            </span>
          </div>
        </form>
      {:else}
        <p>
          Scope: <Badge>{policy.target_type}</Badge>
          Status:
          <Badge tone={policy.enabled ? 'success' : 'neutral'}>
            {policy.enabled ? 'enabled' : 'disabled'}
          </Badge>
        </p>
        <!-- There is no Toggle primitive, so this is a Button plus a Badge. -->
        <Button
          lockedReason={manageLock}
          onclick={() => act(() => inspectorApi.patchPolicy(policy.id, { enabled: !policy.enabled }))}
        >
          {policy.enabled ? 'Disable' : 'Enable'}
        </Button>

        <h4>Tracked keys</h4>
        <p class="caveat">
          Matched case-insensitively and exactly against a key name at any depth.
          <code>Email</code> matches <code>email</code>; <code>user_email</code> does not.
        </p>
        <div class="chips">
          {#each policy.tracked_keys as k (k.key)}
            <Badge>
              {k.key}{k.scope === 'top' ? ' (top level)' : ''}
              <Button
                size="sm"
                lockedReason={manageLock}
                onclick={() =>
                  act(() =>
                    inspectorApi.patchPolicy(policy.id, {
                      tracked_keys: policy.tracked_keys.filter((x) => x.key !== k.key),
                    }),
                  )}
              >
                <Icon name="x" size={12} />
              </Button>
            </Badge>
          {/each}
        </div>
        <!-- Input forwards no keyboard handler, so the "press Enter"
             affordance is the form's implicit submission; the explicit Add
             button keeps it discoverable without one. -->
          <form
            class="addkey"
            onsubmit={(e: SubmitEvent) => {
              e.preventDefault();
              const key = newKey.trim().toLowerCase();
              if (!key) return;
              // Arrays in $state are REPLACED, never mutated in place.
              void act(async () => {
                await inspectorApi.patchPolicy(policy.id, {
                  tracked_keys: [...policy.tracked_keys, { key, scope: 'any' }],
                });
                newKey = '';
              });
            }}
          >
            <Input bind:value={newKey} placeholder="Add a key and press Enter" />
            <Button type="submit" size="sm" lockedReason={manageLock}>Add</Button>
          </form>

        <h4>Schedule</h4>
        <p>{describeSchedule(policy.schedule_days, policy.schedule_time, policy.schedule_tz)}</p>
        {#if DST_RISK_HOURS.includes(Number.parseInt(policy.schedule_time.slice(0, 2), 10))}
          <p class="caveat">
            On the spring-forward day this resolves to a valid instant; on the fall-back day it runs
            once, not twice. Times from 04:00 avoid the question entirely.
          </p>
        {/if}
        <div class="chips">
          {#each weekdayMaskToArray(policy.schedule_days) as on, i (i)}
            <Button
              size="sm"
              variant={on ? 'primary' : 'ghost'}
              lockedReason={manageLock}
              onclick={() =>
                act(() => {
                  const days = weekdayMaskToArray(policy.schedule_days);
                  days[i] = !days[i];
                  return inspectorApi.patchPolicy(policy.id, {
                    schedule_days: weekdayArrayToMask(days),
                  });
                })}
            >
              {WEEKDAYS[i]}
            </Button>
          {/each}
        </div>
        <!-- No Select primitive; a raw <select> fed by the constants module. -->
        <select
          class="sel"
          aria-label="Schedule timezone"
          disabled={manageLock !== null}
          title={manageLock ? lockTitle(manageLock) : undefined}
          value={policy.schedule_tz}
          onchange={(e: Event) =>
            act(() =>
              inspectorApi.patchPolicy(policy.id, {
                schedule_tz: (e.currentTarget as HTMLSelectElement).value,
              }),
            )}
        >
          {#each COMMON_TIMEZONES as tz (tz)}
            <option value={tz}>{tz}</option>
          {/each}
        </select>
        <p class="caveat">
          Next runs (approximate — the server decides):
          {nextRuns(policy.schedule_days, policy.schedule_time, policy.schedule_tz)
            .map((d) => d.toISOString())
            .join(', ') || 'none'}
        </p>
        {#if policy.last_skip_reason}
          <p class="caveat">Last scheduled run: {policy.last_skip_reason}</p>
        {/if}
      {/if}
    </Card>

    <Card>
      <h3>Forward enforcement</h3>
      <p class="caveat">
        New events are masked within about {effective?.enforcement_latency_secs} seconds of a change.
      </p>
      {#if (effective?.masked_keys ?? []).length === 0}
        <EmptyState title="Nothing is masked yet" description="Mask a finding to start enforcing." />
      {:else}
        <ul>
          {#each effective?.masked_keys ?? [] as k (k.id)}
            <li>
              <code>{k.target_table}.{k.target_column}{k.json_path ? `.${k.json_path}` : ''}</code>
              <span class="caveat">since <TimeValue value={k.created_at} /></span>
            </li>
          {/each}
        </ul>
      {/if}
    </Card>
  {:else if tab === 'scans'}
    <Card>
      <div class="head">
        <h3>Scans</h3>
        {#if policy}
          <Button
            lockedReason={manageLock}
            onclick={() =>
              act(async () => {
                await inspectorApi.startScan(policy.id);
                toastStore.success('Scan queued');
              })}
          >
            Run scan now
          </Button>
        {/if}
      </div>
      {#if scans.length === 0}
        <EmptyState title="No scans yet" description="Run one, or set a schedule on the Policy tab." />
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <SortableTh key="started" sort={scanList.sort} onsort={onScanSort}>Started</SortableTh>
              <SortableTh key="finished" sort={scanList.sort} onsort={onScanSort}>
                Finished
              </SortableTh>
              <!-- `desc` (the default), not `asc`: a RANK — see
                   `SCAN_STATUS_ORDER` — so the first click leads with the
                   scans that failed. Coverage, below, is deliberately still
                   text; its alphabetical order already is its meaning. -->
              <SortableTh key="status" sort={scanList.sort} onsort={onScanSort}>
                Status
              </SortableTh>
              <SortableTh key="rows_scanned" class="num" sort={scanList.sort} onsort={onScanSort}>
                Rows scanned
              </SortableTh>
              <SortableTh key="findings" class="num" sort={scanList.sort} onsort={onScanSort}>
                Findings
              </SortableTh>
              <SortableTh key="coverage" sort={scanList.sort} onsort={onScanSort}>
                Coverage
              </SortableTh>
              <!-- Stop / CSV buttons: no value to order by. -->
              <th></th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each scanPage.rows as s (s.id)}
              <tr>
                <td><TimeValue value={s.started_at} /></td>
                <td><TimeValue value={s.finished_at} /></td>
                <td>
                  {#if s.status === 'running' || s.status === 'queued'}
                    <Spinner size={14} />
                  {/if}
                  <Badge>{s.status}</Badge>
                </td>
                <td class="num">{s.rows_scanned.toLocaleString()}</td>
                <td class="num">{s.findings_count.toLocaleString()}</td>
                <td>
                  <span title={s.coverage_note}>
                    <Badge tone={s.coverage === 'full' ? 'success' : 'warning'}>{s.coverage}</Badge>
                  </span>
                </td>
                <td>
                  {#if s.status === 'queued' || s.status === 'running'}
                    <Button
                      size="sm"
                      lockedReason={manageLock}
                      onclick={() => act(() => inspectorApi.cancelScan(s.id))}
                    >
                      Stop
                    </Button>
                  {/if}
                  <Button
                    size="sm"
                    onclick={() =>
                      act(
                        () =>
                          inspectorApi.downloadFindingsCsv(
                            s.id,
                            csvFilename(
                              'findings',
                              appSlug,
                              s.window_from.slice(0, 10),
                              s.window_to.slice(0, 10),
                            ),
                          ),
                        false,
                      )}
                  >
                    CSV
                  </Button>
                </td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>

        <!-- `total` is the length of the EXACT array handed to `pageSlice` —
             `scansSorted`, the same expression. Nothing filters this table; if
             anything ever does, the filtered array must feed both, and the
             filter change must reset the offset with `setOffsetPage`. -->
        <ClientPager
          offset={scanList.offset}
          limit={PAGE}
          total={scansSorted.length}
          onchange={(o) => (scanList = setOffsetPage(scanList, o))}
        />
      {/if}
    </Card>
  {:else}
    <Card>
      <h3>Mask audit trail</h3>
      <p class="caveat">
        Readable by anyone with <code>pii:read</code> — deliberately, and affordable precisely
        because these rows store paths and counts and never a value.
      </p>
      {#if actions.length === 0}
        <EmptyState title="Nothing masked yet" description="Mask a finding to start the trail." />
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <SortableTh key="when" sort={maskList.sort} onsort={onMaskSort}>When</SortableTh>
              <SortableTh key="who" columnDefault="asc" sort={maskList.sort} onsort={onMaskSort}>
                Who
              </SortableTh>
              <SortableTh key="targets" class="num" columnDefault="asc" sort={maskList.sort} onsort={onMaskSort}>
                Targets
              </SortableTh>
              <!-- `desc` (the default), not `asc`: a RANK — see
                   `MASK_STATUS_ORDER` — so the first click leads with the mask
                   actions that failed part-way. -->
              <SortableTh key="status" sort={maskList.sort} onsort={onMaskSort}>
                Status
              </SortableTh>
              <SortableTh key="rows_masked" class="num" sort={maskList.sort} onsort={onMaskSort}>
                Rows masked
              </SortableTh>
              <SortableTh key="cold_skipped" class="num" sort={maskList.sort} onsort={onMaskSort}>
                Cold skipped
              </SortableTh>
              <SortableTh key="cancelled_by" columnDefault="asc" sort={maskList.sort} onsort={onMaskSort}>
                Cancelled by
              </SortableTh>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each maskPage.rows as a (a.id)}
              <tr
                class="clickable"
                onclick={() => (expanded = { ...expanded, [a.id]: !expanded[a.id] })}
              >
                <td><TimeValue value={a.requested_at} /></td>
                <td>{a.requested_by_email || '—'}</td>
                <td class="num">{a.targets.length}</td>
                <td><Badge>{a.status}</Badge></td>
                <!-- rows_masked > estimated_rows is NORMAL on an actively
                     ingesting app, because preview and execution are separated
                     in time. Never render it as an error. -->
                <td class="num">{a.rows_masked.toLocaleString()}</td>
                <td class="num">{a.cold_rows_skipped.toLocaleString()}</td>
                <td>{a.cancelled_by_email || '—'}</td>
              </tr>
              {#if expanded[a.id]}
                <tr>
                  <td
                    colspan="7"
                    style="background: var(--surface-2); white-space: normal; cursor: default;"
                  >
                    <div class="detail" role="table" aria-label="Mask action detail">
                      {#each a.targets as t, i (i)}
                        <div role="row">
                          <span role="cell">Target</span>
                          <span role="cell">
                            <code>{t.table}.{t.column}{t.path ? `.${t.path}` : ''}</code>
                          </span>
                        </div>
                      {/each}
                      {#if a.error}
                        <div role="row">
                          <span role="cell">Error</span><span role="cell">{a.error}</span>
                        </div>
                      {/if}
                      {#if a.vacuum_advised}
                        <div role="row">
                          <span role="cell">Maintenance</span>
                          <span role="cell">
                            This pass rewrote enough rows that a VACUUM is worth scheduling.
                          </span>
                        </div>
                      {/if}
                      <h4>What this did not reach</h4>
                      {#each UNREACHABLE_COPY as r, i (i)}
                        <div role="row">
                          <span role="cell">{r.headline ? '' : r.what}</span>
                          <span role="cell" class:headline={r.headline}>
                            {r.headline ? r.what : `${r.why} — bounded by: ${r.bounded}`}
                          </span>
                        </div>
                      {/each}
                    </div>
                  </td>
                </tr>
              {/if}
            {/each}
          {/snippet}
        </DataTable>

        <!-- `total` is the length of the EXACT array handed to `pageSlice` —
             `masksSorted`, the same expression. -->
        <ClientPager
          offset={maskList.offset}
          limit={PAGE}
          total={masksSorted.length}
          onchange={(o) => (maskList = setOffsetPage(maskList, o))}
        />

        <!-- Exports the WHOLE trail, not the page on screen — the server builds
             the CSV from the app's actions and knows nothing about this pager. -->
        <Button
          size="sm"
          onclick={() => {
            if (!appId) return;
            void act(
              () =>
                inspectorApi.downloadMaskActionsCsv(
                  appId,
                  csvFilename('mask-actions', appSlug, '', ''),
                ),
              false,
            );
          }}
        >
          Export CSV
        </Button>
      {/if}
    </Card>
  {/if}
</AdminShell>

{#if maskTargetFinding && appId}
  <MaskDialog
    {appId}
    finding={maskTargetFinding}
    onclose={() => (maskTargetFinding = null)}
    ondone={() => {
      maskTargetFinding = null;
      tab = 'audit';
      void loadAll();
    }}
  />
{/if}

<style>
  .head {
    display: flex;
    align-items: baseline;
    gap: 12px;
    margin-bottom: 12px;
  }
  .muted {
    color: var(--text-muted);
    font-size: 12.5px;
  }
  /* --danger is not defined in app.css; the theme name is --error. An undefined
     custom property with no fallback invalidates the whole declaration, so this
     banner — the one that must render the 403 the cosmetic sidebar `show` does
     not prevent — inherited body colour and stopped reading as a failure. */
  .err {
    color: var(--error);
  }
  .tabs {
    display: flex;
    gap: 4px;
    border-bottom: 1px solid var(--border);
  }
  .tab {
    padding: 8px 14px;
    font-size: 13.5px;
    font-weight: 550;
    color: var(--text-muted);
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    cursor: pointer;
  }
  .tab:hover {
    color: var(--text);
  }
  .tab.active {
    color: var(--primary);
    border-bottom-color: var(--primary);
  }
  .count {
    display: inline-block;
    margin-left: 6px;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--surface-2);
    font-size: 11px;
  }
  .caveat {
    color: var(--text-muted);
    font-size: 12.5px;
    margin: 0 0 6px;
  }
  .detail {
    display: grid;
    gap: 6px;
    padding: 8px 0;
  }
  .detail [role='row'] {
    display: grid;
    grid-template-columns: 180px 1fr;
    gap: 12px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 6px 0;
  }
  .sel {
    padding: 6px 8px;
  }
  .addkey {
    display: flex;
    align-items: flex-start;
    gap: 8px;
  }
  .headline {
    font-weight: 600;
  }
  .create {
    display: grid;
    gap: 18px;
    margin-top: 4px;
  }
  /* fieldset/legend carry the grouping for a screen reader; the browser
     default border and inline legend are reset so it reads as a plain
     section, matching every other block on this tab. */
  .create .field {
    border: none;
    padding: 0;
    margin: 0;
    min-width: 0;
  }
  .create legend {
    padding: 0;
    margin-bottom: 4px;
    font-size: 13.5px;
    font-weight: 600;
  }
  .target {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    font-size: 13.5px;
  }
  .dets {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
    gap: 6px 12px;
  }
  .det {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    cursor: pointer;
  }
  .det input {
    cursor: pointer;
  }
  .blocked {
    color: var(--error);
    margin: 0;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }
</style>
