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
import { Scope } from './scope.js';
import { resetScreen, setScreenState } from './screen.js';
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

  isEnabled(): boolean {
    return this.enabled;
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
      environment: this.options.environment,
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
    if (item.tags === undefined) {
      const tags = this.scope.tags;
      if (Object.keys(tags).length > 0) item.tags = { ...tags };
    }
    if (item.user === undefined && this.scope.hasUser()) {
      item.user = this.scope.getUser();
    }
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
    environment: options.environment ?? 'production',
    release: options.release ?? null,
    sampleRate: clamp(options.sampleRate ?? 1, 0, 1),
    maxBreadcrumbs: options.maxBreadcrumbs ?? 50,
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
