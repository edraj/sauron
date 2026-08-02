<script lang="ts">
  import AppShell from '../lib/components/layout/AppShell.svelte';
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
    NotificationChannel,
    TriggerType,
  } from '../lib/models';
  import { errorMessage } from '../lib/api/client';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Badge from '../lib/components/ui/Badge.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import ConfirmDialog from '../lib/components/ui/ConfirmDialog.svelte';

  type Tab = 'channels' | 'rules' | 'history';

  let tab = $state<Tab>('channels');
  let channels = $state<NotificationChannel[]>([]);
  let rules = $state<AlertRule[]>([]);
  let history = $state<AlertEvent[]>([]);
  let meta = $state<AlertMeta | null>(null);
  let loading = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);

  const orgId = $derived(sessionStore.currentOrgId);
  // notifications.rs:113,187,260,272,443,522,580 all use `authorize_org`, so a
  // project- or app-scoped `alert:write` grant cannot satisfy any of them.
  const writeLock = $derived(lockedBy('alert:write', { level: 'org' }));

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

  let confirmDelete = $state<{ kind: 'channel' | 'rule'; id: string; name: string } | null>(null);

  /** Which extra condition inputs a trigger actually uses. */
  const triggerNeeds = (t: TriggerType) => ({
    threshold: t === 'error_threshold' || t === 'event_threshold' || t === 'perf_degradation',
    window: t !== 'monitor_down' && t !== 'monitor_up',
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

  async function load() {
    if (!orgId) {
      loading = false;
      return;
    }
    loading = true;
    error = null;
    try {
      const [c, r, h, m] = await Promise.all([
        listChannels(orgId),
        listRules(orgId),
        listAlertEvents(orgId, 50),
        meta ? Promise.resolve(meta) : getAlertMeta(),
      ]);
      channels = c;
      rules = r;
      history = h;
      meta = m;
    } catch (e) {
      error = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  async function refresh() {
    refreshing = true;
    try {
      await load();
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
    error = null;
    try {
      const { config, secret } = buildChannelPayload();
      await createChannel(orgId, {
        name: chName,
        kind: chKind,
        config,
        secret: Object.keys(secret).length ? secret : undefined,
      });
      showChannelForm = false;
      chName = '';
      resetChannelFields();
      await load();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      savingChannel = false;
    }
  }

  async function toggleChannel(c: NotificationChannel) {
    try {
      await updateChannel(c.id, { enabled: !c.enabled });
      await load();
    } catch (e) {
      error = errorMessage(e);
    }
  }

  async function runTest(c: NotificationChannel) {
    testingId = c.id;
    error = null;
    notice = null;
    try {
      const res = await testChannel(c.id);
      if (res.ok) notice = `Test notification delivered to “${c.name}”.`;
      else error = `Test to “${c.name}” failed: ${res.error ?? 'unknown error'}`;
    } catch (e) {
      error = errorMessage(e);
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
    error = null;
    try {
      await createRule(orgId, {
        name: rName,
        trigger_type: rTrigger,
        conditions: buildConditions(),
        severity: rSeverity,
        throttle_seconds: num(rThrottle, 300),
        message_template: rTemplate || null,
        channel_ids: rChannels,
      });
      showRuleForm = false;
      rName = '';
      rTemplate = '';
      rChannels = [];
      await load();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      savingRule = false;
    }
  }

  async function toggleRule(r: AlertRule) {
    try {
      await updateRule(r.id, { enabled: !r.enabled });
      await load();
    } catch (e) {
      error = errorMessage(e);
    }
  }

  async function doDelete() {
    const target = confirmDelete;
    if (!target) return;
    confirmDelete = null;
    try {
      if (target.kind === 'channel') await deleteChannel(target.id);
      else await deleteRule(target.id);
      await load();
    } catch (e) {
      error = errorMessage(e);
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

  const fmtTime = (iso: string) => new Date(iso).toLocaleString();

  const channelName = (id: string | null) =>
    channels.find((c) => c.id === id)?.name ?? '—';

  $effect(() => {
    if (orgId) void load();
  });
</script>

<AppShell>
  <div class="alerts">
    <header class="head">
      <div>
        <h1 class="page-title">Alerts</h1>
        <p class="sub muted">
          Deliver notifications to email, Slack, Discord, Element/Matrix, Telegram or any
          webhook — on triggers you define.
        </p>
      </div>
      <div class="controls">
        <RefreshButton onclick={refresh} loading={refreshing} />
      </div>
    </header>

    <nav class="tabs" aria-label="Alert settings sections">
      <button class="tab" class:active={tab === 'channels'} onclick={() => (tab = 'channels')}>
        Channels <span class="count">{channels.length}</span>
      </button>
      <button class="tab" class:active={tab === 'rules'} onclick={() => (tab = 'rules')}>
        Rules <span class="count">{rules.length}</span>
      </button>
      <button class="tab" class:active={tab === 'history'} onclick={() => (tab = 'history')}>
        History
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
          A channel is where an alert is delivered. Secrets are encrypted at rest and never
          returned by the API.
        </p>
        {#if !showChannelForm}
          <Button variant="primary" lockedReason={writeLock} onclick={() => (showChannelForm = true)}>
            New channel
          </Button>
        {/if}
      </div>

      {#if showChannelForm}
        <Card title="New channel">
          <div class="form-grid">
            <Input label="Name" bind:value={chName} placeholder="Ops Slack" required />

            <div class="field">
              <label class="lbl" for="ch-kind">Type</label>
              <div class="control select">
                <select id="ch-kind" bind:value={chKind}>
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
                  label="Incoming webhook URL"
                  bind:value={chFields.webhook_url}
                  placeholder="https://hooks.slack.com/services/…"
                  hint="Stored encrypted. Create it in your workspace’s Incoming Webhooks app."
                  required
                />
              </div>
            {:else if chKind === 'email'}
              <Input
                label="SMTP host"
                bind:value={chFields.host}
                placeholder="smtp.example.com"
                required
              />
              <Input
                label="Port"
                bind:value={chFields.port}
                placeholder="587"
                hint="587 = STARTTLS, 465 = implicit TLS."
              />
              <Input
                label="From address"
                bind:value={chFields.from}
                placeholder="sauron@example.com"
                required
              />
              <Input
                label="Recipients"
                bind:value={chFields.to}
                placeholder="oncall@example.com, sre@example.com"
                hint="Comma-separated."
                required
              />
              <Input
                label="Username"
                bind:value={chFields.username}
                placeholder="Optional"
              />
              <Input
                label="Password"
                type="password"
                bind:value={chFields.password}
                placeholder="Optional"
                hint="Encrypted at rest."
              />
            {:else if chKind === 'matrix'}
              <Input
                label="Homeserver"
                bind:value={chFields.homeserver}
                placeholder="https://matrix.org"
                required
              />
              <Input
                label="Room ID"
                bind:value={chFields.room_id}
                placeholder="!abcdef:matrix.org"
                required
              />
              <div class="span-2">
                <Input
                  label="Access token"
                  type="password"
                  bind:value={chFields.access_token}
                  hint="Encrypted at rest."
                  required
                />
              </div>
            {:else if chKind === 'telegram'}
              <Input
                label="Chat ID"
                bind:value={chFields.chat_id}
                placeholder="-1001234567890"
                required
              />
              <Input
                label="Bot token"
                type="password"
                bind:value={chFields.bot_token}
                hint="Encrypted at rest."
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
                  label="Signing secret"
                  type="password"
                  bind:value={chFields.signing_secret}
                  hint="Optional. When set, requests carry an x-sauron-signature HMAC of the body."
                />
              </div>
            {/if}
          </div>

          <div class="form-foot">
            <Button variant="ghost" onclick={() => (showChannelForm = false)}>Cancel</Button>
            <Button
              variant="primary"
              loading={savingChannel}
              disabled={!chName}
              onclick={submitChannel}
            >
              Create channel
            </Button>
          </div>
        </Card>
      {/if}

      {#if channels.length === 0}
        <EmptyState
          title="No channels yet"
          description="Add a destination — email, Slack, Discord, Element/Matrix, Telegram or a webhook — before creating rules."
          icon="bell"
        >
          {#snippet action()}
            {#if !showChannelForm}
              <Button variant="primary" lockedReason={writeLock} onclick={() => (showChannelForm = true)}>
                New channel
              </Button>
            {/if}
          {/snippet}
        </EmptyState>
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <th>Name</th>
              <th>Type</th>
              <th>Secret</th>
              <th>Status</th>
              <th class="num">Actions</th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each channels as c (c.id)}
              <tr>
                <td>{c.name}</td>
                <td>{KIND_LABELS[c.kind] ?? c.kind}</td>
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
                    title="Send a test notification"
                  >
                    Test
                  </Button>
                  <Button size="sm" lockedReason={writeLock} onclick={() => toggleChannel(c)}>
                    {c.enabled ? 'Disable' : 'Enable'}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    lockedReason={writeLock}
                    onclick={() => (confirmDelete = { kind: 'channel', id: c.id, name: c.name })}
                  >
                    Delete
                  </Button>
                </td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>
      {/if}
    {:else if tab === 'rules'}
      <!-- ---------------- Rules ---------------- -->
      <div class="section-head">
        <p class="muted small">
          A rule decides when to notify and which channels to fan out to. Repeat alerts for the
          same cause are suppressed for the throttle window.
        </p>
        {#if !showRuleForm}
          <Button
            variant="primary"
            lockedReason={writeLock}
            disabled={channels.length === 0}
            title={channels.length === 0 ? 'Create a channel first' : undefined}
            onclick={() => (showRuleForm = true)}
          >
            New rule
          </Button>
        {/if}
      </div>

      {#if showRuleForm}
        <Card title="New alert rule">
          <div class="form-grid">
            <Input label="Name" bind:value={rName} placeholder="API down → oncall" required />

            <div class="field">
              <label class="lbl" for="r-trigger">Trigger</label>
              <div class="control select">
                <select id="r-trigger" bind:value={rTrigger}>
                  {#each Object.entries(TRIGGER_LABELS) as [k, label] (k)}
                    <option value={k}>{label}</option>
                  {/each}
                </select>
                <span class="affix"><Icon name="chevron-down" size={15} /></span>
              </div>
            </div>

            {#if needs.threshold}
              <div class="field">
                <label class="lbl" for="r-cmp">Condition</label>
                <div class="control select">
                  <select id="r-cmp" bind:value={rComparator}>
                    <option value="gte">is at least</option>
                    <option value="gt">is more than</option>
                    <option value="lte">is at most</option>
                    <option value="lt">is less than</option>
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
                <label class="lbl" for="r-metric">Metric</label>
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
                label="Spike factor (×)"
                bind:value={rSpikeFactor}
                hint="Fire when the window exceeds the previous window by this multiple."
              />
            {/if}

            {#if needs.window}
              <Input
                label="Window (minutes)"
                bind:value={rWindowMinutes}
                hint="Max 24 hours."
              />
            {/if}

            {#if needs.level}
              <Input
                label="Level filter"
                bind:value={rLevel}
                placeholder="error"
                hint="Optional — debug, info, warning, error, fatal."
              />
            {/if}

            {#if needs.eventName}
              <Input label="Event name" bind:value={rEventName} placeholder="checkout_completed" />
            {/if}

            <Input
              label="Environment filter"
              bind:value={rEnvironment}
              placeholder="production"
              hint="Optional."
            />

            <div class="field">
              <label class="lbl" for="r-sev">Severity</label>
              <div class="control select">
                <select id="r-sev" bind:value={rSeverity}>
                  <option value="info">Info</option>
                  <option value="warning">Warning</option>
                  <option value="critical">Critical</option>
                </select>
                <span class="affix"><Icon name="chevron-down" size={15} /></span>
              </div>
            </div>

            <Input
              label="Throttle (seconds)"
              bind:value={rThrottle}
              hint="Suppress repeats of the same alert for this long."
            />

            <div class="span-2">
              <label class="lbl" for="r-template">Message template</label>
              <textarea
                id="r-template"
                class="textarea"
                bind:value={rTemplate}
                rows="3"
                placeholder="{'{{monitor}}'} is {'{{status}}'} — {'{{cause}}'}"
              ></textarea>
              <p class="hint">
                Optional. Use <code>{'{{variable}}'}</code> placeholders.
                {#if meta?.template_vars?.[rTrigger]}
                  Available: {meta.template_vars[rTrigger].map((v) => `{{${v}}}`).join(', ')}
                {/if}
              </p>
            </div>

            <div class="span-2">
              <span class="lbl">Channels</span>
              <div class="chips">
                {#each channels as c (c.id)}
                  <button
                    type="button"
                    class="chip"
                    class:selected={rChannels.includes(c.id)}
                    onclick={() => toggleRuleChannel(c.id)}
                  >
                    {c.name}
                    <span class="chip-kind">{KIND_LABELS[c.kind] ?? c.kind}</span>
                  </button>
                {/each}
              </div>
            </div>
          </div>

          <div class="form-foot">
            <Button variant="ghost" onclick={() => (showRuleForm = false)}>Cancel</Button>
            <Button
              variant="primary"
              loading={savingRule}
              disabled={!rName || rChannels.length === 0}
              onclick={submitRule}
            >
              Create rule
            </Button>
          </div>
        </Card>
      {/if}

      {#if rules.length === 0}
        <EmptyState
          title="No alert rules yet"
          description="Define when Sauron should notify you — a monitor going down, a new issue, an error spike, or a latency threshold."
          icon="bell"
        >
          {#snippet action()}
            {#if !showRuleForm && channels.length > 0}
              <Button variant="primary" lockedReason={writeLock} onclick={() => (showRuleForm = true)}>
                New rule
              </Button>
            {/if}
          {/snippet}
        </EmptyState>
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <th>Name</th>
              <th>Trigger</th>
              <th>Severity</th>
              <th class="num">Channels</th>
              <th class="num">Throttle</th>
              <th>Status</th>
              <th class="num">Actions</th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each rules as r (r.id)}
              <tr>
                <td>{r.name}</td>
                <td>{TRIGGER_LABELS[r.trigger_type] ?? r.trigger_type}</td>
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
                  <Button
                    size="sm"
                    variant="ghost"
                    lockedReason={writeLock}
                    onclick={() => (confirmDelete = { kind: 'rule', id: r.id, name: r.name })}
                  >
                    Delete
                  </Button>
                </td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>
      {/if}
    {:else}
      <!-- ---------------- History ---------------- -->
      {#if history.length === 0}
        <EmptyState
          title="No alerts delivered yet"
          description="Once a rule fires, every delivery attempt is recorded here with its outcome."
          icon="inbox"
        />
      {:else}
        <DataTable>
          {#snippet head()}
            <tr>
              <th>When</th>
              <th>Title</th>
              <th>Channel</th>
              <th>Status</th>
              <th class="num">Attempts</th>
            </tr>
          {/snippet}
          {#snippet children()}
            {#each history as h (h.id)}
              <tr>
                <td class="nowrap">{fmtTime(h.created_at)}</td>
                <td>
                  {h.title}
                  {#if h.error}
                    <div class="err-detail">{h.error}</div>
                  {/if}
                </td>
                <td>{channelName(h.channel_id)}</td>
                <td><Badge tone={statusTone(h.status)} size="sm">{h.status}</Badge></td>
                <td class="num">{h.attempts}</td>
              </tr>
            {/each}
          {/snippet}
        </DataTable>
      {/if}
    {/if}
  </div>

  {#if confirmDelete}
    <ConfirmDialog
      open
      title={`Delete ${confirmDelete.kind}?`}
      message={`“${confirmDelete.name}” will be removed. This cannot be undone.`}
      confirmLabel="Delete"
      danger
      onconfirm={doDelete}
      oncancel={() => (confirmDelete = null)}
    />
  {/if}
</AppShell>

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
    margin-left: 6px;
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
    right: 10px;
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
