<script lang="ts">
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import {
    getAdminStorage,
    getTierPolicy,
    setTierPolicy,
    createRestore,
    listRestores,
    releasePin,
    extendPin,
    RESTORABLE_TABLES,
  } from '../lib/api/admin';
  import type {
    StorageReport,
    TierPolicy,
    RestoreJob,
    RestorableTable,
  } from '../lib/api/admin';
  import Button from '../lib/components/ui/Button.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import {
    parseWholeDays,
    isHotDaysValid,
    parseRestoreDays,
    isHotDaysDirty,
    revertWouldLower,
    describeRevert,
    RESTORE_MIN_DAYS,
    RESTORE_MAX_DAYS,
  } from '../lib/models/tier-policy';
  import {
    STORAGE_APP_DEFAULT_SORT,
    STORAGE_TABLE_DEFAULT_SORT,
    storageAppAccessor,
    storageTableAccessor,
  } from '../lib/models/storage-sort';
  import { sortRows } from '../lib/models/sort-rows';
  import { toggleSort, type SortDir, type SortState } from '../lib/models/sort';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewCache, viewKey } from '../lib/stores/view-cache';

  // Cached view (lib/stores/cached-view.svelte.ts): the report paints instantly
  // on a revisit and refreshes behind the tables. `/v1/admin/storage` walks
  // `pg_partition_tree` across every partition, so it is one of the slower
  // reads in the dashboard and the one most worth not repeating on every visit.
  //
  // Not keyed on a scope: the endpoint is org-scoped server-side and returns
  // exactly the orgs you manage, so the caller has no id to vary it by. The
  // whole cache is dropped on logout, which is what keeps a second user on the
  // same tab from being served the first one's report.
  const view = new CachedView<StorageReport>();
  const report = $derived(view.data ?? null);
  const loading = $derived(view.loading);
  const error = $derived(view.error);

  // Both tables arrive whole in the one `/v1/admin/storage` response — one row
  // per tiered table, one per visible app — so each sort runs over the ENTIRE
  // array its table renders. Neither gets a pager: the table list is a fixed
  // handful and the app list is bounded by the orgs you manage, and a pager on
  // either would imply a page two that does not exist.
  //
  // A bare `SortState` per table, not the `OffsetListState` the paginated
  // tables use: that type exists to make "apply a sort" and "reset to page 1"
  // one indivisible step, and with no offset there is nothing to reset. Its
  // `key`/`dir` are `readonly` (see `sort.ts`), so `tableSort.dir = 'asc'` is
  // a type error and every transition goes through `toggleSort`.
  let tableSort = $state<SortState>(STORAGE_TABLE_DEFAULT_SORT);
  let appSort = $state<SortState>(STORAGE_APP_DEFAULT_SORT);

  // `sortRows` copies before sorting, so the arrays inside `report` — the very
  // object the poll below replaces wholesale — are never reordered in place.
  // Every byte column orders by its RAW byte count, never by `fmtBytes`'s
  // label: "900 KB" above "1.2 GB" is what text ordering gives, on the one
  // page whose job is saying what is big.
  const sortedTables = $derived(
    sortRows(report?.database.tables ?? [], storageTableAccessor(tableSort.key), tableSort.dir),
  );
  const sortedApps = $derived(
    sortRows(report?.apps ?? [], storageAppAccessor(appSort.key), appSort.dir),
  );

  function onTableSort(key: string, columnDefault: SortDir) {
    tableSort = toggleSort(tableSort, key, columnDefault);
  }
  function onAppSort(key: string, columnDefault: SortDir) {
    appSort = toggleSort(appSort, key, columnDefault);
  }

  // Rotation policy. Loaded separately from the storage report and allowed to
  // fail on its own: the endpoint requires org:manage in EVERY org, so an admin
  // of one tenant gets a 403 here while the storage report above still renders.
  // Treating that as a page-level error would break Storage for them entirely.
  let policy = $state<TierPolicy | null>(null);
  /**
   * Split deliberately. A load failure means there is no policy to show, so it
   * replaces the card. An ACTION failure — a rejected Apply, a pin that would
   * not release — must not: it used to share one variable with the load error,
   * so a single 400 unmounted the input, both buttons, the facts and the pin
   * list, and the only route back was a page reload (the poll that could have
   * re-fetched runs only while a restore job is active).
   */
  let policyLoadError = $state<string | null>(null);
  let policyActionError = $state<string | null>(null);
  let policyBusy = $state(false);
  let hotDaysInput = $state('');
  /** Last value written into the field from the server. See `isHotDaysDirty`. */
  let seededHotDays = $state('');
  let policySaved = $state(false);
  const hotDaysDirty = $derived(isHotDaysDirty(hotDaysInput, seededHotDays));

  // Which app rows are expanded to show their cold Parquet file inventory.
  let openApp = $state<Record<string, boolean>>({});
  function toggleApp(appId: string) {
    openApp = { ...openApp, [appId]: !openApp[appId] };
  }

  /** `force` bypasses the fresh window — an explicit Refresh means "go now". */
  async function load(force = false) {
    await view.load(viewKey('admin.storage'), () => getAdminStorage(), force);
  }

  async function refresh() {
    viewCache.invalidate('admin.storage');
    await load(true);
  }

  async function loadPolicy() {
    policyLoadError = null;
    try {
      policy = await getTierPolicy();
      // Never over an edit in progress: this runs from a 3s poll while a
      // restore job is active, and reseeding unconditionally is what made the
      // field impossible to type in. `policy` itself still updates, so the
      // "In force" row below keeps showing the server's value — the input is
      // the draft, the facts are the truth.
      if (!hotDaysDirty) {
        const seeded = String(policy.effective_hot_days);
        hotDaysInput = seeded;
        seededHotDays = seeded;
      }
    } catch (e) {
      policy = null;
      policyLoadError = (e as Error).message;
    }
  }

  /**
   * `next` is deliberately non-nullable, and separate from `revertPolicy`
   * below. On the wire `hot_days: null` means "clear the override", which is
   * the Revert button's job — and when the override sits above the configured
   * default, clearing it lowers the rotation age, the one change this page
   * warns is irreversible. Keeping null out of this signature makes
   * `savePolicy(parsedHotDays)` a compile error the moment `parsedHotDays`
   * becomes nullable again, rather than a silent revert.
   */
  async function savePolicy(next: number) {
    await putPolicy(next);
  }

  /**
   * The explicit "back to the configured default" path.
   *
   * Guarded when it would LOWER the rotation age, because then it is not an
   * undo — it is the same irreversible export-and-drop the typed-value warning
   * covers, reached from one click. Reverting upward destroys nothing, so it
   * goes straight through rather than training people to dismiss the dialog.
   */
  let confirmRevert = $state(false);

  function askRevert() {
    if (policy !== null && revertWouldLower(policy)) {
      confirmRevert = true;
      return;
    }
    void revertPolicy();
  }

  async function revertPolicy() {
    // Closed AFTER the request, not before, so `loading` on the dialog is real:
    // the confirm button spins and Cancel is disabled while the PUT is in
    // flight, rather than the dialog vanishing and leaving a destructive action
    // with no visible progress.
    await putPolicy(null);
    confirmRevert = false;
  }

  async function putPolicy(next: number | null) {
    policyBusy = true;
    policyActionError = null;
    policySaved = false;
    try {
      policy = await setTierPolicy(next);
      const seeded = String(policy.effective_hot_days);
      hotDaysInput = seeded;
      seededHotDays = seeded;
      policySaved = true;
    } catch (e) {
      policyActionError = (e as Error).message;
    } finally {
      policyBusy = false;
    }
  }

  // Parsed once so the button's disabled state and the submit path can never
  // disagree about whether the input is valid. The field is a TEXT input on
  // purpose — see lib/models/tier-policy.ts for why a number input made this
  // parse both crash and, worse, silently round a mis-typed value down.
  const parsedHotDays = $derived(parseWholeDays(hotDaysInput));
  const hotDaysValid = $derived(
    policy !== null && isHotDaysValid(parsedHotDays, policy.min_hot_days),
  );
  const wouldLower = $derived(
    policy !== null && parsedHotDays !== null && parsedHotDays < policy.effective_hot_days,
  );

  // -------------------------------------------------------------------------
  // Cold-data restore
  // -------------------------------------------------------------------------

  let jobs = $state<RestoreJob[]>([]);
  let restoreTable = $state<RestorableTable>('error_events');
  let restoreFrom = $state('');
  let restoreTo = $state('');
  let restoreDays = $state('30');
  let restoreBusy = $state(false);
  let restoreError = $state<string | null>(null);
  let pinBusy = $state<string | null>(null);

  /**
   * True while any job could still change. Drives the poll, so a finished queue
   * stops hitting the server rather than polling forever at 3s.
   */
  const jobsActive = $derived(jobs.some((j) => j.status === 'queued' || j.status === 'running'));

  const restoreRange = $derived.by(() => {
    if (!restoreFrom || !restoreTo) return null;
    // Dates are entered as plain days; the API takes half-open [start, end) in
    // UTC. Adding a day to `to` is what makes the picker inclusive of the last
    // day, which is what a human means by "the 1st to the 3rd".
    const start = new Date(`${restoreFrom}T00:00:00Z`);
    const end = new Date(`${restoreTo}T00:00:00Z`);
    if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) return null;
    end.setUTCDate(end.getUTCDate() + 1);
    if (end <= start) return null;
    return { start, end };
  });

  const parsedRestoreDays = $derived(parseRestoreDays(restoreDays));
  const restoreDaysValid = $derived(parsedRestoreDays !== null);

  const restoreValid = $derived(restoreRange !== null && restoreDaysValid && !restoreBusy);

  async function loadJobs() {
    try {
      jobs = await listRestores();
    } catch {
      // Deliberately quiet: the job list is secondary to the policy card, and
      // the same 403 that hides the policy hides this. `policyLoadError` above
      // already says so once.
    }
  }

  async function submitRestore() {
    const range = restoreRange;
    const days = parsedRestoreDays;
    if (!range || days === null) return;
    restoreBusy = true;
    restoreError = null;
    try {
      await createRestore({
        table_name: restoreTable,
        range_start: range.start.toISOString(),
        range_end: range.end.toISOString(),
        expires_in_days: days,
      });
      await loadJobs();
    } catch (e) {
      restoreError = (e as Error).message;
    } finally {
      restoreBusy = false;
    }
  }

  async function doReleasePin(id: string) {
    pinBusy = id;
    policyActionError = null;
    try {
      await releasePin(id);
      await loadPolicy();
    } catch (e) {
      policyActionError = (e as Error).message;
    } finally {
      pinBusy = null;
    }
  }

  async function doExtendPin(id: string) {
    pinBusy = id;
    policyActionError = null;
    try {
      await extendPin(id, 30);
      await loadPolicy();
    } catch (e) {
      policyActionError = (e as Error).message;
    } finally {
      pinBusy = null;
    }
  }

  // A restore copies rows back into hot storage, so the byte counts in the
  // report are stale the moment one finishes. Refresh when the last active job
  // settles — the poll below is what drives `jobsActive` false.
  //
  // A PLAIN let, not `$state`: an effect that both reads and writes the same
  // reactive value re-triggers on its own write. Nothing renders this, so it
  // has no reason to be reactive.
  let wasJobsActive = false;
  $effect(() => {
    const active = jobsActive;
    if (wasJobsActive && !active) void refresh();
    wasJobsActive = active;
  });

  // Poll only while something is in flight. A restore is a background copy that
  // can take minutes, so the create call returns a queued job and this is what
  // turns it into visible progress.
  $effect(() => {
    if (!jobsActive) return;
    const t = setInterval(() => {
      void loadJobs();
      // The pin appears only once the worker creates it, so the pin list has to
      // refresh alongside the job or a completed restore shows no protection.
      void loadPolicy();
    }, 3000);
    return () => clearInterval(t);
  });

  function jobPercent(j: RestoreJob): number {
    if (j.status === 'succeeded') return 100;
    if (j.rows_estimated <= 0) return 0;
    return Math.min(100, Math.round((j.rows_restored / j.rows_estimated) * 100));
  }

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    const u = ['KB', 'MB', 'GB', 'TB'];
    let v = n / 1024, i = 0;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return `${v.toFixed(1)} ${u[i]}`;
  }

  $effect(() => {
    void load();
    void loadPolicy();
    void loadJobs();
  });
