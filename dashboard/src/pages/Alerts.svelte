<script lang="ts">
  import { t } from '../lib/i18n';
  import { formatDateTime } from '../lib/utils/format';
  import AdminShell from '../lib/components/layout/AdminShell.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { lockedBy } from '../lib/models/page-access';
  import {
    listChannels,
    createChannel,
    updateChannel,
    deleteChannel,
    testChannel,
    listRules,
    createRule,
    updateRule,
    deleteRule,
    listAlertEvents,
    getAlertMeta,
  } from '../lib/api/alerts';
  import type {
    AlertEvent,
    AlertMeta,
    AlertRule,
    AlertSeverity,
    ChannelKind,
    MonitorListItem,
    NotificationChannel,
    TriggerType,
  } from '../lib/models';
  import { errorMessage } from '../lib/api/client';
  import { listMonitors } from '../lib/api/monitors';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SortableTh from '../lib/components/SortableTh.svelte';
  import ClientPager from '../lib/components/ClientPager.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import { CachedView } from '../lib/stores/cached-view.svelte';
  import { viewCache, viewKey } from '../lib/stores/view-cache';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';
  import { setOffsetPage, setOffsetSort, type OffsetListState } from '../lib/models/list-state';
  import {
    alertEventAccessor,
    channelAccessor,
    ruleAccessor,
    CHANNEL_DEFAULT_SORT,
    EVENT_DEFAULT_SORT,
    RULE_DEFAULT_SORT,
  } from '../lib/models/alert-sort';
  import { pageSlice } from '../lib/models/paginate';
  import { sortRows } from '../lib/models/sort-rows';
  import type { SortDir } from '../lib/models/sort';

  /** Rows per page, for all three tables. Each list arrives whole, so this is a
      rendering budget only — no request is issued by a sort or a page click. */
  const PAGE = 25;

  type Tab = 'channels' | 'rules' | 'history';

  let tab = $state<Tab>('channels');
  // Cached view (lib/stores/cached-view.svelte.ts): the three lists paint
  // instantly when you come back from a rule editor, then refresh behind them.
  //
  // ONE view for all three because the tabs share a single load and are only
  // meaningful together — a rule row names the channel it delivers to, so
  // fresh rules beside stale channels would render a rule pointing at a
  // channel that no longer exists.
  const view = new CachedView<{
    channels: NotificationChannel[];
    rules: AlertRule[];
    history: AlertEvent[];
    meta: AlertMeta;
  }>();
  const channels = $derived(view.data?.channels ?? []);
  const rules = $derived(view.data?.rules ?? []);
  const history = $derived(view.data?.history ?? []);
  const meta = $derived(view.data?.meta ?? null);
  const loading = $derived(view.loading);
  let refreshing = $state(false);
  // Mutations report through the same banner as the load, and a `$derived`
  // cannot be assigned to — so they keep their own state and this is the fold.
  let actionError = $state<string | null>(null);
  const error = $derived(actionError ?? view.error);
  let notice = $state<string | null>(null);

  const orgId = $derived(sessionStore.currentOrgId);
  // notifications.rs:113,187,260,272,443,522,580 all use `authorize_org`, so a
  // project- or app-scoped `alert:write` grant cannot satisfy any of them.
  const writeLock = $derived(lockedBy('alert:write', { level: 'org' }));
  // The Alerts page has no project selector of its own — it reuses the
  // session's, same as Monitors.svelte. Monitor pinning is unavailable
  // (field disabled) until one is selected.
  const projectId = $derived(sessionStore.currentProjectId);

  // --- channel form ---------------------------------------------------------
  let showChannelForm = $state(false);
  let chName = $state('');
  let chKind = $state<ChannelKind>('slack');
  // One flat bag of every field any kind might need; only the relevant subset is
  // read when building the request, so switching kind never loses typed input.
  let chFields = $state<Record<string, string>>({
    host: '',
    port: '587',
    from: '',
    to: '',
    username: '',
    password: '',
    webhook_url: '',
    homeserver: '',
    room_id: '',
    access_token: '',
    chat_id: '',
    bot_token: '',
    url: '',
    signing_secret: '',
  });

  function resetChannelFields() {
    for (const k of Object.keys(chFields)) chFields[k] = k === 'port' ? '587' : '';
  }
  let savingChannel = $state(false);
  let testingId = $state<string | null>(null);

  // --- rule form ------------------------------------------------------------
  let showRuleForm = $state(false);
  let rName = $state('');
  let rTrigger = $state<TriggerType>('monitor_down');
  // Plain id string, not the monitor object — `$state` deep-proxies stored
  // objects, so an identity comparison against `monitorOptions` entries would
  // never match. See buildConditions/needs.monitor for how this is gated.
  let rMonitor = $state<string>('');
  let monitorOptions = $state<MonitorListItem[]>([]);
  let rSeverity = $state<AlertSeverity>('warning');
  // Numeric fields are held as strings because `Input.value` is a string; they
  // are parsed once, at submit.
  let rThrottle = $state('300');
  let rThreshold = $state('10');
  let rComparator = $state('gte');
  let rWindowMinutes = $state('5');
  let rSpikeFactor = $state('3');
  let rMetric = $state('p95');
  let rLevel = $state('');
  let rEnvironment = $state('');
  let rEventName = $state('');
  let rTemplate = $state('');
  let rChannels = $state<string[]>([]);
  let savingRule = $state(false);

  // --- edit mode ------------------------------------------------------------
  // The create forms double as edit forms rather than being duplicated. Both
  // PATCH endpoints are narrower than their POST counterparts: a channel's
  // `kind` and a rule's `trigger_type`/scope cannot change after creation, so
  // those controls are locked while editing rather than offered and ignored.
  let editingChannelId = $state<string | null>(null);
  let editingRuleId = $state<string | null>(null);

  /**
   * The channel config fields as they were when the form opened.
   *
   * `config` is REPLACED wholesale by the API, and what it hands back on read is
   * a redacted projection — a webhook's `url` and a Slack/Discord `webhook_url`
   * come back as presence flags, never values. So a form that always sent its
   * config would erase the destination of any channel whose destination it was
   * never allowed to see. Comparing against this snapshot means config and
   * secret are sent only when someone actually typed in them.
   *
   * It also keeps a rename cheap: `update_channel` runs an extra retarget
   * authorization whenever config or secret is present, which a rename neither
   * needs nor should be refused for.
   */
  let chSnapshot = $state('');
  const chFieldsTouched = $derived(JSON.stringify(chFields) !== chSnapshot);

  function openNewChannel() {
    editingChannelId = null;
    chName = '';
    chKind = 'slack';
    resetChannelFields();
    chSnapshot = JSON.stringify(chFields);
    showChannelForm = true;
  }

  function openEditChannel(c: NotificationChannel) {
    editingChannelId = c.id;
    chName = c.name;
    chKind = c.kind;
    resetChannelFields();
    // Prefill only the genuinely non-secret fields the API returns. Anything
    // redacted stays blank and is re-typed to change it.
    const cfg = (c.config ?? {}) as Record<string, unknown>;
    const put = (k: string, v: unknown) => {
      if (v === undefined || v === null) return;
      chFields[k] = Array.isArray(v) ? v.join(', ') : String(v);
    };
    put('host', cfg.host);
    put('port', cfg.port);
    put('from', cfg.from);
    put('to', cfg.to);
    put('username', cfg.username);
    put('homeserver', cfg.homeserver);
    put('room_id', cfg.room_id);
    put('chat_id', cfg.chat_id);
    chSnapshot = JSON.stringify(chFields);
    showChannelForm = true;
  }

  function closeChannelForm() {
    showChannelForm = false;
    editingChannelId = null;
    chName = '';
    resetChannelFields();
  }

  /**
   * `/v1/projects/{id}/monitors` is project-scoped, and the Alerts page has no
   * project selector of its own — it uses the session's. With no project
   * selected the field is disabled rather than empty-and-clickable, and the
   * rule can still be created un-narrowed.
   */
  async function loadMonitorOptions() {
    const pid = projectId;
    if (!pid) { monitorOptions = []; return; }
    try {
      monitorOptions = await listMonitors(pid);
    } catch {
      monitorOptions = [];
    }
  }

  function openNewRule() {
    editingRuleId = null;
    rName = '';
    rTrigger = 'monitor_down';
    rSeverity = 'warning';
    rThrottle = '300';
    rThreshold = '10';
    rComparator = 'gte';
    rWindowMinutes = '5';
    rSpikeFactor = '3';
    rMetric = 'p95';
    rLevel = '';
    rEnvironment = '';
    rEventName = '';
    rTemplate = '';
    rChannels = [];
    rMonitor = '';
    void loadMonitorOptions();
    showRuleForm = true;
  }

  async function openEditRule(r: AlertRule) {
    editingRuleId = r.id;
    rName = r.name;
    rTrigger = r.trigger_type;
    rMonitor = r.monitor_id ?? '';
    // The rule's own project is derived from its monitor at creation and may
    // not be the session's currently-selected project, so the fetched list
    // can legitimately not contain a pinned rule's monitor. A native <select>
    // silently falls back to its first option ("All monitors...") when its
    // bound value matches no <option>, which would tell the operator a
    // pinned rule is un-pinned. Awaiting the load (rather than firing it and
    // moving on, as `openNewRule` does) lets us detect that and splice in a
    // synthetic option carrying the real id — never inventing a name we
    // don't have.
    await loadMonitorOptions();
    if (r.monitor_id && !monitorOptions.some((m) => m.id === r.monitor_id)) {
      monitorOptions = [
        {
          id: r.monitor_id,
          name: 'Pinned monitor (outside the selected project)',
          kind: 'http',
          target: '',
          status: 'unknown',
          enabled: true,
          last_response_time_ms: null,
          last_checked_at: null,
          uptime_24h: null,
        },
        ...monitorOptions,
      ];
    }
    rSeverity = r.severity;
    rThrottle = String(r.throttle_seconds);
    rTemplate = r.message_template ?? '';
    rChannels = [...r.channel_ids];
    // Unlike a channel's config, conditions come back in full, so the form can
    // round-trip them. Anything the trigger does not use keeps its default —
    // `buildConditions` only reads the subset `triggerNeeds` selects.
    const c = r.conditions ?? {};
    rThreshold = c.threshold != null ? String(c.threshold) : '10';
    rComparator = c.comparator ?? 'gte';
    rWindowMinutes = c.window_seconds != null ? String(Math.round(c.window_seconds / 60)) : '5';
    rSpikeFactor = c.spike_factor != null ? String(c.spike_factor) : '3';
    rMetric = c.metric ?? 'p95';
    rLevel = c.filters?.level ?? '';
    rEnvironment = c.filters?.environment ?? '';
    rEventName = c.filters?.event_name ?? '';
    showRuleForm = true;
  }

  function closeRuleForm() {
    showRuleForm = false;
    editingRuleId = null;
  }

  let confirmDelete = $state<{ kind: 'channel' | 'rule'; id: string; name: string } | null>(null);

  /** Which extra condition inputs a trigger actually uses. */
  const triggerNeeds = (t: TriggerType) => ({
    threshold: t === 'error_threshold' || t === 'event_threshold' || t === 'perf_degradation',
    window: t !== 'monitor_down' && t !== 'monitor_up',
    monitor: t === 'monitor_down' || t === 'monitor_up',
    spike: t === 'error_spike',
    metric: t === 'perf_degradation',
    level: t === 'issue_new' || t === 'issue_regression' || t === 'error_threshold' || t === 'error_spike',
    eventName: t === 'event_threshold',
  });

  const needs = $derived(triggerNeeds(rTrigger));

  const TRIGGER_LABELS: Record<TriggerType, string> = {
    monitor_down: 'Monitor goes down',
    monitor_up: 'Monitor recovers',
    issue_new: 'New issue appears',
    issue_regression: 'Resolved issue regresses',
    error_threshold: 'Error count crosses threshold',
    error_spike: 'Error rate spikes',
    event_threshold: 'Event count crosses threshold',
    perf_degradation: 'Latency degrades',
  };

  const KIND_LABELS: Record<ChannelKind, string> = {
    email: 'Email (SMTP)',
    slack: 'Slack',
    discord: 'Discord',
    matrix: 'Element / Matrix',
    telegram: 'Telegram',
    webhook: 'Webhook',
  };

  // The two label lookups the cells render, named once so the Type and Trigger
  // columns can be SORTED by exactly the text they show. Both maps are keyed by
  // an enum the API could extend, so the `?? key` arm is a real fallback rather
  // than defensive noise — and passing the accessor a lookup that disagreed
  // with the cell is precisely the "orders by one value, displays another" bug.
  const kindLabel = (k: ChannelKind): string => KIND_LABELS[k] ?? k;
  const triggerLabel = (t: TriggerType): string => TRIGGER_LABELS[t] ?? t;

  /**
   * Channel/rule TYPE metadata — the static catalogue of what fields each
   * channel kind takes. Cached under its own key and never revalidated with the
   * lists: it is deployment-wide configuration, identical for every org, and
   * the old code already fetched it once and reused it for the tab's lifetime.
   * Keying it separately keeps that property while extending it across
   * navigations instead of losing it on every unmount.
   */
  async function loadMeta(): Promise<AlertMeta> {
    const key = viewKey('alerts.meta');
    const cached = viewCache.get<AlertMeta>(key);
    if (cached !== undefined) return cached;
    return viewCache.set(key, await viewCache.dedupe(key, () => getAlertMeta()));
  }

  /**
   * `force` bypasses the fresh-window short-circuit. Every mutation re-lists
   * through it: a re-list that joined a flight issued before the write returns
   * the pre-write lists, and `set` would then cache it — so the channel just
   * deleted reappears and stays for the whole fresh window.
   */
  async function load(force = false) {
    const org = orgId;
    if (!org) {
      // No org means no request is issued, and `loading` starts true — without
      // this the page spins forever on a fetch that never happened.
      view.idle();
      return;
    }
    actionError = null;
    await view.load(
      viewKey('alerts.page', org),
      async () => {
        const [channels, rules, history, meta] = await Promise.all([
          listChannels(org),
          listRules(org),
          listAlertEvents(org, 50),
          loadMeta(),
        ]);
        return { channels, rules, history, meta };
      },
      force,
    );
  }

  async function refresh() {
    refreshing = true;
    try {
      viewCache.invalidate('alerts.page');
      await load(true);
    } finally {
      refreshing = false;
    }
  }

  function f(key: string): string {
    return chFields[key] ?? '';
  }

  /** Split the flat form bag into the channel's non-secret config and secret. */
  function buildChannelPayload(): { config: Record<string, unknown>; secret: Record<string, string> } {
    const config: Record<string, unknown> = {};
    const secret: Record<string, string> = {};
    switch (chKind) {
      case 'email': {
        config.host = f('host');
        config.port = Number(f('port') || 587);
        config.from = f('from');
        config.to = f('to')
          .split(',')
          .map((s) => s.trim())
          .filter(Boolean);
        if (f('username')) config.username = f('username');
        if (f('password')) secret.password = f('password');
        break;
      }
      case 'slack':
      case 'discord':
        if (f('webhook_url')) secret.webhook_url = f('webhook_url');
        break;
      case 'matrix':
        config.homeserver = f('homeserver');
        config.room_id = f('room_id');
        if (f('access_token')) secret.access_token = f('access_token');
        break;
      case 'telegram':
        config.chat_id = f('chat_id');
        if (f('bot_token')) secret.bot_token = f('bot_token');
        break;
      case 'webhook':
        config.url = f('url');
        if (f('signing_secret')) secret.signing_secret = f('signing_secret');
        break;
    }
    return { config, secret };
  }

  async function submitChannel() {
    if (!orgId || !chName) return;
    savingChannel = true;
    actionError = null;
    try {
      const { config, secret } = buildChannelPayload();
      if (editingChannelId) {
        // `kind` is absent on purpose — the API cannot change it. config and
        // secret go only when something was typed into them; see `chSnapshot`.
        await updateChannel(editingChannelId, {
          name: chName,
          ...(chFieldsTouched ? { config } : {}),
          ...(chFieldsTouched && Object.keys(secret).length ? { secret } : {}),
        });
      } else {
        await createChannel(orgId, {
          name: chName,
          kind: chKind,
          config,
          secret: Object.keys(secret).length ? secret : undefined,
        });
      }
      closeChannelForm();
      await load(true);
    } catch (e) {
      actionError = errorMessage(e);
    } finally {
      savingChannel = false;
    }
  }

  async function toggleChannel(c: NotificationChannel) {
    try {
      await updateChannel(c.id, { enabled: !c.enabled });
      await load(true);
    } catch (e) {
      actionError = errorMessage(e);
    }
  }

  async function runTest(c: NotificationChannel) {
    testingId = c.id;
    actionError = null;
    notice = null;
    try {
      const res = await testChannel(c.id);
      if (res.ok) notice = `Test notification delivered to “${c.name}”.`;
      else actionError = `Test to “${c.name}” failed: ${res.error ?? 'unknown error'}`;
    } catch (e) {
      actionError = errorMessage(e);
    } finally {
      testingId = null;
    }
  }

  /** Parse a numeric form field, falling back when the text is not a number. */
  function num(text: string, fallback: number): number {
    const n = Number(text);
    return Number.isFinite(n) ? n : fallback;
  }

  function buildConditions(): Record<string, unknown> {
    const conditions: Record<string, unknown> = {};
    if (needs.threshold) {
      conditions.threshold = num(rThreshold, 0);
      conditions.comparator = rComparator;
    }
    if (needs.window) conditions.window_seconds = num(rWindowMinutes, 5) * 60;
    if (needs.spike) conditions.spike_factor = num(rSpikeFactor, 3);
    if (needs.metric) conditions.metric = rMetric;
    const filters: Record<string, string> = {};
    if (needs.level && rLevel) filters.level = rLevel;
    if (rEnvironment) filters.environment = rEnvironment;
    if (needs.eventName && rEventName) filters.event_name = rEventName;
    if (Object.keys(filters).length) conditions.filters = filters;
    return conditions;
  }

  async function submitRule() {
    if (!orgId || !rName) return;
    savingRule = true;
    actionError = null;
    try {
      if (editingRuleId) {
        // No `trigger_type`: it is fixed at creation, along with the
        // project/app scope, so the form locks the selector while editing.
        await updateRule(editingRuleId, {
          name: rName,
          conditions: buildConditions(),
          severity: rSeverity,
          throttle_seconds: num(rThrottle, 300),
          message_template: rTemplate || null,
          channel_ids: rChannels,
        });
      } else {
        await createRule(orgId, {
          name: rName,
          trigger_type: rTrigger,
          // Gated on `needs.monitor`, not just `rMonitor` — switching the
          // trigger away from monitor_down/monitor_up leaves `rMonitor` set
          // but hidden, and the API 400s on monitor_id with a non-monitor
          // trigger. project_id is also never sent here: the API derives it
          // from the monitor, and a disagreeing value is a 400.
          monitor_id: needs.monitor ? rMonitor || undefined : undefined,
          conditions: buildConditions(),
          severity: rSeverity,
          throttle_seconds: num(rThrottle, 300),
          message_template: rTemplate || null,
          channel_ids: rChannels,
        });
      }
      closeRuleForm();
      await load(true);
    } catch (e) {
      actionError = errorMessage(e);
    } finally {
      savingRule = false;
    }
  }

  async function toggleRule(r: AlertRule) {
    try {
      await updateRule(r.id, { enabled: !r.enabled });
      await load(true);
    } catch (e) {
      actionError = errorMessage(e);
    }
  }

  async function doDelete() {
    const target = confirmDelete;
    if (!target) return;
    confirmDelete = null;
    try {
      if (target.kind === 'channel') await deleteChannel(target.id);
      else await deleteRule(target.id);
      await load(true);
    } catch (e) {
      actionError = errorMessage(e);
    }
  }

  function toggleRuleChannel(id: string) {
    rChannels = rChannels.includes(id)
      ? rChannels.filter((c) => c !== id)
      : [...rChannels, id];
  }

  const severityTone = (s: string) =>
    s === 'critical' ? 'error' : s === 'warning' ? 'warning' : 'info';
  const statusTone = (s: string) =>
    s === 'sent' ? 'success' : s === 'failed' ? 'error' : 'neutral';

  const fmtTime = (iso: string) => formatDateTime(iso);

  /**
   * The channel a delivery went to, or `null` when there is no name to show.
   *
   * `null` rather than the em dash, so the History table's Channel column can
   * sort by it: `sortRows` keeps a null last in both directions, whereas the
   * literal `'—'` collates before every real name and would lead one direction
   * while trailing the other. The dash is applied at the cell instead, which is
   * where a rendering decision belongs.
   */
  const channelName = (id: string | null): string | null =>
    channels.find((c) => c.id === id)?.name ?? null;

  // --- sorting and paging ---------------------------------------------------
  // All three lists arrive whole (`listChannels` / `listRules` /
  // `listAlertEvents(orgId, 50)`), so both the sort and the pager run here,
  // over the SAME array each time: order the whole list first, then take a
  // window out of it. Sorting the window instead would reorder only what is on
  // screen while presenting itself as having ordered everything.
  //
  // THREE states, one per table, never a shared pair: sorting the channels list
  // must not send the rules list back to page 1 under a header nobody clicked.
  // Each is one `OffsetListState` rather than two variables because
  // `setOffsetSort` resets the offset as part of applying a sort — a re-ordered
  // list makes the current window meaningless, so page 1 is the only honest
  // place to land.
  let channelList = $state<OffsetListState>({ sort: CHANNEL_DEFAULT_SORT, offset: 0 });
  let ruleList = $state<OffsetListState>({ sort: RULE_DEFAULT_SORT, offset: 0 });
  let historyList = $state<OffsetListState>({ sort: EVENT_DEFAULT_SORT, offset: 0 });

  // `sortRows` copies before sorting, and that is load-bearing rather than
  // tidy: `channels` / `rules` / `history` are the arrays the page holds and
  // hands to the forms as well (the rule form's channel chips read `channels`
  // directly), so an in-place sort would reorder them underneath every other
  // reader.
  const channelsSorted = $derived(
    sortRows(channels, channelAccessor(channelList.sort.key, kindLabel), channelList.sort.dir),
  );
  const channelPage = $derived(pageSlice(channelsSorted, channelList.offset, PAGE));

  const rulesSorted = $derived(
    sortRows(rules, ruleAccessor(ruleList.sort.key, triggerLabel), ruleList.sort.dir),
  );
  const rulePage = $derived(pageSlice(rulesSorted, ruleList.offset, PAGE));

  // The Channel accessor closes over `channelName`, which reads `channels` — so
  // renaming a channel re-derives this ordering, as it must, since the column
  // is ordered by the name it renders.
  const historySorted = $derived(
    sortRows(history, alertEventAccessor(historyList.sort.key, channelName), historyList.sort.dir),
  );
  const historyPage = $derived(pageSlice(historySorted, historyList.offset, PAGE));

  function onChannelSort(key: string, columnDefault: SortDir) {
    channelList = setOffsetSort(channelList, key, columnDefault);
  }
  function onRuleSort(key: string, columnDefault: SortDir) {
    ruleList = setOffsetSort(ruleList, key, columnDefault);
  }
  function onHistorySort(key: string, columnDefault: SortDir) {
    historyList = setOffsetSort(historyList, key, columnDefault);
  }

  $effect(() => {
    // Unguarded: `load()` reads `orgId` itself (so this still re-runs on an org
    // switch) AND owns the no-org case. Guarding here instead would skip the
    // call entirely, leaving `view.loading` true from construction — the page
    // would spin forever on a request that was never issued.
    void load();
  });

  // Loaded at page-mount (and whenever the session's project changes), not
  // only when the rule form opens, so the rules table's "· <monitor name>"
  // annotation shows real names in the common case instead of the generic
  // `?? 'pinned monitor'` fallback. `loadMonitorOptions` already no-ops to
  // `[]` with no project selected and swallows fetch failures, so this can't
  // break the page.
  $effect(() => {
    if (projectId) void loadMonitorOptions();
  });
