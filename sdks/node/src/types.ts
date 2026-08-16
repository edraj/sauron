/**
 * Sauron wire-contract types (server-side subset).
 *
 * These mirror the LOCKED envelope shape consumed by the Rust ingest gateway
 * (`sauron-core/src/envelope.rs`). Field names, nullability and ordering are
 * load-bearing — do not "clean them up".
 */

/** Severity level. Matches the backend enum exactly. */
export type Level = 'debug' | 'info' | 'warning' | 'error' | 'fatal';

/** A single normalized stack frame (raw, never symbolicated). */
export interface Frame {
  function: string | null;
  module: string | null;
  filename: string | null;
  abs_path: string | null;
  lineno: number | null;
  colno: number | null;
  in_app: boolean;
}

/** How an exception reached the SDK. */
export interface Mechanism {
  type: string;
  handled: boolean;
}

/** The exception payload of an error item. */
export interface ExceptionValue {
  type: string;
  value: string | null;
  mechanism: Mechanism;
  stacktrace: Frame[];
}

/** User attribution attached to an error item. */
export interface ErrorUser {
  id: string | null;
  email: string | null;
  username: string | null;
}

/** Scope user input — a superset of {@link ErrorUser} accepted by `setUser`. */
export interface User {
  id?: string | null;
  email?: string | null;
  username?: string | null;
}

/**
 * A stored breadcrumb, matching `envelope.rs::Breadcrumb`. Attached to captured
 * errors from the active scope's ring buffer.
 */
export interface Breadcrumb {
  type: string;
  category: string | null;
  message: string | null;
  level: string | null;
  timestamp: string;
  data: Record<string, unknown>;
}

/** Caller-supplied breadcrumb; missing fields are defaulted, `timestamp` stamped. */
export interface BreadcrumbInput {
  type?: string;
  category?: string;
  message?: string;
  level?: Level;
  data?: Record<string, unknown>;
}

/** The mutable state carried by a {@link Scope}. */
export interface ScopeData {
  user: User | null;
  tags: Record<string, string>;
  contexts: Record<string, unknown>;
  extra: Record<string, unknown>;
  breadcrumbs: Breadcrumb[];
  /**
   * The workflow bounded by `startWorkflow`/`endWorkflow`/`cancelWorkflow` on
   * THIS scope, or `null` if none. Request-isolated: the async-local child
   * scope created by `withScope`/`runWithAsyncScope` gets its own copy, so
   * concurrent requests never observe each other's workflow.
   */
  workflow: ActiveWorkflow | null;
}

/** Exactly six reachable outcomes for a workflow lifecycle call. No seventh. */
export type WorkflowStatus =
  | 'ok'
  | 'already_active'
  | 'not_active'
  | 'name_mismatch'
  | 'invalid_name'
  | 'disabled';

/** Return value of `startWorkflow` / `endWorkflow` / `cancelWorkflow`. */
export interface WorkflowResult {
  status: WorkflowStatus;
  /** Present when `status` is `'ok'`. */
  workflowId?: string;
}

/** The workflow currently bounded on a scope. */
export interface ActiveWorkflow {
  /** Fresh client-generated UUID v4, minted by `startWorkflow`. */
  workflowId: string;
  /** Trimmed, caller-supplied name (see `normalizeWorkflowName`). */
  name: string;
  /** ISO-8601 timestamp of when the workflow started. */
  startedAt: string;
}

/** An error item (manual `captureException` / `captureMessage`). */
export interface ErrorItem {
  type: 'error';
  event_id: string;
  level: Level;
  timestamp: string;
  exception: ExceptionValue;
  message: string | null;
  breadcrumbs: Breadcrumb[];
  tags: Record<string, string>;
  contexts?: Record<string, unknown>;
  extra?: Record<string, unknown>;
  fingerprint: string[] | null;
  user: ErrorUser | null;
  session_id: string | null;
  /**
   * Id/name of the workflow this error occurred within, if the SDK bounded
   * one via `startWorkflow`. Optional-not-nullable: apps that never use
   * workflows must be byte-identical to before this field existed, so an
   * absent workflow means the key is OMITTED from the item, never `null`.
   */
  workflow_id?: string;
  workflow_name?: string;
  screen: string | null;
}

/** A product-analytics event (PostHog-style `track`). */
export interface EventItem {
  type: 'event';
  name: string;
  distinct_id: string;
  properties: Record<string, unknown>;
  timestamp: string;
  session_id: string | null;
  /** See {@link ErrorItem.workflow_id} — same omit-not-null convention. */
  workflow_id?: string;
  workflow_name?: string;
  screen: string | null;
  tags?: Record<string, string>;
  contexts?: Record<string, unknown>;
  extra?: Record<string, unknown>;
}

/** An identity association (PostHog-style `identify`). */
export interface IdentifyItem {
  type: 'identify';
  distinct_id: string;
  anonymous_id: string | null;
  traits: Record<string, unknown>;
  timestamp: string;
}

