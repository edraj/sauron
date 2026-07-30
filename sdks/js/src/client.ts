import { buildContext } from './context.js';
import { parseDsn, type Dsn } from './dsn.js';
import { buildEnvelope } from './envelope.js';
import { getDeviceId, getSessionId } from './identity.js';
import { installConsole } from './integrations/console.js';
import { installDom } from './integrations/dom.js';
import { installFetch } from './integrations/fetch.js';
import { installGlobalHandlers } from './integrations/globalHandlers.js';
import { installHistory, onNavigation } from './integrations/history.js';
import * as instrument from './integrations/instrument.js';
import { installPerformance } from './integrations/performance.js';
import { installXhr } from './integrations/xhr.js';
import { setScreen } from './api/product.js';
import { Scope, mergeMeta } from './scope.js';
import { resetScreen, setScreenState } from './screen.js';
import { getWorkflow, resetWorkflow } from './workflow.js';
import { installBeacon } from './transport/beacon.js';
import { Transport } from './transport/transport.js';
import type {
  Breadcrumb,
  Envelope,
  EnvelopeItem,
  ErrorItem,
  Hint,
  InitOptions,
  ResolvedOptions,
} from './types.js';
import { clamp, makeLogger, nowIso, SDK_NAME, SDK_VERSION, uuidv4 } from './utils.js';

type Logger = ReturnType<typeof makeLogger>;

/**
 * The Sauron client singleton. Owns the resolved options, the scope
 * (user + breadcrumbs), the transport, and the installed integrations.
 */
export class SauronClient {
  readonly options: ResolvedOptions;
  readonly dsn: Dsn;

  private readonly scope: Scope;
  private readonly transport: Transport;
  private readonly logger: Logger;
  private readonly nativeFetch?: typeof fetch;

  private enabled = true;
  private installed = false;
  private anonymousId: string | null = null;
  private beaconCleanup: (() => void) | null = null;

  constructor(options: ResolvedOptions) {
    this.options = options;
    this.dsn = parseDsn(options.dsn);
    this.logger = makeLogger(options.debug);
    this.scope = new Scope(options.maxBreadcrumbs);
    // Seed init-default metadata into the global scope so every later signal
    // inherits it; runtime setters still last-write-win over these.
    this.scope.setTags(options.tags);
    for (const [name, block] of Object.entries(options.contexts)) {
      this.scope.setContext(name, block);
    }
    for (const [key, value] of Object.entries(options.extra)) {
      this.scope.setExtra(key, value);
    }

    // Capture the NATIVE fetch before any integration wraps it, so the
    // transport's own requests never hit our instrumentation.
    const g = globalThis as { fetch?: typeof fetch };
    this.nativeFetch = typeof g.fetch === 'function' ? g.fetch.bind(globalThis) : undefined;

    instrument.setDsnHost(this.dsn.host);

    this.transport = new Transport({
      dsn: this.dsn,
      options: options.transport,
      makeEnvelope: (items) => this.makeEnvelope(items),
      fetchImpl: this.nativeFetch,
      logger: this.logger,
      onDisable: () => this.disable(),
    });
  }

  /** Install global handlers + auto-instrumentation and start the transport. */
  install(): void {
    if (this.installed) return;
    this.installed = true;

    // Establish the durable device id and the current session id at init.
    getDeviceId();
    getSessionId();

    installGlobalHandlers();
    installConsole();
    installDom();
    installHistory();
    installFetch();
    installXhr();
    if (this.options.performance) installPerformance();

    // Screen tracking: seed the initial screen, then follow SPA navigations.
    if (this.options.screen) setScreenState(this.options.screen);
    if (this.options.screenTracking) {
      onNavigation((path) => setScreen(path));
    }

    this.beaconCleanup = installBeacon(this.transport);

    this.transport.start();
    void this.transport.drainOfflineQueue();
    this.logger.log('initialized', { dsn: this.dsn.host, project: this.dsn.projectId });
  }

  getScope(): Scope {
    return this.scope;
  }

  /**
   * False once this client was explicitly disabled/closed, OR once the
   * transport has auto-disabled itself on a 401/403 (revoked/invalid DSN
   * key) — computed from the transport's own state on every call, not a
   * separately mirrored flag, so a propagation regression there cannot leave
   * this predicate stale. `this.transport` always exists once a client
   * exists (it is constructed synchronously in the constructor); the
   * "nothing installed yet" case is instead handled one layer up, by every
   * module-level API (`startWorkflow`, `track`, ...) treating `getClient() ===
   * null` as the no-op/disabled case before it ever reaches here.
   */
  isEnabled(): boolean {
    return this.enabled && this.transport.isEnabled();
  }