</script>

<AdminShell requireProject>
  <div class="alerts">
    <header class="head">
      <div>
        <h1 class="page-title">{t('alerts.title')}</h1>
        <p class="sub muted">
          {t('prose.alerts.subtitle')}
        </p>
      </div>
      <div class="controls">
        <RefreshButton onclick={refresh} loading={refreshing} />
      </div>
    </header>

    <nav class="tabs" aria-label={t('alerts.sections')}>
      <button class="tab" class:active={tab === 'channels'} onclick={() => (tab = 'channels')}>
        {t('alerts.tab.channels')} <span class="count">{channels.length}</span>
      </button>
      <button class="tab" class:active={tab === 'rules'} onclick={() => (tab = 'rules')}>
        {t('alerts.tab.rules')} <span class="count">{rules.length}</span>
      </button>
      <button class="tab" class:active={tab === 'history'} onclick={() => (tab = 'history')}>
        {t('alerts.tab.history')}
      </button>
    </nav>

    {#if error}
      <div class="err-banner" role="alert">
        <Icon name="triangle-alert" size={15} />
        <span>{error}</span>
      </div>
    {/if}
    {#if notice}
      <div class="ok-banner" role="status">
        <Icon name="circle-check" size={15} />
        <span>{notice}</span>
      </div>
    {/if}

    {#if loading}
      <div class="center"><Spinner size={24} /></div>
    {:else if tab === 'channels'}
      <!-- ---------------- Channels ---------------- -->
      <div class="section-head">
        <p class="muted small">
          {t('prose.alerts.channels')}
        </p>
        {#if !showChannelForm}
          <Button variant="primary" lockedReason={writeLock} onclick={openNewChannel}>
            {t('alerts.newChannel')}
          </Button>
        {/if}
      </div>

      {#if showChannelForm}
        <Card title={editingChannelId ? 'Edit channel' : 'New channel'}>
          <div class="form-grid">
            <Input label={t('common.name')} bind:value={chName} placeholder={t('prose.placeholder.opsSlack')} required />

            {#if editingChannelId}
              <!-- Says out loud what the API enforces: kind is fixed, and a
                   secret that is never returned cannot be round-tripped, so a
                   blank field has to mean "keep" rather than "clear". -->
              <p class="span-2 muted small">
                Type is fixed after creation. Secrets are never returned — leave a credential
                field blank to keep what is stored, or type a new value to replace it.
                {#if chKind === 'webhook'}
                  The destination URL is stored encrypted and is not shown; retype it to change it.
                {/if}
              </p>
            {/if}

            <div class="field">
              <label class="lbl" for="ch-kind">{t('monitors.column.type')}</label>
              <div class="control select">
                <select id="ch-kind" bind:value={chKind} disabled={editingChannelId !== null}>
                  {#each Object.entries(KIND_LABELS) as [k, label] (k)}
                    <option value={k}>{label}</option>
                  {/each}
                </select>
                <span class="affix"><Icon name="chevron-down" size={15} /></span>
              </div>
            </div>

            {#if chKind === 'slack' || chKind === 'discord'}
              <div class="span-2">
                <Input
                  label={t('alerts.field.webhookUrl')}
                  bind:value={chFields.webhook_url}
                  placeholder="https://hooks.slack.com/services/…"
                  hint={t('alerts.field.webhookHint')}
                  required
                />
              </div>
            {:else if chKind === 'email'}
              <Input
                label={t('alerts.field.smtpHost')}
                bind:value={chFields.host}
                placeholder="smtp.example.com"
                required
              />
              <Input
                label={t('alerts.field.port')}
                bind:value={chFields.port}
                placeholder="587"
                hint={t('alerts.field.portHint')}
              />
              <Input
                label={t('alerts.field.fromAddress')}
                bind:value={chFields.from}
                placeholder="sauron@example.com"
                required
              />
              <Input
                label={t('alerts.field.recipients')}
                bind:value={chFields.to}
                placeholder="oncall@example.com, sre@example.com"
                hint={t('alerts.field.recipientsHint')}
                required
              />
              <Input
                label={t('alerts.field.username')}
                bind:value={chFields.username}
                placeholder={t('prose.placeholder.optional')}
              />
              <Input
                label={t('common.password')}
                type="password"
                bind:value={chFields.password}
                placeholder={t('prose.placeholder.optional')}
                hint={t('alerts.field.encryptedHint')}
              />
            {:else if chKind === 'matrix'}
              <Input
                label={t('alerts.field.homeserver')}
                bind:value={chFields.homeserver}
                placeholder="https://matrix.org"
                required
              />
              <Input
                label={t('alerts.field.roomId')}
                bind:value={chFields.room_id}
                placeholder="!abcdef:matrix.org"
                required
              />
              <div class="span-2">
                <Input
                  label={t('alerts.field.accessToken')}
                  type="password"
                  bind:value={chFields.access_token}
                  hint={t('alerts.field.encryptedHint')}
                  required
                />
              </div>
            {:else if chKind === 'telegram'}
              <Input
                label={t('alerts.field.chatId')}
                bind:value={chFields.chat_id}
                placeholder="-1001234567890"
                required
              />
              <Input
                label={t('alerts.field.botToken')}
                type="password"
                bind:value={chFields.bot_token}
                hint={t('alerts.field.encryptedHint')}
                required
              />
            {:else}
              <div class="span-2">
                <Input
                  label="URL"
                  bind:value={chFields.url}
                  placeholder="https://example.com/hooks/sauron"
                  required
                />
              </div>
              <div class="span-2">
                <Input
                  label={t('alerts.field.signingSecret')}
                  type="password"
                  bind:value={chFields.signing_secret}
                  hint={t('alerts.field.signingHint')}
                />
              </div>
            {/if}
          </div>

          <div class="form-foot">
            <Button variant="ghost" onclick={closeChannelForm}>{t('common.cancel')}</Button>
            <Button
              variant="primary"
              loading={savingChannel}
              disabled={!chName}
              lockedReason={writeLock}
              onclick={submitChannel}
            >
              {t('alerts.createChannel')}
            </Button>
          </div>
        </Card>
      {/if}

      {#if channels.length === 0}
        <EmptyState
          title={t('alerts.empty.channels')}
          description={t('alerts.empty.channelsBody')}
          icon="bell"
        >
          {#snippet action()}
            {#if !showChannelForm}
              <Button variant="primary" lockedReason={writeLock} onclick={openNewChannel}>
                {t('alerts.newChannel')}
              </Button>
            {/if}
          {/snippet}
        </EmptyState>
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <SortableTh key="name" columnDefault="asc" sort={channelList.sort} onsort={onChannelSort}>
                {t('common.name')}
              </SortableTh>
              <SortableTh key="type" columnDefault="asc" sort={channelList.sort} onsort={onChannelSort}>
                {t('monitors.column.type')}
              </SortableTh>
              <!-- Secret is a presence flag rendered as a badge and Actions is
                   a row of buttons; neither is a value to order by, so both
                   stay plain. -->
              <th>{t('alerts.secret')}</th>
              <SortableTh key="status" columnDefault="asc" sort={channelList.sort} onsort={onChannelSort}>
                {t('common.status')}
              </SortableTh>
              <th class="num">{t('common.actions')}</th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each channelPage.rows as c (c.id)}
              <tr>
                <td>{c.name}</td>
                <td>{kindLabel(c.kind)}</td>
                <td>
                  {#if c.has_secret}
                    <Badge tone="success" size="sm">stored</Badge>
                  {:else}
                    <span class="muted">—</span>
                  {/if}
                </td>
                <td>
                  <Badge tone={c.enabled ? 'success' : 'neutral'} size="sm">
                    {c.enabled ? 'enabled' : 'disabled'}
                  </Badge>
                </td>
                <td class="num actions">
                  <Button
                    size="sm"
                    loading={testingId === c.id}
                    lockedReason={writeLock}
                    onclick={() => runTest(c)}
                    title={t('alerts.testTitle')}
                  >
                    {t('alerts.test')}
                  </Button>
                  <Button size="sm" lockedReason={writeLock} onclick={() => toggleChannel(c)}>
                    {c.enabled ? 'Disable' : 'Enable'}
                  </Button>
                  <Button
                    size="sm"
                    lockedReason={writeLock}
                    disabled={c.config_error}
                    title={c.config_error
                      ? 'This channel’s stored payload cannot be decrypted, so it cannot be edited'
                      : undefined}
                    onclick={() => openEditChannel(c)}
                  >
                    {t('common.edit')}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    lockedReason={writeLock}
                    onclick={() => (confirmDelete = { kind: 'channel', id: c.id, name: c.name })}
                  >
                    {t('common.delete')}
                  </Button>
                </td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>

        <!-- `total` is the length of the EXACT array handed to `pageSlice`
             above — `channelsSorted`, the same expression, not "all the
             channels". A pager measuring a longer list than the one being
             sliced re-creates the enabled-Next-onto-an-empty-page bug that
             `Pagination.hasNext` was made a required prop to kill; it is only
             because they agree that a final page of exactly PAGE rows
             correctly disables Next. Nothing filters this table — if anything
             ever does, the filtered array must feed BOTH `pageSlice` and this
             total, and the filter change must reset the offset with
             `setOffsetPage(list, 0)`. -->
        <ClientPager
          offset={channelList.offset}
          limit={PAGE}
          total={channelsSorted.length}
          onchange={(o) => (channelList = setOffsetPage(channelList, o))}
        />
      {/if}
    {:else if tab === 'rules'}
      <!-- ---------------- Rules ---------------- -->
      <div class="section-head">
        <p class="muted small">
          {t('prose.alerts.rules')}
        </p>
        {#if !showRuleForm}
          <Button
            variant="primary"
            lockedReason={writeLock}
            disabled={channels.length === 0}
            title={channels.length === 0 ? 'Create a channel first' : undefined}
            onclick={openNewRule}
          >
            {t('alerts.newRule')}
          </Button>
        {/if}
      </div>

      {#if showRuleForm}
        <Card title={editingRuleId ? 'Edit alert rule' : 'New alert rule'}>
          <div class="form-grid">
            <Input label={t('common.name')} bind:value={rName} placeholder={t('prose.placeholder.ruleName')} required />

            {#if editingRuleId}
              <p class="span-2 muted small">
                {t('alerts.immutableNote')}
              </p>
            {/if}

            <div class="field">
              <label class="lbl" for="r-trigger">{t('alerts.column.trigger')}</label>
              <div class="control select">
                <select id="r-trigger" bind:value={rTrigger} disabled={editingRuleId !== null}>
                  {#each Object.entries(TRIGGER_LABELS) as [k, label] (k)}
                    <option value={k}>{label}</option>
                  {/each}
                </select>
                <span class="affix"><Icon name="chevron-down" size={15} /></span>
              </div>
            </div>

            {#if needs.monitor}
              <div class="field">
                <label class="lbl" for="r-monitor">{t('alerts.column.monitor')}</label>
                <div class="control select">
                  <select
                    id="r-monitor"
                    bind:value={rMonitor}
                    disabled={editingRuleId !== null || !projectId}
                  >
                    <option value="">{t('alerts.allMonitors')}</option>
                    {#each monitorOptions as m (m.id)}
                      <option value={m.id}>{m.name}</option>
                    {/each}
                  </select>
                  <span class="affix"><Icon name="chevron-down" size={15} /></span>
                </div>
                {#if !projectId}
                  <p class="muted small">{t('alerts.pinToMonitor')}</p>
                {/if}
              </div>
            {/if}

            {#if needs.threshold}
              <div class="field">
                <label class="lbl" for="r-cmp">{t('alerts.column.condition')}</label>
                <div class="control select">
                  <select id="r-cmp" bind:value={rComparator}>
                    <option value="gte">is at least</option>
                    <option value="gt">{t('prose.alerts.isMoreThan')}</option>
                    <option value="lte">is at most</option>
                    <option value="lt">{t('prose.alerts.isLessThan')}</option>
                    <option value="eq">equals</option>
                  </select>
                  <span class="affix"><Icon name="chevron-down" size={15} /></span>
                </div>
              </div>
              <Input
                label={rTrigger === 'perf_degradation' ? 'Threshold (ms)' : 'Threshold (count)'}
                bind:value={rThreshold}
              />
            {/if}

            {#if needs.metric}
              <div class="field">
                <label class="lbl" for="r-metric">{t('alerts.column.metric')}</label>
                <div class="control select">
                  <select id="r-metric" bind:value={rMetric}>
                    {#each meta?.metrics ?? ['p95'] as m (m)}
                      <option value={m}>{m}</option>
                    {/each}
                  </select>
                  <span class="affix"><Icon name="chevron-down" size={15} /></span>
                </div>
              </div>
            {/if}

            {#if needs.spike}
              <Input
                label={t('alerts.field.spikeFactor')}
                bind:value={rSpikeFactor}
                hint={t('alerts.field.spikeHint')}
              />
            {/if}

            {#if needs.window}
              <Input
                label={t('alerts.field.window')}
                bind:value={rWindowMinutes}
                hint={t('alerts.field.max24h')}
              />
            {/if}

            {#if needs.level}
              <Input
                label={t('alerts.field.levelFilter')}
                bind:value={rLevel}
                placeholder="error"
                hint={t('alerts.field.levelHint')}
              />
            {/if}

            {#if needs.eventName}
              <Input label={t('alerts.field.eventName')} bind:value={rEventName} placeholder="checkout_completed" />
            {/if}

            <Input
              label={t('alerts.field.envFilter')}
              bind:value={rEnvironment}
              placeholder="production"
              hint={t('alerts.field.optional')}
            />

            <div class="field">
              <label class="lbl" for="r-sev">{t('alerts.column.severity')}</label>
              <div class="control select">
                <select id="r-sev" bind:value={rSeverity}>
                  <option value="info">{t('alerts.severity.info')}</option>
                  <option value="warning">{t('alerts.severity.warning')}</option>
                  <option value="critical">{t('alerts.severity.critical')}</option>
                </select>
                <span class="affix"><Icon name="chevron-down" size={15} /></span>
              </div>
            </div>

            <Input
              label={t('alerts.field.throttle')}
              bind:value={rThrottle}
              hint={t('alerts.field.throttleHint')}
            />

            <div class="span-2">
              <label class="lbl" for="r-template">{t('alerts.messageTemplate')}</label>
              <textarea
                id="r-template"
                class="textarea"
                bind:value={rTemplate}
                rows="3"
                placeholder="{'{{monitor}}'} is {'{{status}}'} — {'{{cause}}'}"
              ></textarea>
              <p class="hint">
                {t('alerts.optionalUse')} <code>{'{{variable}}'}</code> placeholders.
                {#if meta?.template_vars?.[rTrigger]}
                  Available: {meta.template_vars[rTrigger].map((v) => `{{${v}}}`).join(', ')}
                {/if}
              </p>
            </div>

            <div class="span-2">
              <span class="lbl">{t('alerts.tab.channels')}</span>
              <div class="chips">
                {#each channels as c (c.id)}
                  <button
                    type="button"
                    class="chip"
                    class:selected={rChannels.includes(c.id)}
                    onclick={() => toggleRuleChannel(c.id)}
                  >
                    {c.name}
                    <span class="chip-kind">{kindLabel(c.kind)}</span>
                  </button>
                {/each}
              </div>
            </div>
          </div>

          <div class="form-foot">
            <Button variant="ghost" onclick={closeRuleForm}>{t('common.cancel')}</Button>
            <Button
              variant="primary"
              loading={savingRule}
              disabled={!rName || rChannels.length === 0}
              lockedReason={writeLock}
              onclick={submitRule}
            >
              {t('alerts.createRule')}
            </Button>
          </div>
        </Card>
      {/if}

      {#if rules.length === 0}
        <EmptyState
          title={t('alerts.empty.rules')}
          description={t('alerts.empty.rulesBody')}
          icon="bell"
        >
          {#snippet action()}
            {#if !showRuleForm && channels.length > 0}
              <Button variant="primary" lockedReason={writeLock} onclick={openNewRule}>
                {t('alerts.newRule')}
              </Button>
            {/if}
          {/snippet}
        </EmptyState>
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <SortableTh key="name" columnDefault="asc" sort={ruleList.sort} onsort={onRuleSort}>
                {t('common.name')}
              </SortableTh>
              <SortableTh key="trigger" columnDefault="asc" sort={ruleList.sort} onsort={onRuleSort}>
                {t('alerts.column.trigger')}
              </SortableTh>
              <!-- A RANK, not the word — see `SEVERITY_ORDER` in
                   `alert-sort.ts`. `columnDefault` is left at `desc` so the
                   first click leads with `critical`, the same way the count
                   columns lead with their largest value. -->
              <SortableTh key="severity" sort={ruleList.sort} onsort={onRuleSort}>
                {t('alerts.column.severity')}
              </SortableTh>
              <!-- The brief ruled this column out as a chip list; the chips are
                   in the rule FORM, and the cell is `r.channel_ids.length` — the
                   same shape as the mask audit trail's Targets column, which
                   this slice made sortable. Actions, below, is the genuinely
                   unsortable one: a row of buttons. -->
              <SortableTh key="channels" class="num" sort={ruleList.sort} onsort={onRuleSort}>
                {t('alerts.tab.channels')}
              </SortableTh>
              <SortableTh key="throttle" class="num" sort={ruleList.sort} onsort={onRuleSort}>
                {t('alerts.column.throttle')}
              </SortableTh>
              <SortableTh key="status" columnDefault="asc" sort={ruleList.sort} onsort={onRuleSort}>
                {t('common.status')}
              </SortableTh>
              <th class="num">{t('common.actions')}</th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each rulePage.rows as r (r.id)}
              <tr>
                <td>{r.name}</td>
                <td>
                  {triggerLabel(r.trigger_type)}
                  {#if r.monitor_id}
                    <span class="muted small">
                      · {monitorOptions.find((m) => m.id === r.monitor_id)?.name ?? 'pinned monitor'}
                    </span>
                  {/if}
                </td>
                <td>
                  <Badge tone={severityTone(r.severity)} size="sm">{r.severity}</Badge>
                </td>
                <td class="num">{r.channel_ids.length}</td>
                <td class="num">{r.throttle_seconds}s</td>
                <td>
                  <Badge tone={r.enabled ? 'success' : 'neutral'} size="sm">
                    {r.enabled ? 'enabled' : 'disabled'}
                  </Badge>
                </td>
                <td class="num actions">
                  <Button size="sm" lockedReason={writeLock} onclick={() => toggleRule(r)}>
                    {r.enabled ? 'Disable' : 'Enable'}
                  </Button>
                  <Button size="sm" lockedReason={writeLock} onclick={() => openEditRule(r)}>
                    {t('common.edit')}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    lockedReason={writeLock}
                    onclick={() => (confirmDelete = { kind: 'rule', id: r.id, name: r.name })}
                  >
                    {t('common.delete')}
                  </Button>
                </td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>

        <!-- Same rule as the channels pager: `total` is the length of the very
             array `pageSlice` was given. -->
        <ClientPager
          offset={ruleList.offset}
          limit={PAGE}
          total={rulesSorted.length}
          onchange={(o) => (ruleList = setOffsetPage(ruleList, o))}
        />
      {/if}
    {:else}
      <!-- ---------------- History ---------------- -->
      {#if history.length === 0}
        <EmptyState
          title={t('alerts.empty.history')}
          description={t('alerts.empty.historyBody')}
          icon="inbox"
        />
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <SortableTh key="when" sort={historyList.sort} onsort={onHistorySort}>{t('ui.opModal.when')}</SortableTh>
              <SortableTh key="title" columnDefault="asc" sort={historyList.sort} onsort={onHistorySort}>
                {t('common.name')}
              </SortableTh>
              <SortableTh key="channel" columnDefault="asc" sort={historyList.sort} onsort={onHistorySort}>
                {t('alerts.column.channel')}
              </SortableTh>
              <!-- `desc` (the default), not `asc`: a RANK — see
                   `DELIVERY_ORDER` — so the first click leads with the
                   deliveries that failed rather than the ones that arrived. -->
              <SortableTh key="status" sort={historyList.sort} onsort={onHistorySort}>
                {t('common.status')}
              </SortableTh>
              <SortableTh key="attempts" class="num" sort={historyList.sort} onsort={onHistorySort}>
                {t('alerts.column.attempts')}
              </SortableTh>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each historyPage.rows as h (h.id)}
              <tr>
                <td class="nowrap">{fmtTime(h.created_at)}</td>
                <td>
                  {h.title}
                  {#if h.error}
                    <div class="err-detail">{h.error}</div>
                  {/if}
                </td>
                <td>{channelName(h.channel_id) ?? '—'}</td>
                <td><Badge tone={statusTone(h.status)} size="sm">{h.status}</Badge></td>
                <td class="num">{h.attempts}</td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>

        <!-- Same rule again. `listAlertEvents(orgId, 50)` caps the fetch at 50
             rows, so this pager walks what the server sent, not the whole
             history — the cap is the server's, and the pager measures the
             array it slices either way. -->
        <ClientPager
          offset={historyList.offset}
          limit={PAGE}
          total={historySorted.length}
          onchange={(o) => (historyList = setOffsetPage(historyList, o))}
        />
      {/if}
    {/if}
  </div>

  {#if confirmDelete}
    <ConfirmDialog
      open
      title={`Delete ${confirmDelete.kind}?`}
      message={`“${confirmDelete.name}” will be removed. This cannot be undone.`}
      confirmLabel={t('common.delete')}
      danger
      onconfirm={doDelete}
      oncancel={() => (confirmDelete = null)}
    />
  {/if}
</AdminShell>

<style>
  .alerts {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
  }
  .sub {
    margin-top: 4px;
    font-size: 13px;
    max-width: 62ch;
  }
  .controls {
    display: flex;
    gap: 8px;
    align-items: center;
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
    margin-inline-start: 6px;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--surface-2);
    font-size: 11px;
  }
  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
  }
  .small {
    font-size: 12.5px;
    max-width: 70ch;
  }
  .form-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 14px;
  }
  .span-2 {
    grid-column: 1 / -1;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .lbl {
    font-size: 12.5px;
    font-weight: 550;
    color: var(--text-muted);
  }
  .control.select {
    position: relative;
    display: flex;
    align-items: center;
  }
  .control.select select {
    width: 100%;
    appearance: none;
    padding: 9px 32px 9px 11px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    color: var(--text);
    font-size: 13.5px;
  }
  .affix {
    position: absolute;
    inset-inline-end: 10px;
    display: grid;
    place-items: center;
    color: var(--text-faint);
    pointer-events: none;
  }
  .textarea {
    width: 100%;
    padding: 9px 11px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    color: var(--text);
    font-size: 13.5px;
    font-family: inherit;
    resize: vertical;
  }
  .hint {
    margin-top: 5px;
    font-size: 12px;
    color: var(--text-faint);
  }
  .hint code {
    font-size: 11.5px;
  }
  .form-foot {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 6px 11px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--surface);
    color: var(--text-muted);
    font-size: 12.5px;
    cursor: pointer;
  }
  .chip:hover {
    color: var(--text);
  }
  .chip.selected {
    border-color: var(--primary);
    background: var(--primary-soft);
    color: var(--primary);
  }
  .chip-kind {
    font-size: 11px;
    color: var(--text-faint);
  }
  .chip.selected .chip-kind {
    color: inherit;
    opacity: 0.75;
  }
  .actions {
    display: flex;
    gap: 6px;
    justify-content: flex-end;
  }
  .nowrap {
    white-space: nowrap;
  }
  .err-detail {
    margin-top: 3px;
    font-size: 11.5px;
    color: var(--error);
  }
  .err-banner,
  .ok-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border-radius: var(--radius);
    font-size: 13px;
  }
  .err-banner {
    background: color-mix(in srgb, var(--error) 12%, transparent);
    color: var(--error);
  }
  .ok-banner {
    background: color-mix(in srgb, var(--success) 12%, transparent);
    color: var(--success);
  }
  .center {
    display: grid;
    place-items: center;
    padding: 48px 0;
  }

  @media (max-width: 720px) {
    .form-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