/**
 * A performance transaction — one timed operation. Matches
 * `envelope.rs::TransactionItem`. Optional fields are omitted from the wire
 * JSON when absent (never serialized as `null`).
 */
export interface TransactionItem {
  type: 'transaction';
  name: string;
  op: string;
  duration_ms: number;
  status?: string;
  http_method?: string;
  http_status?: number;
  url?: string;
  distinct_id?: string;
  /** See {@link ErrorItem.workflow_id} — same omit-not-null convention. */
  workflow_id?: string;
  workflow_name?: string;
  timestamp: string;
  /**
   * Developer-supplied flat string tags for THIS transaction. NOT merged with
   * the scope — see {@link TransactionInput.tags}. Omitted when empty.
   */
  tags?: Record<string, string>;
  /**
   * Developer-supplied freeform JSON for THIS transaction. Capped and replaced
   * with a truncation marker past `MAX_TRANSACTION_EXTRA_BYTES`. Omitted when
   * empty.
   */
  extra?: Record<string, unknown>;
}

/** Caller input for {@link TransactionItem} via `trackTransaction`. */
export interface TransactionInput {
  name: string;
  /** Operation class: `navigation | http | resource | screen_load | custom`. Default `custom`. */
  op?: string;
  duration_ms?: number;
  /**
   * Accepted alias for {@link duration_ms}.
   *
   * The browser SDK's equivalent input takes `durationMs` (`sdks/js`), so a
   * snippet moved between the two — or any plain-JavaScript caller, where the
   * type checker is not there to object — used to produce a transaction with no
   * duration at all. The item still validated and still shipped, just without
   * the one field it exists to carry, which is the worst possible outcome: no
   * error anywhere and a silently useless performance record.
   *
   * Supply exactly one. `duration_ms` wins if both are present.
   */
  durationMs?: number;
  status?: string;
  http_method?: string;
  http_status?: number;
  url?: string;
  /** Falls back to the scoped user's id when omitted. */
  distinct_id?: string;
  /**
   * Flat string tags for this transaction.
   *
   * **Per-call only — the scope is NOT merged in**, which is the one place
   * transactions differ from `track()` and `captureException()`. Those two
   * merge `setTag`/`setExtra` defaults; a transaction carries only what its own
   * call site attached. Transactions are the highest-volume signal (one per
   * navigation and per HTTP call), so inheriting a global blob would write it
   * onto every row.
   */
  tags?: Record<string, string>;
  /**
   * Freeform JSON for this transaction — the request body, the response body,
   * an order id, a retry count.
   *
   * Per-call only, for the reason on {@link TransactionInput.tags}. Serialized
   * and capped at `MAX_TRANSACTION_EXTRA_BYTES`; past that the whole map is
   * replaced with `{ _truncated: true, _bytes: N }` so one large body cannot
   * take a batched envelope over the ingest limit and drop every span in it.
   *
   * Nothing here is scrubbed. `beforeSend` is the redaction seam.
   */
  extra?: Record<string, unknown>;
}

/** Any item that can appear in an envelope's `items` array. */
export type EnvelopeItem = ErrorItem | EventItem | IdentifyItem | TransactionItem;

/** A hook run on every outgoing item; return `null` to drop it. */
export type BeforeSend = (item: EnvelopeItem, hint?: unknown) => EnvelopeItem | null;

/** A hook run on every breadcrumb; return `null` to drop it. */
export type BeforeBreadcrumb = (crumb: Breadcrumb, hint?: unknown) => Breadcrumb | null;

/* ------------------------------------------------------------------ context */

export interface DeviceContext {
  device_id: string;
}

export interface OsContext {
  name: string | null;
  version: string | null;
}

export interface RuntimeContext {
  name: string | null;
  version: string | null;
}

export interface Context {
  device: DeviceContext;
  os: OsContext;
  app: Record<string, unknown>;
  runtime: RuntimeContext;
  user: null;
}

/* ------------------------------------------------------------------- header */

export interface SdkInfo {
  name: string;
  version: string;
}

export interface EnvelopeHeader {
  dsn: string;
  sdk: SdkInfo;
  sent_at: string;
  release: string | null;
}

/** The complete, serializable envelope posted to the ingest gateway. */
export interface Envelope {
  header: EnvelopeHeader;
  context: Context;
  items: EnvelopeItem[];
}

/* -------------------------------------------------------------------- input */

/** A minimal `Headers`-like view over a response (for reading `Retry-After`). */
export interface ResponseHeadersLike {
  get(name: string): string | null;
}

/** The subset of a `fetch` `Response` the transport inspects. */
export interface FetchResponse {
  status: number;
  ok?: boolean;
  headers?: ResponseHeadersLike;
}

/**
 * A subset of the DOM `fetch` used by the transport. Injectable for tests.
 * The body may be a gzip `Uint8Array`/`Buffer` when compression kicks in.
 */
