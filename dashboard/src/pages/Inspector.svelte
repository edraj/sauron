<!--
  PII inspector. Four hand-rolled tabs (there is no Tabs primitive and adding
  one is out of scope): Findings / Policy / Scans / Audit.

  Every destructive control is gated on `pii:manage` AT THE CURRENT APP, and
  the sidebar entry's `show` is cosmetic — the endpoint's 403 is the real gate.
-->
<script lang="ts">
  import { t } from '../lib/i18n';
  import { formatNumber } from '../lib/i18n';
  import { querystring, replace } from 'svelte-spa-router';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Skeleton from '../lib/components/ui/Skeleton.svelte';
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
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewKey } from '../lib/stores/view-cache';
  import Freshness from '../lib/components/ui/Freshness.svelte';
  import { lockedBy } from '../lib/models/page-access';
  import { lockTip } from '../lib/actions/lock-tip';
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
    inspectorTabFromQuery,
    inspectorTabRoute,
    type InspectorTab,
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

  /**
   * DERIVED from the URL, not held beside it. A `$state` copy synced by an
   * effect would be a second source of truth for the same fact, and the tab is
   * exactly the kind of fact that has to survive a reload, a Back press and a
   * pasted link.
   */
  const tab = $derived<InspectorTab>(inspectorTabFromQuery($querystring));
  const goTab = (next: InspectorTab) => replace(inspectorTabRoute(next, $querystring));


  // $state.raw, not $state: these are replaced wholesale on every reload and
  // deep-proxying them makes `===` never match a raw row, which breaks the
  // "is this the row I opened?" check in the expand map below.
  /**
   * One payload, because these six are ONE read.
   *
   * `loadAll` is a dependent chain — the policy decides whether scans are
   * fetched, and the newest succeeded scan decides which findings are. Caching
   * them separately would let a cache hit on one pair with a miss on another
   * and paint a policy beside findings from a different scan. As one entry
   * they are always mutually consistent.
   */
  interface InspectorPayload {
    effective: EffectivePolicy | null;
    scans: InspectorScan[];
    findings: InspectorFinding[];
    actions: InspectorMaskAction[];
    coverageNote: string;
    detectionCaveat: string;
  }
  const EMPTY_PAYLOAD: InspectorPayload = {
    effective: null,
    scans: [],
    findings: [],
    actions: [],
    coverageNote: '',
    detectionCaveat: '',
  };

  const view = new CachedView<InspectorPayload>();
  const payload = $derived(view.data ?? EMPTY_PAYLOAD);
  const effective = $derived(payload.effective);
  const scans = $derived(payload.scans);
  const findings = $derived(payload.findings);
  const actions = $derived(payload.actions);
  const coverageNote = $derived(payload.coverageNote);
  const detectionCaveat = $derived(payload.detectionCaveat);
  const revalidating = $derived(view.revalidating);
  const loading = $derived(view.loading);
  /**
   * `errorMessage`, not `e.message`: every rejection from `api` is a
   * NormalizedError PLAIN OBJECT, so `e instanceof Error` is false and the
   * banner would read "[object Object]" for the 403 that is this page's real
   * gate. `CachedView` already normalises, which is why this is now derived.
   */
  const error = $derived(view.error ?? '');
  let expanded = $state<Record<string, boolean>>({});
  let revealed = $state<Record<string, unknown>>({});
  let maskTargetFinding = $state.raw<InspectorFinding | null>(null);
  let newKey = $state('');

  /**
   * The strip, in order, with the count each tab shows.
   *
   * `null` means "this tab has no count", not "zero" — Policy is a single form,
   * so a badge reading 0 beside it would be describing nothing.
   */
  const TAB_META = $derived<{ key: InspectorTab; label: string; count: number | null }[]>([
    { key: 'findings', label: 'Findings', count: findings.length },
    { key: 'policy', label: 'Policy', count: null },
    { key: 'scans', label: 'Scans', count: scans.length },
    { key: 'audit', label: 'Audit', count: actions.length },
  ]);

  /**
   * Left/Right/Home/End across the strip, wrapping at both ends.
   *
   * Bound to each TAB, not to the `tablist` container. A tablist must not be
   * focusable itself — the roving `tabindex` on the tabs is the whole point —
   * and a container carrying an interactive role plus a key handler is both an
   * a11y-lint failure and a thing that can never receive the event anyway.
   *
   * Moves focus as well as selection: with roving `tabindex` the previously
   * selected button leaves the tab order the moment selection changes, so
   * without this the focus ring would land on nothing and keyboard navigation
   * would dead-end after one press.
   */
  function onTabKeydown(e: KeyboardEvent) {
    const keys = ['ArrowLeft', 'ArrowRight', 'Home', 'End'];
    if (!keys.includes(e.key)) return;
    e.preventDefault();
    const order = TAB_META.map((tab) => tab.key);
    const at = order.indexOf(tab);
    const next =
      e.key === 'Home'
        ? order[0]
        : e.key === 'End'
          ? order[order.length - 1]
          : order[(at + (e.key === 'ArrowRight' ? 1 : order.length - 1)) % order.length];
    goTab(next);
    // After the route change re-renders the strip; querying by id rather than
    // holding element refs keeps this working when the list changes.
    queueMicrotask(() => document.getElementById(`tab-${next}`)?.focus());
  }

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
    const aid = appId;
    // `quiet` used to mean "do not raise the skeleton". Under the cached view
    // that is the default for anything already on screen — a reload with rows
    // up is a revalidate, never a `loading` — so it only decides whether the
    // network is forced.
    await view.load(
      viewKey('inspector.page', aid, sessionStore.scopeKey),
      async () => {
        const effective = await inspectorApi.effectivePolicy(aid);
        const actions = await inspectorApi.listAppMaskActions(aid);
        if (!effective.policy) {
          return { ...EMPTY_PAYLOAD, effective, actions };
        }
        const scans = await inspectorApi.listScans(effective.policy.id);
        const latest = scans.find((s) => s.status === 'succeeded') ?? scans[0];
        if (!latest) {
          return { ...EMPTY_PAYLOAD, effective, actions, scans };
        }
        const page = await inspectorApi.listFindings(latest.id);
        return {
          effective,
          actions,
          scans,
          findings: page.findings,
          coverageNote: page.coverage === 'partial' ? page.coverage_note : '',
          detectionCaveat: page.detection_caveat,
        };
      },
      !quiet,
    );
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