</script>

<AdminShell requireProject={false}>
  <div class="storage">
    <header class="head">
      <div>
        <h1 class="page-title">Storage</h1>
        <!-- Wording tracks `full_scope`. When the caller manages every org there
             is no other tenant to leak, so the figures are real physical bytes
             (pg_database_size / pg_total_relation_size) and must NOT be called
             estimates. Otherwise they are the physical size apportioned by row
             share, which is still an estimate — just a far better one than the
             old rows × avg_width, which omitted indexes, TOAST, page overhead
             and bloat and so read several times low. -->
        <p class="sub muted">
          {#if report?.database.full_scope}
            Actual storage on disk across the deployment, with per-app hot/cold record
            counts. Postgres figures include indexes and overhead.
          {:else}
            Estimated storage across the organisations you manage, with per-app hot/cold
            record counts.
          {/if}
        </p>
      </div>
    </header>

    {#if error}
      <div class="err-banner" role="alert">
        <Icon name="triangle-alert" size={15} />
        <span>{error}</span>
      </div>
    {/if}

    {#if loading}
      <div class="center"><Spinner size={24} /></div>
    {:else if report}
      {@const rep = report}
      {@const dbBytes = rep.database.physical_bytes ?? rep.database.total_bytes}
      {@const coldBytes = rep.database.cold_bytes ?? 0}
      <StatTiles min={180}>
        <StatTile
          label={rep.database.full_scope ? 'Database (Postgres)' : 'Estimated size'}
          value={fmtBytes(dbBytes)}
          tone="primary"
        />
        <StatTile label="Cold (Parquet)" value={fmtBytes(coldBytes)} />
        <StatTile label="Total" value={fmtBytes(dbBytes + coldBytes)} />
        <StatTile label="Tables" value={rep.database.tables.length} />
        <StatTile label="Apps" value={rep.apps.length} />
      </StatTiles>

      <div class="section">
        <Card title="Cold-tier rotation">
          {#if policyLoadError}
            <!-- Shown inline, not as a page error: this endpoint needs org:manage in
                 every org, so a single-tenant admin legitimately gets a 403 while the
                 rest of this page still works for them.

                 Only a LOAD failure belongs here. An action that fails leaves the
                 policy perfectly displayable, so it renders inside the card below —
                 replacing the card would strand the user with no way to retry. -->
            <p class="policy-denied muted">{policyLoadError}</p>
          {:else if policy}
            {@const pol = policy}
            <p class="muted policy-lede">
              Data older than this moves out of Postgres into Parquet. It stays
              readable — queries span both tiers — but it no longer occupies
              database storage.
            </p>

            <div class="policy-row">
              <label class="policy-field">
                <span class="policy-label">Rotation age (days)</span>
                <!-- Text, not type="number": the binding on a number input
                     hands back a coerced float, which both broke the parse
                     below and turned a mis-typed "3.0" into a saved 3-day
                     rotation. tier-policy.ts has the full reasoning. -->
                <input
                  class="policy-input"
                  type="text"
                  inputmode="numeric"
                  bind:value={hotDaysInput}
                  disabled={policyBusy}
                />
              </label>
              <Button
                variant="primary"
                disabled={!hotDaysValid || policyBusy}
                onclick={() => {
                  // Belt and braces with the `disabled` above: that guard is a
                  // derived value, and a derived that throws leaves the button
                  // frozen at its last state — which is exactly how this card
                  // first broke. submitRestore re-checks for the same reason.
                  if (parsedHotDays !== null) savePolicy(parsedHotDays);
                }}
              >
                {policyBusy ? 'Saving…' : 'Apply'}
              </Button>
              {#if pol.overridden}
                <Button variant="secondary" disabled={policyBusy} onclick={askRevert}>
                  Revert to default ({pol.configured_hot_days}d)
                </Button>
              {/if}
            </div>

            {#if parsedHotDays !== null && !hotDaysValid}
              <p class="policy-warn" role="alert">
                Must be a whole number of days, at least {pol.min_hot_days}. A smaller
                value would put the cutoff at or after now and tier partitions that are
                still being written to.
              </p>
            {:else if wouldLower}
              <!-- The asymmetry is the single most important thing on this page.
                   Lowering acts on the next cycle and cannot be undone by raising
                   the number back. -->
              <p class="policy-warn" role="alert">
                <Icon name="triangle-alert" size={14} />
                Lowering this is one-way. On its next cycle the tier worker will export
                and then drop everything between {parsedHotDays} and
                {pol.effective_hot_days} days old. Raising the number afterwards does
                not bring it back into Postgres — that needs a restore from cold.
              </p>
            {/if}

            {#if policyActionError}
              <p class="policy-err" role="alert">
                <Icon name="triangle-alert" size={14} />
                {policyActionError}
              </p>
            {/if}

            <!-- Hidden the moment the field says something else: otherwise the
                 page goes on asserting that an unsaved number is in force. -->
            {#if policySaved && !hotDaysDirty}
              <p class="policy-ok">Saved. Takes effect on the tier worker's next cycle.</p>
            {/if}

            <dl class="policy-facts">
              <div>
                <dt>In force</dt>
                <dd>{pol.effective_hot_days} days{pol.overridden ? '' : ' (default)'}</dd>
              </div>
              <div>
                <dt>Configured</dt>
                <dd>{pol.configured_hot_days} days (TIER_HOT_DAYS)</dd>
              </div>
            </dl>

            {#if pol.follows_on_restart.length > 0}
              <details class="policy-detail">
                <summary>Not every component picks this up immediately</summary>
                <p class="muted">
                  Applies without a restart: {pol.follows_immediately.join('; ')}. Still
                  reading start-time configuration, and so able to disagree about where
                  the boundary is until restarted:
                </p>
                <ul class="muted">
                  {#each pol.follows_on_restart as c (c)}
                    <li>{c}</li>
                  {/each}
                </ul>
              </details>
            {/if}

            {#if pol.pins.length > 0}
              <div class="policy-pins">
                <h3 class="pins-title">Restored ranges held in Postgres</h3>
                <p class="muted">
                  Each pin keeps a restored range from being re-tiered. Without one, a
                  restore is undone on the next cycle.
                </p>
                <ul class="pin-list">
                  {#each pol.pins as pin (pin.id)}
                    <li class:expired={pin.expired} class:soon={pin.expiring_soon}>
                      <div class="pin-main">
                        <code>{pin.table_name}</code>
                        {new Date(pin.range_start).toISOString().slice(0, 10)} →
                        {new Date(pin.range_end).toISOString().slice(0, 10)}
                        <span class="muted">
                          {pin.expired ? 'expired' : 'until'}
                          {new Date(pin.expires_at).toISOString().slice(0, 10)}
                        </span>
                        {#if pin.reason}<span class="muted">— {pin.reason}</span>{/if}
                      </div>
                      {#if pin.expiring_soon}
                        <!-- Warn BEFORE the data goes. A restore that simply
                             vanishes is the same silent disappearance the pin
                             exists to prevent, just deferred to the expiry. -->
                        <p class="pin-warn" role="alert">
                          Expires in {Math.max(0, Math.round(pin.expires_in_hours / 24))} day(s).
                          The restored rows will be deleted from Postgres; the Parquet
                          copy is not touched.
                        </p>
                      {/if}
                      <div class="pin-actions">
                        <Button
                          variant="secondary"
                          disabled={pinBusy === pin.id}
                          onclick={() => doExtendPin(pin.id)}
                        >
                          Extend 30 days
                        </Button>
                        <Button
                          variant="secondary"
                          disabled={pinBusy === pin.id}
                          onclick={() => doReleasePin(pin.id)}
                        >
                          {pinBusy === pin.id ? 'Working…' : 'Release now'}
                        </Button>
                      </div>
                    </li>
                  {/each}
                </ul>
              </div>
            {/if}
          {:else}
            <div class="center"><Spinner size={20} /></div>
          {/if}
        </Card>
      </div>

      {#if policy}
        <div class="section">
          <Card title="Restore from cold">
            <p class="muted policy-lede">
              Copies a range back out of Parquet into Postgres so the rest of the
              dashboard can query it again. The Parquet copy is never removed, so a
              restore adds storage rather than moving it — which is why every restore
              expires.
            </p>

            <div class="restore-form">
              <label class="policy-field">
                <span class="policy-label">Table</span>
                <select class="policy-input" bind:value={restoreTable} disabled={restoreBusy}>
                  {#each RESTORABLE_TABLES as t (t)}
                    <option value={t}>{t}</option>
                  {/each}
                </select>
              </label>
              <label class="policy-field">
                <span class="policy-label">From (UTC)</span>
                <input class="policy-input" type="date" bind:value={restoreFrom} disabled={restoreBusy} />
              </label>
              <label class="policy-field">
                <span class="policy-label">To (inclusive)</span>
                <input class="policy-input" type="date" bind:value={restoreTo} disabled={restoreBusy} />
              </label>
              <label class="policy-field">
                <span class="policy-label">Keep for ({RESTORE_MIN_DAYS}–{RESTORE_MAX_DAYS} days)</span>
                <!-- Text for the same reason as the rotation age above. -->
                <input class="policy-input" type="text" inputmode="numeric" bind:value={restoreDays} disabled={restoreBusy} />
              </label>
              <Button disabled={!restoreValid} onclick={submitRestore}>
                {restoreBusy ? 'Queueing…' : 'Restore'}
              </Button>
            </div>

            {#if restoreError}
              <p class="policy-warn" role="alert">{restoreError}</p>
            {/if}
            {#if restoreFrom && restoreTo && restoreRange === null}
              <p class="policy-warn" role="alert">
                The end date must be on or after the start date.
              </p>
            {/if}

            {#if jobs.length > 0}
              <ul class="job-list">
                {#each jobs as job (job.id)}
                  <li>
                    <div class="job-head">
                      <code>{job.table_name}</code>
                      {new Date(job.range_start).toISOString().slice(0, 10)} →
                      {new Date(job.range_end).toISOString().slice(0, 10)}
                      <span class="job-status job-{job.status}">{job.status}</span>
                    </div>
                    {#if job.status === 'running' || job.status === 'queued'}
                      <div class="job-bar"><div class="job-fill" style="width:{jobPercent(job)}%"></div></div>
                      <p class="muted">
                        {job.rows_restored.toLocaleString()} of
                        {job.rows_estimated.toLocaleString()} rows
                      </p>
                    {:else if job.status === 'succeeded'}
                      <p class="muted">
                        Restored {job.rows_restored.toLocaleString()} rows. Held until
                        {new Date(job.pin_expires_at).toISOString().slice(0, 10)}.
                      </p>
                    {:else if job.error}
                      <p class="policy-warn" role="alert">{job.error}</p>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
          </Card>
        </div>
      {/if}

      <div class="section">
        <Card title="Database tables" padding="none">
          {#if sortedTables.length === 0}
            <EmptyState title="No tables" description="No tables were reported." icon="server" />
          {:else}
            <DataTable>
              {#snippet head()}
                <tr>
                  <SortableTh key="table" columnDefault="asc" sort={tableSort} onsort={onTableSort}>
                    Table
                  </SortableTh>
                  <SortableTh key="size" class="num" sort={tableSort} onsort={onTableSort}>
                    Size
                  </SortableTh>
                  <SortableTh key="hot_rows" class="num" sort={tableSort} onsort={onTableSort}>
                    Hot rows
                  </SortableTh>
                </tr>
              {/snippet}
              {#snippet children()}
                {#each sortedTables as t (t.name)}
                  <tr>
                    <td><span class="cell-mono">{t.name}</span></td>
                    <td class="num">{fmtBytes(t.total_bytes)}</td>
                    <td class="num">{t.hot_rows.toLocaleString()}</td>
                  </tr>
                {/each}
              {/snippet}
            </DataTable>
          {/if}
        </Card>
      </div>

      <div class="section">
        <Card title="Storage by app" padding="none">
          {#if sortedApps.length === 0}
            <EmptyState title="No apps" description="No apps have been created yet." icon="package" />
          {:else}
            <DataTable>
              {#snippet head()}
                <tr>
                  <SortableTh key="org" columnDefault="asc" sort={appSort} onsort={onAppSort}>
                    Org
                  </SortableTh>
                  <SortableTh key="project" columnDefault="asc" sort={appSort} onsort={onAppSort}>
                    Project
                  </SortableTh>
                  <SortableTh key="app" columnDefault="asc" sort={appSort} onsort={onAppSort}>
                    App
                  </SortableTh>
                  <SortableTh key="hot_rows" class="num" sort={appSort} onsort={onAppSort}>
                    Hot rows
                  </SortableTh>
                  <SortableTh key="cold_rows" class="num" sort={appSort} onsort={onAppSort}>
                    Cold rows
                  </SortableTh>
                  <SortableTh key="cold_bytes" class="num" sort={appSort} onsort={onAppSort}>
                    Cold bytes
                  </SortableTh>
                  <SortableTh key="hot_bytes" class="num" sort={appSort} onsort={onAppSort}>
                    Hot bytes
                  </SortableTh>
                </tr>
              {/snippet}
              {#snippet children()}
                {#each sortedApps as a (a.app_id)}
                  <tr class="clickable" onclick={() => toggleApp(a.app_id)}>
                    <td>
                      <!-- The disclosure chevron leads the row, so it stays in
                           the first cell even though what expands below is the
                           app's breakdown. -->
                      <div class="name-cell">
                        <span class="chevron" class:open={openApp[a.app_id]}>
                          <Icon name="chevron-right" size={14} />
                        </span>
                        <span class="cell-muted">{a.org_name}</span>
                      </div>
                    </td>
                    <!-- Empty only for a report cached by a build that predates
                         project_name; the next refresh fills it in. -->
                    <td><span class="cell-muted">{a.project_name || '—'}</span></td>
                    <td><span class="name">{a.app_name}</span></td>
                    <td class="num">{a.hot_rows_total.toLocaleString()}</td>
                    <td class="num">{a.cold_rows_total.toLocaleString()}</td>
                    <td class="num">{fmtBytes(a.cold_bytes_total)}</td>
                    <td class="num">{fmtBytes(a.estimated_hot_bytes_total)}</td>
                  </tr>
                  {#if openApp[a.app_id]}
                    <tr class="expand-row">
                      <td colspan="7" style="background: var(--surface-2); white-space: normal; cursor: default;">
                        <div class="expand-body">
                          <h4 class="expand-title">Per-table breakdown</h4>
                          <!--
                            A CSS grid, not a nested <table> — a raw <table> here would sit
                            inside DataTable's own <tbody>/<td> and pick up its scoped-but-
                            :global() `tbody td` / `td.num` rules (padding, white-space,
                            alignment) by DOM descendance, regardless of component
                            boundaries. See the `uptimeColor` inline-style note in
                            Monitors.svelte for the same trap on a different property.
                          -->
                          <div class="mini-grid" role="table" aria-label="Per-table breakdown">
                            <div class="mini-row mini-head" role="row">
                              <span role="columnheader">Table</span>
                              <span class="num" role="columnheader">Hot rows</span>
                              <span class="num" role="columnheader">Cold rows</span>
                              <span class="num" role="columnheader">Cold bytes</span>
                              <span class="num" role="columnheader">Hot bytes</span>
                            </div>
                            {#each a.tables as t (t.name)}
                              <div class="mini-row" role="row">
                                <span class="cell-mono" role="cell">{t.name}</span>
                                <span class="num" role="cell">{t.hot_rows.toLocaleString()}</span>
                                <span class="num" role="cell">{t.cold_rows.toLocaleString()}</span>
                                <span class="num" role="cell">{fmtBytes(t.cold_bytes)}</span>
                                <span class="num" role="cell">{fmtBytes(t.estimated_hot_bytes)}</span>
                              </div>
                            {/each}
                          </div>

                          <!-- Show the true total, not the page size: the API
                               truncates the list, so `cold_files.length` caps
                               out and silently reads as "that's all of them". -->
                          <h4 class="expand-title">Cold Parquet files ({a.cold_files_total})</h4>
                          {#if a.cold_files_total === 0}
                            <p class="faint">No cold files for this app.</p>
                          {:else}
                            <ul class="file-list">
                              {#each a.cold_files as f (f.path)}
                                <li>
                                  <span class="cell-mono file-path" title={f.path}>{f.path}</span>
                                  <span class="cell-muted file-size">{fmtBytes(f.bytes)}</span>
                                </li>
                              {/each}
                            </ul>
                            {#if a.cold_files_total > a.cold_files.length}
                              <p class="faint">
                                Showing the first {a.cold_files.length} of {a.cold_files_total} files.
                              </p>
                            {/if}
                          {/if}
                        </div>
                      </td>
                    </tr>
                  {/if}
                {/each}
              {/snippet}
            </DataTable>
          {/if}
        </Card>
      </div>
    {/if}
  </div>
</AdminShell>

<ConfirmDialog
  bind:open={confirmRevert}
  title="Lower the rotation age?"
  message={policy && revertWouldLower(policy) ? describeRevert(policy) : ''}
  confirmLabel="Revert"
  danger
  loading={policyBusy}
  onconfirm={revertPolicy}
  oncancel={() => (confirmRevert = false)}
/>

<style>
  .policy-lede { margin: 0 0 14px; }
  .policy-row { display: flex; align-items: flex-end; gap: 10px; flex-wrap: wrap; }
  .policy-field { display: flex; flex-direction: column; gap: 5px; }
  .policy-label { font-size: 12px; color: var(--text-muted); }
  .policy-input {
    width: 120px;
    padding: 7px 9px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text);
  }
  .policy-warn {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    margin: 12px 0 0;
    padding: 9px 11px;
    border: 1px solid var(--warning, var(--border-strong));
    border-radius: var(--radius);
    color: var(--text);
    font-size: 13px;
    line-height: 1.5;
  }
  /* Same shape as .policy-warn, error tone: an action that failed is a harder
     signal than a caution about one that would succeed. */
  .policy-err {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    margin: 12px 0 0;
    padding: 9px 11px;
    border: 1px solid var(--error);
    border-radius: var(--radius);
    color: var(--error);
    font-size: 13px;
    line-height: 1.5;
  }
  .policy-ok { margin: 10px 0 0; font-size: 13px; color: var(--success, var(--text)); }
  .policy-denied { margin: 0; }
  .policy-facts {
    display: flex;
    gap: 26px;
    margin: 16px 0 0;
    flex-wrap: wrap;
  }
  .policy-facts dt { font-size: 11px; text-transform: uppercase; letter-spacing: 0.04em; color: var(--text-muted); }
  .policy-facts dd { margin: 3px 0 0; font-size: 14px; }
  .policy-detail { margin-top: 14px; font-size: 13px; }
  .policy-detail summary { cursor: pointer; color: var(--text-muted); }
  .policy-detail ul { margin: 6px 0 0 18px; }
  .policy-pins { margin-top: 18px; }
  .pins-title { margin: 0 0 4px; font-size: 14px; }
  .pin-list { margin: 8px 0 0; padding-left: 18px; font-size: 13px; }
  .pin-list li { margin-bottom: 10px; }
  .pin-list li.expired { opacity: 0.55; }
  /* Warning state is a border, not just colour — the row also has to read as
     "about to change" for anyone who cannot distinguish the hue. */
  .pin-list li.soon {
    border-left: 3px solid var(--warning, #b45309);
    padding-left: 8px;
    margin-left: -11px;
  }
  .pin-main { display: flex; flex-wrap: wrap; gap: 6px; align-items: baseline; }
  .pin-warn { margin: 4px 0; font-size: 12px; color: var(--warning, #b45309); }
  .pin-actions { display: flex; gap: 8px; margin-top: 6px; }

  .restore-form {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    align-items: flex-end;
    margin: 12px 0;
  }
  .job-list { list-style: none; margin: 12px 0 0; padding: 0; font-size: 13px; }
  .job-list li {
    padding: 8px 0;
    border-top: 1px solid var(--border, #2a2a2a);
  }
  .job-head { display: flex; flex-wrap: wrap; gap: 8px; align-items: baseline; }
  .job-status { font-size: 12px; text-transform: uppercase; letter-spacing: 0.04em; }
  .job-succeeded { color: var(--success, #15803d); }
  .job-failed, .job-cancelled { color: var(--danger, #b91c1c); }
  .job-queued, .job-running { color: var(--muted-fg, #888); }
  .job-bar {
    height: 4px;
    background: var(--border, #2a2a2a);
    border-radius: 2px;
    overflow: hidden;
    margin: 6px 0 4px;
  }
  .job-fill { height: 100%; background: var(--accent, #2563eb); transition: width 0.3s; }

  .storage {
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  /* --- header --------------------------------------------------------------- */
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
  }

  /* --- error banner --------------------------------------------------------- */
  .err-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    font-size: 13px;
    color: var(--error);
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 38%, transparent);
    border-radius: var(--radius);
  }

  .center {
    display: grid;
    place-items: center;
    min-height: 180px;
  }

  .section {
    display: flex;
    flex-direction: column;
  }

  /* --- app row / expander ---------------------------------------------------- */
  .name-cell {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .name {
    font-weight: 560;
  }
  .chevron {
    display: inline-flex;
    color: var(--text-faint);
    transition: transform 0.14s ease;
  }
  .chevron.open {
    transform: rotate(90deg);
  }

  /* The expand-row <td>'s background/white-space/cursor are set inline (see
     markup) rather than here — DataTable's own scoped-but-:global() `tbody td`
     rule would otherwise win the specificity fight. */
  .expand-body {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 6px 4px 10px;
  }
  .expand-title {
    font-size: 11px;
    font-weight: 620;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-faint);
    margin-top: 6px;
  }
  .expand-title:first-child {
    margin-top: 0;
  }

  .mini-grid {
    display: flex;
    flex-direction: column;
    font-size: 12.5px;
  }
  .mini-row {
    display: grid;
    grid-template-columns: 1.6fr repeat(4, 1fr);
    gap: 8px;
    padding: 5px 8px;
    border-bottom: 1px solid var(--border);
  }
  .mini-row:last-child {
    border-bottom: none;
  }
  .mini-head {
    font-weight: 600;
    color: var(--text-faint);
  }
  .mini-row .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  .file-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 240px;
    overflow-y: auto;
  }
  .file-list li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 4px 8px;
    border-radius: var(--radius-sm);
  }
  .file-list li:hover {
    background: var(--surface-3);
  }
  .file-path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .file-size {
    flex-shrink: 0;
    font-size: 12px;
  }
</style>