  /** The current distinct id: the user id when identified, else an anon id. */
  getDistinctId(): string | null {
    const user = this.scope.getUser();
    if (user.id) return user.id;
    return this.ensureAnonymousId();
  }

  /** The anonymous id, or null if one was never needed. */
  getAnonymousId(): string | null {
    return this.anonymousId;
  }

  private ensureAnonymousId(): string {
    if (!this.anonymousId) this.anonymousId = `anon_${uuidv4()}`;
    return this.anonymousId;
  }

  /** Stamp a fresh envelope (new `sent_at`, current context) around `items`. */
  makeEnvelope(items: EnvelopeItem[]): Envelope {
    const header = {
      dsn: this.dsn.raw,
      sdk: { name: SDK_NAME, version: SDK_VERSION },
      sent_at: nowIso(),
      release: this.options.release,
    };
    const context = buildContext(this.options.release, this.scope.getUser());
    return buildEnvelope(header, context, items);
  }

  /** Add a breadcrumb, running it through `beforeBreadcrumb` first. */
  addBreadcrumb(breadcrumb: Breadcrumb, hint?: Hint): void {
    if (!this.enabled) return;
    let processed: Breadcrumb | null = breadcrumb;
    if (this.options.beforeBreadcrumb) {
      try {
        processed = this.options.beforeBreadcrumb(breadcrumb, hint);
      } catch (err) {
        this.logger.warn('beforeBreadcrumb threw', err);
        processed = breadcrumb;
      }
    }
    if (!processed) return;
    this.scope.addBreadcrumb(processed);
  }

  /**
   * Reconcile an error item to the shared wire shape by filling the optional
   * `event_id`/`message`/`tags`/`user` fields from the current scope and hint.
   * Each field is left untouched when the item already sets it, and omitted
   * entirely when there is nothing to attach (the backend defaults it) — only
   * `event_id` is always minted so callers can correlate the report.
   */
  private enrichErrorItem(item: ErrorItem, hint?: Hint): void {
    if (item.event_id === undefined) {
      const hinted = hint?.event_id;
      item.event_id = typeof hinted === 'string' ? hinted : uuidv4();
    }
    if (item.message === undefined && typeof hint?.message === 'string') {
      item.message = hint.message;
    }
    const tags = mergeMeta(this.scope.tags, item.tags);
    if (Object.keys(tags).length > 0) item.tags = tags;
    const contexts = mergeMeta(this.scope.contexts, item.contexts);
    if (Object.keys(contexts).length > 0) item.contexts = contexts;
    const extra = mergeMeta(this.scope.extra, item.extra);
    if (Object.keys(extra).length > 0) item.extra = extra;
    if (item.user === undefined && this.scope.hasUser()) {
      item.user = this.scope.getUser();
    }
  }

  /**
   * Stamp the active workflow (if any) onto a signal item.
   *
   * Done HERE — the single choke point every capture path funnels through —
   * rather than at each item-construction site, so a capture path added later
   * is stamped by construction instead of by remembering to. The keys are
   * ASSIGNED ONLY when a workflow is active: an item with no workflow keeps
   * them absent entirely (not present-as-`undefined`), which is what makes
   * `JSON.stringify` omit them and keeps the no-workflow wire bytes identical
   * to pre-1.3.0.
   *
   * Only error/event/transaction carry `workflow_id`/`workflow_name` columns
   * server-side — identify and breadcrumb_batch items are deliberately left
   * alone. An item that already carries an explicit `workflow_id` is left
   * untouched, matching how `enrichErrorItem` defers to caller-set fields.
   */
  private stampWorkflow(item: EnvelopeItem): void {
    if (item.type !== 'error' && item.type !== 'event' && item.type !== 'transaction') return;

    // `captureItem` is a public escape hatch, so an item can arrive already
    // attributed. Caller-set wins — same rule `enrichErrorItem` applies to
    // `event_id`/`message`/`user`.
    //
    // But the two fields are a PAIR, not two independent options: the server
    // guards on `if let (Some(id), Some(name))`, so an item carrying only one
    // of them is silently dropped from every workflow query. That failure is
    // invisible, so warn rather than let it through quietly — and still leave
    // the item alone, because overwriting a caller's deliberate (if partial)
    // attribution with a different workflow would be worse than not stamping.
    const hasId = item.workflow_id !== undefined;
    const hasName = item.workflow_name !== undefined;
    if (hasId || hasName) {
      if (hasId !== hasName) {
        this.logger.warn(
          'item sets only one of workflow_id/workflow_name; the server treats them as a ' +
            'pair and will drop this attribution. Set both, or neither.',
        );
      }
      return;
    }

    const workflow = getWorkflow();
    if (!workflow) return;
    item.workflow_id = workflow.workflowId;
    item.workflow_name = workflow.name;
  }