<AdminShell>
  <div class="head">
    <h1 class="page-title">
      {t('inspector.title')}
      <Freshness fetchedAt={view.fetchedAt} {revalidating} />
    </h1>
    {#if effective}
      <span class="muted">
        New events are masked within about {effective.enforcement_latency_secs} seconds of a change.
      </span>
    {/if}
  </div>

  <!--
    A real `tablist`, not four buttons that look like one. They already behaved
    like tabs to a mouse; to a screen reader they were unlabelled buttons with
    no indication that one was current or that they controlled the region
    below, and Left/Right did nothing. `tabindex` is roving — only the selected
    tab is in the tab order, so Tab moves past the whole strip to the panel,
    which is the ARIA authoring-practices behaviour people expect from tabs.
  -->
  <div class="tabs" role="tablist" aria-label={t('inspector.sections')}>
    {#each TAB_META as meta (meta.key)}
      <button
        class="tab"
        class:active={tab === meta.key}
        role="tab"
        id={`tab-${meta.key}`}
        aria-selected={tab === meta.key}
        aria-controls="inspector-panel"
        tabindex={tab === meta.key ? 0 : -1}
        onclick={() => goTab(meta.key)}
        onkeydown={onTabKeydown}
      >
        {meta.label}
        <!--
          Suppressed until the first load resolves. Rendering `0` while the
          request is still out states a fact we do not have yet, and "Findings
          0" is the same shape as a clean scan — so the strip announced "nothing
          to see" and then silently changed its mind.
        -->
        {#if !loading && meta.count !== null}
          <span class="count">{meta.count}</span>
        {/if}
      </button>
    {/each}
  </div>

  <div class="content" id="inspector-panel" role="tabpanel" aria-labelledby={`tab-${tab}`}>
  {#if error}
    <Card><p class="err">{error}</p></Card>
  {:else if loading}
    <Card><Skeleton rows={6} label={t('inspector.loading')} /></Card>
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
      <EmptyState
        title={t('inspector.empty.noFindings')}
        description={t('inspector.empty.noFindingsBody')}
      >
        {#snippet action()}
          <Button variant="secondary" onclick={() => goTab('scans')}>{t('inspector.goToScans')}</Button>
        {/snippet}
      </EmptyState>
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
                  {t('inspector.column.path')}
                </SortableTh>
                <SortableTh
                  key="type"
                  columnDefault="asc"
                  sort={gl.sort}
                  onsort={(k, d) => onFindingSort(g.key, k, d)}
                >
                  {t('monitors.column.type')}
                </SortableTh>
                <SortableTh
                  key="matches"
                  class="num"
                  sort={gl.sort}
                  onsort={(k, d) => onFindingSort(g.key, k, d)}
                >
                  {t('inspector.column.matches')}
                </SortableTh>
                <SortableTh
                  key="last_seen"
                  sort={gl.sort}
                  onsort={(k, d) => onFindingSort(g.key, k, d)}
                >
                  {t('explore.column.lastSeen')}
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
                        <Icon name="eye-off" size={14} /> {t('inspector.mask')}
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
                      <div class="detail" role="table" aria-label={t('inspector.findingDetail')}>
                        <div role="row">
                          <span role="cell">{t('inspector.redactedPreview')}</span>
                          <span role="cell"><code>{f.sample_preview}</code></span>
                        </div>
                        <div role="row">
                          <span role="cell">{t('nav.env')}</span>
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
                            {t('inspector.revealOne')}
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
      <h3>{t('inspector.inspection')}</h3>
      {#if !policy}
        <!-- This used to be a bare EmptyState reading "Create one from the
             organization settings" — a dead pointer: there is no org settings
             screen, and `createPolicy` had no call site anywhere in the
             dashboard, so no role could create a policy at all. The form is
             here, where the wall is. -->
        <EmptyState
          title={t('inspector.empty.noPolicy')}
          description={t('inspector.empty.noPolicyBody')}
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
            <legend>{t('members.column.scope')}</legend>
            <p class="caveat">
              {t('prose.inspector.precedence')}
            </p>
            <div class="chips">
              {#each TARGET_TYPES as tt (tt.value)}
                <Button
                  size="sm"
                  variant={newTargetType === tt.value ? 'primary' : 'ghost'}
                  lockedReason={manageLock}
                  disabled={tt.value === 'app_env' && envOptions.length === 0}
                  title={tt.value === 'app_env' && envOptions.length === 0
                    ? 'This app has no active environments'
                    : tt.hint}
                  onclick={() => (newTargetType = tt.value)}
                >
                  {tt.label}
                </Button>
              {/each}
            </div>
          </fieldset>

          <fieldset class="field">
            <legend>{t('inspector.target')}</legend>
            {#if newTargetType === 'app_env'}
              <!-- `env.id` is the app_environments ENROLLMENT id, not the
                   catalogue `environment_id` — `validate_scope_in_org` matches
                   `app_environments.id` for app_env, and sending the catalogue
                   id would 404 with no hint as to why. -->
              <select
                class="sel"
                aria-label={t('inspector.targetEnvironment')}
                use:lockTip={manageLock}
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
            <legend>{t('inspector.trackedKeys')}</legend>
            <p class="caveat">
              {t('inspector.literalKeys')}
              <code>{t('common.email')}</code> matches <code>email</code>; <code>user_email</code> does not.
              Separate with commas or spaces.
            </p>
            <Input
              bind:value={newKeyInput}
              disabled={manageLock !== null}
              placeholder={t('inspector.placeholder.keys')}
            />
            {#if newKeys.length > 0}
              <div class="chips">
                {#each newKeys as k (k.key)}<Badge>{k.key}</Badge>{/each}
              </div>
            {/if}
          </fieldset>

          <fieldset class="field">
            <legend>{t('inspector.detectors')}</legend>
            <p class="caveat">
              {t('prose.inspector.shapeDetectors')}
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
              {t('inspector.createPolicy')}
            </Button>
            <span class="caveat">
              {t('inspector.scanSeparate')}
            </span>
          </div>
        </form>
      {:else}
        <p>
          {t('prose.inspector.scopeLabel')} <Badge>{policy.target_type}</Badge>
          {t('prose.inspector.statusLabel')}
          <Badge tone={policy.enabled ? 'success' : 'neutral'}>
            {policy.enabled ? t('prose.inspector.enabled') : t('prose.inspector.disabled')}
          </Badge>
        </p>
        <!-- There is no Toggle primitive, so this is a Button plus a Badge. -->
        <Button
          lockedReason={manageLock}
          onclick={() => act(() => inspectorApi.patchPolicy(policy.id, { enabled: !policy.enabled }))}
        >
          {policy.enabled ? 'Disable' : 'Enable'}
        </Button>

        <h4>{t('inspector.trackedKeys')}</h4>
        <p class="caveat">
          {t('inspector.keyMatchNote')}
          <code>{t('common.email')}</code> matches <code>email</code>; <code>user_email</code> does not.
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
            <Input bind:value={newKey} placeholder={t('inspector.placeholder.addKey')} />
            <Button type="submit" size="sm" lockedReason={manageLock}>{t('filter.add')}</Button>
          </form>

        <h4>{t('inspector.schedule')}</h4>
        <p>{describeSchedule(policy.schedule_days, policy.schedule_time, policy.schedule_tz)}</p>
        {#if DST_RISK_HOURS.includes(Number.parseInt(policy.schedule_time.slice(0, 2), 10))}
          <p class="caveat">
            {t('prose.inspector.dst')}
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
          aria-label={t('inspector.scheduleTimezone')}
          use:lockTip={manageLock}
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
      <h3>{t('inspector.forwardEnforcement')}</h3>
      <p class="caveat">
        New events are masked within about {effective?.enforcement_latency_secs} seconds of a change.
      </p>
      {#if (effective?.masked_keys ?? []).length === 0}
        <EmptyState title={t('inspector.empty.nothingMasked')} description={t('inspector.empty.maskToEnforce')} />
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
        <h3>{t('inspector.tab.scans')}</h3>
        {#if policy}
          <Button
            lockedReason={manageLock}
            onclick={() =>
              act(async () => {
                await inspectorApi.startScan(policy.id);
                toastStore.success('Scan queued');
              })}
          >
            {t('inspector.runScan')}
          </Button>
        {/if}
      </div>
      {#if scans.length === 0}
        <EmptyState title={t('inspector.empty.noScans')} description={t('inspector.empty.noScansBody')}>
          {#snippet action()}
            <Button variant="secondary" onclick={() => goTab('policy')}>{t('inspector.setSchedule')}</Button>
          {/snippet}
        </EmptyState>
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <SortableTh key="started" sort={scanList.sort} onsort={onScanSort}>{t('explore.column.started')}</SortableTh>
              <SortableTh key="finished" sort={scanList.sort} onsort={onScanSort}>
                {t('inspector.column.finished')}
              </SortableTh>
              <!-- `desc` (the default), not `asc`: a RANK — see
                   `SCAN_STATUS_ORDER` — so the first click leads with the
                   scans that failed. Coverage, below, is deliberately still
                   text; its alphabetical order already is its meaning. -->
              <SortableTh key="status" sort={scanList.sort} onsort={onScanSort}>
                {t('common.status')}
              </SortableTh>
              <SortableTh key="rows_scanned" class="num" sort={scanList.sort} onsort={onScanSort}>
                {t('inspector.column.rowsScanned')}
              </SortableTh>
              <SortableTh key="findings" class="num" sort={scanList.sort} onsort={onScanSort}>
                {t('inspector.tab.findings')}
              </SortableTh>
              <SortableTh key="coverage" sort={scanList.sort} onsort={onScanSort}>
                {t('inspector.coverage')}
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
                <td class="num">{formatNumber(s.rows_scanned)}</td>
                <td class="num">{formatNumber(s.findings_count)}</td>
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
                      {t('inspector.stop')}
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
      <h3>{t('inspector.maskAuditTrail')}</h3>
      <p class="caveat">
        {t('inspector.readableBy')} <code>pii:read</code> — deliberately, and affordable precisely
        because these rows store paths and counts and never a value.
      </p>
      {#if actions.length === 0}
        <EmptyState title={t('inspector.empty.nothingMaskedTrail')} description={t('inspector.empty.maskToTrail')} />
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <SortableTh key="when" sort={maskList.sort} onsort={onMaskSort}>{t('ui.opModal.when')}</SortableTh>
              <SortableTh key="who" columnDefault="asc" sort={maskList.sort} onsort={onMaskSort}>
                {t('audit.column.who')}
              </SortableTh>
              <SortableTh key="targets" class="num" columnDefault="asc" sort={maskList.sort} onsort={onMaskSort}>
                {t('inspector.targets')}
              </SortableTh>
              <!-- `desc` (the default), not `asc`: a RANK — see
                   `MASK_STATUS_ORDER` — so the first click leads with the mask
                   actions that failed part-way. -->
              <SortableTh key="status" sort={maskList.sort} onsort={onMaskSort}>
                {t('common.status')}
              </SortableTh>
              <SortableTh key="rows_masked" class="num" sort={maskList.sort} onsort={onMaskSort}>
                {t('inspector.column.rowsMasked')}
              </SortableTh>
              <SortableTh key="cold_skipped" class="num" sort={maskList.sort} onsort={onMaskSort}>
                {t('inspector.column.coldSkipped')}
              </SortableTh>
              <SortableTh key="cancelled_by" columnDefault="asc" sort={maskList.sort} onsort={onMaskSort}>
                {t('inspector.cancelledBy')}
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
                <td class="num">{formatNumber(a.rows_masked)}</td>
                <td class="num">{formatNumber(a.cold_rows_skipped)}</td>
                <td>{a.cancelled_by_email || '—'}</td>
              </tr>
              {#if expanded[a.id]}
                <tr>
                  <td
                    colspan="7"
                    style="background: var(--surface-2); white-space: normal; cursor: default;"
                  >
                    <div class="detail" role="table" aria-label={t('inspector.maskActionDetail')}>
                      {#each a.targets as tgt, i (i)}
                        <div role="row">
                          <span role="cell">{t('inspector.target')}</span>
                          <span role="cell">
                            <code>{tgt.table}.{tgt.column}{tgt.path ? `.${tgt.path}` : ''}</code>
                          </span>
                        </div>
                      {/each}
                      {#if a.error}
                        <div role="row">
                          <span role="cell">{t('issues.stat.error')}</span><span role="cell">{a.error}</span>
                        </div>
                      {/if}
                      {#if a.vacuum_advised}
                        <div role="row">
                          <span role="cell">{t('inspector.maintenance')}</span>
                          <span role="cell">
                            {t('inspector.vacuumHint')}
                          </span>
                        </div>
                      {/if}
                      <h4>{t('inspector.notReached')}</h4>
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
          {t('explore.exportCsv')}
        </Button>
      {/if}
    </Card>
  {/if}
  </div>
</AdminShell>

{#if maskTargetFinding && appId}
  <MaskDialog
    {appId}
    finding={maskTargetFinding}
    onclose={() => (maskTargetFinding = null)}
    ondone={() => {
      maskTargetFinding = null;
      goTab('audit');
      void loadAll();
    }}
  />
{/if}

<style>
  .content {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
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
  /* Roving tabindex means keyboard users land here with no other affordance —
     the browser default outline is clipped by the strip's bottom border. */
  .tab:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: -2px;
    border-radius: var(--radius-sm) var(--radius-sm) 0 0;
  }
  .tab.active {
    color: var(--primary);
    border-bottom-color: var(--primary);
  }
  .count {
    display: inline-block;
    margin-inline-start: 6px;
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