export type FetchLike = (
  url: string,
  init: {
    method: string;
    headers: Record<string, string>;
    body: string | Uint8Array;
  },
) => Promise<FetchResponse>;

/** Optional deterministic sleep seam (defaults to a real `setTimeout` promise). */
export type SleepFn = (ms: number) => Promise<void>;

/**
 * The subset of Node's `process` the opt-in auto-capture / shutdown hooks touch.
 * Injectable so tests can drive the handlers without registering real
 * process-level listeners or terminating the test runner.
 */
export interface ProcessLike {
  on(event: string, listener: (...args: any[]) => void): unknown;
  removeListener(event: string, listener: (...args: any[]) => void): unknown;
  listeners(event: string): Array<(...args: any[]) => void>;
  exit(code?: number): void;
}

/** Transport tuning knobs. */
export interface TransportOptions {
  /** How often the pending batch is flushed, in ms. Default 5000. */
  flushIntervalMs?: number;
  /** Max items per envelope before an eager flush. Default 30. */
  maxBatch?: number;
  /** Injected HTTP sender. Defaults to global `fetch`. */
  fetchImpl?: FetchLike;
}

/** Options accepted by `init`. */
export interface InitOptions {
  /** `https://<public_key>@<host>/<project_id>` */
  dsn: string;
  release?: string | null;
  /** Default tags seeded into the global scope at init. */
  tags?: Record<string, string>;
  /** Default named dev context blocks seeded into the global scope at init. Distinct from the machine `context`. */
  contexts?: Record<string, unknown>;
  /** Default freeform extra values seeded into the global scope at init. */
  extra?: Record<string, unknown>;
  /** Error sample rate in [0, 1]. Default 1 (send everything). */
  sampleRate?: number;
  /** How often the pending batch is flushed, in ms. Default 5000. */
  flushInterval?: number;
  /** Max items per envelope before an eager flush. Default 30. */
  maxBatch?: number;
  /** Breadcrumb ring-buffer size on the global scope. Default 100. */
  maxBreadcrumbs?: number;
  /** Gzip the request body once it exceeds this many bytes. Default 1024. */
  gzipThresholdBytes?: number;
  /** Drop-oldest byte cap for the in-memory send buffer. Default 1 MiB. */
  maxQueueBytes?: number;
  /** Opt-in directory for FIFO disk persistence of pending envelopes. Default off. */
  offlineDir?: string;
  /** Max retries after the first attempt for transient failures. Default 3. */
  maxRetries?: number;
  /**
   * Opt-in: capture uncaught exceptions / unhandled rejections with
   * `mechanism.handled = false`. Default `false`. Never swallows the crash —
   * the process's default exit behavior is preserved after flushing.
   */
  autoCaptureUnhandled?: boolean;
  /**
   * Opt-in: wire `beforeExit`/`SIGTERM`/`SIGINT` to `close()` for a graceful
   * flush on shutdown. Default `false`. Explicit `close()` still works.
   */
  autoShutdown?: boolean;
  /** Runs on every outgoing item just before enqueue; return `null` to drop it. */
  beforeSend?: BeforeSend;
  /** Runs on every breadcrumb before it is stored; return `null` to drop it. */
  beforeBreadcrumb?: BeforeBreadcrumb;
  /** Injected HTTP sender (for tests). Defaults to global `fetch`. */
  fetchImpl?: FetchLike;
  debug?: boolean;
}

/** Fully-resolved options with all defaults applied. */
export interface ResolvedOptions {
  dsn: string;
  release: string | null;
  tags: Record<string, string>;
  contexts: Record<string, unknown>;
  extra: Record<string, unknown>;
  sampleRate: number;
  flushInterval: number;
  maxBatch: number;
  maxBreadcrumbs: number;
  gzipThresholdBytes: number;
  maxQueueBytes: number;
  offlineDir: string | null;
  maxRetries: number;
  autoCaptureUnhandled: boolean;
  autoShutdown: boolean;
  beforeSend?: BeforeSend;
  beforeBreadcrumb?: BeforeBreadcrumb;
  fetchImpl?: FetchLike;
  debug: boolean;
}

/**
 * Per-capture metadata overrides shared by `captureMessage` and `track`, and the
 * metadata subset of {@link CaptureExceptionOptions}. Empty maps are omitted on
 * the wire per the emit convention.
 */
export interface MetadataOptions {
  tags?: Record<string, string>;
  contexts?: Record<string, unknown>;
  extra?: Record<string, unknown>;
}

/** Extra attribution for `captureException`. */
export interface CaptureExceptionOptions extends MetadataOptions {
  user?: Partial<ErrorUser> | null;
  level?: Level;
  handled?: boolean;
  /** Client-supplied fingerprint override (honored verbatim by the backend). */
  fingerprint?: string[] | null;
}