  /**
   * Run an item through sampling (errors only) and `beforeSend`, then hand it to
   * the transport. Returns silently when dropped.
   */
  captureItem(item: EnvelopeItem, hint?: Hint): void {
    if (!this.enabled) return;

    if (item.type === 'error') {
      if (Math.random() >= this.options.sampleRate) {
        this.logger.log('dropped error by sampleRate');
        return;
      }
      this.enrichErrorItem(item, hint);
    }

    // Before `beforeSend`, so a consumer's hook sees the workflow fields.
    this.stampWorkflow(item);

    let processed: EnvelopeItem | null = item;
    if (this.options.beforeSend) {
      try {
        processed = this.options.beforeSend(item, hint);
      } catch (err) {
        this.logger.warn('beforeSend threw', err);
        processed = item;
      }
    }
    if (!processed) {
      this.logger.log('dropped by beforeSend');
      return;
    }

    this.transport.send(processed);
  }

  /** Flush pending events. Resolves false if `timeoutMs` elapses first. */
  flush(timeoutMs?: number): Promise<boolean> {
    return this.transport.flush(timeoutMs);
  }

  /** Disable the client (called on 401/403). Stops accepting/sending events. */
  disable(): void {
    if (!this.enabled) return;
    this.enabled = false;
    this.transport.disable();
    this.logger.warn('client disabled');
  }

  /** Restore all patched globals and stop timers/listeners. */
  teardown(): void {
    this.enabled = false;
    this.transport.stop();
    if (this.beaconCleanup) {
      this.beaconCleanup();
      this.beaconCleanup = null;
    }
    onNavigation(null);
    resetScreen();
    resetWorkflow();
    instrument.unpatchAll();
    instrument.setDsnHost(null);
    this.installed = false;
  }

  /** Flush then tear down. Resolves to the flush result. */
  async close(timeoutMs?: number): Promise<boolean> {
    const flushed = await this.transport.flush(timeoutMs);
    this.teardown();
    return flushed;
  }
}

/* ---------------------------------------------------------------- singleton */

let currentClient: SauronClient | null = null;

/** The active client, or null before `init`. */
export function getClient(): SauronClient | null {
  return currentClient;
}

function resolveOptions(options: InitOptions): ResolvedOptions {
  if (!options || typeof options.dsn !== 'string' || options.dsn.length === 0) {
    throw new Error('[sauron] init() requires a `dsn`');
  }
  const t = options.transport ?? {};
  return {
    dsn: options.dsn,
    release: options.release ?? null,
    sampleRate: clamp(options.sampleRate ?? 1, 0, 1),
    maxBreadcrumbs: options.maxBreadcrumbs ?? 50,
    tags: options.tags ?? {},
    contexts: options.contexts ?? {},
    extra: options.extra ?? {},
    beforeSend: options.beforeSend,
    beforeBreadcrumb: options.beforeBreadcrumb,
    transport: {
      flushIntervalMs: t.flushIntervalMs ?? 5000,
      maxBatch: t.maxBatch ?? 30,
      maxQueueBytes: t.maxQueueBytes ?? 1048576,
    },
    performance: options.performance ?? false,
    screen: options.screen,
    screenTracking: options.screenTracking ?? false,
    debug: options.debug ?? false,
  };
}

/**
 * Initialize the SDK. Idempotent: a second call tears down the previous client
 * (restoring patched globals) before installing a fresh one.
 */
export function init(options: InitOptions): SauronClient {
  if (currentClient) {
    try {
      currentClient.teardown();
    } catch {
      /* ignore teardown failures */
    }
  }
  const resolved = resolveOptions(options);
  const client = new SauronClient(resolved);
  currentClient = client;
  client.install();
  return client;
}
