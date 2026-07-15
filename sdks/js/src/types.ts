/**
 * Sauron wire-contract types.
 *
 * These interfaces mirror the LOCKED envelope shape that the Rust ingest
 * gateway and the Flutter SDK also emit/consume. Field names, nullability and
 * ordering are load-bearing — do not "clean them up".
 */

/** Severity level. Matches the backend enum exactly. */
export type Level = 'debug' | 'info' | 'warning' | 'error' | 'fatal';

/** Discriminant for an envelope item. */
export type ItemType = 'error' | 'event' | 'identify' | 'breadcrumb_batch' | 'transaction';

/** Category of a performance transaction. Matches the backend enum exactly. */
export type TransactionOp = 'navigation' | 'http' | 'resource' | 'screen_load' | 'custom';

/** A single normalized stack frame (raw, never symbolicated client-side). */
export interface Frame {
  function: string | null;
  filename: string | null;
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
  type: string | null;
  value: string | null;
  mechanism: Mechanism;
  stacktrace: Frame[];
}

/** A breadcrumb — a short trail-of-events entry. `data`/`message` may be null. */
export interface Breadcrumb {
  type: string;
  category: string;
  message: string | null;
  level: Level;
  timestamp: string;
  data: Record<string, unknown> | null;
}

/** An error item (uncaught error, rejection, or manual capture). */
export interface ErrorItem {
  type: 'error';
  /**
   * Stable id the SDK mints for this report so callers can correlate it. Wire
   * field `event_id`. Optional — the backend defaults one when omitted.
   */
  event_id?: string;
  timestamp: string;
  level: Level;
  exception: ExceptionValue;
  /** Optional human-readable summary alongside the exception. */
  message?: string;
  breadcrumbs: Breadcrumb[];
  fingerprint: string[] | null;
  /**
   * Free-form indexed tags lifted from the current scope. Optional — omitted
   * when the scope carries none (the backend defaults to `{}`).
   */
  tags?: Record<string, unknown>;
  /**
   * Per-item user override (falls back to the envelope-context user). Optional
   * — omitted when no identity is set on the scope.
   */
  user?: UserContext | null;
  session_id?: string | null;
  screen?: string | null;
}

/** A product-analytics event (PostHog-style `track`). */
export interface EventItem {
  type: 'event';
  name: string;
  distinct_id: string | null;
  session_id?: string | null;
  screen?: string | null;
  timestamp: string;
  properties: Record<string, unknown>;
}

/**
 * A performance transaction (navigation timing, an instrumented `fetch`, a
 * screen load, ...). `duration_ms` is the wall-clock span; the `http_*`/`url`
 * fields carry request metadata for `http` ops and are `null` otherwise.
 */
export interface TransactionItem {
  type: 'transaction';
  name: string;
  op: TransactionOp;
  duration_ms: number;
  status?: string | null;
  http_method?: string | null;
  http_status?: number | null;
  url?: string | null;
  distinct_id?: string | null;
  session_id?: string | null;
  timestamp: string;
}

/** An identity association (PostHog-style `identify`). */
export interface IdentifyItem {
  type: 'identify';
  distinct_id: string | null;
  anonymous_id: string | null;
  traits: Record<string, unknown>;
}

/** A standalone batch of breadcrumbs (used for periodic session trails). */
export interface BreadcrumbBatchItem {
  type: 'breadcrumb_batch';
  breadcrumbs: Breadcrumb[];
}

/** Any item that can appear in an envelope's `items` array. */
export type EnvelopeItem =
  | ErrorItem
  | EventItem
  | IdentifyItem
  | BreadcrumbBatchItem
  | TransactionItem;

/* ------------------------------------------------------------------ context */

export interface DeviceContext {
  /** Durable, persisted device identity (localStorage `sauron.device_id`). */
  device_id: string;
  family: string | null;
  model: string | null;
  arch: string | null;
}

export interface OsContext {
  name: string | null;
  version: string | null;
}

export interface AppContext {
  version: string | null;
  build: string | null;
}

export interface RuntimeContext {
  name: string | null;
  version: string | null;
}

export interface UserContext {
  id: string | null;
  email: string | null;
  traits: Record<string, unknown>;
}

export interface Context {
  device: DeviceContext;
  os: OsContext;
  app: AppContext;
  runtime: RuntimeContext;
  user: UserContext;
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
  environment: string;
  release: string | null;
}

/** The complete, serializable envelope posted to the ingest gateway. */
export interface Envelope {
  header: EnvelopeHeader;
  context: Context;
  items: EnvelopeItem[];
}

/* -------------------------------------------------------------------- input */

/** Loose hint bag passed through to `beforeSend` / `beforeBreadcrumb`. */
export type Hint = Record<string, unknown> & {
  originalException?: unknown;
  event?: unknown;
};

/** Value accepted by `setUser` — normalized into a `UserContext`. */
export type UserInput =
  | (Partial<UserContext> & { id?: string | null; email?: string | null })
  | null;

export type BeforeSend = (item: EnvelopeItem, hint?: Hint) => EnvelopeItem | null;
export type BeforeBreadcrumb = (
  breadcrumb: Breadcrumb,
  hint?: Hint,
) => Breadcrumb | null;

/** Transport tuning knobs. */
export interface TransportOptions {
  /** How often the pending batch is flushed, in ms. Default 5000. */
  flushIntervalMs?: number;
  /** Max items per envelope before an eager flush. Default 30. */
  maxBatch?: number;
  /** Cap on the offline localStorage queue, in bytes. Default 1 MiB. */
  maxQueueBytes?: number;
}

/** Options accepted by `Sauron.init`. */
export interface InitOptions {
  /** `https://<public_key>@<host>/<project_id>` */
  dsn: string;
  environment?: string;
  release?: string;
  /** Error sample rate in [0, 1]. Default 1 (send everything). */
  sampleRate?: number;
  /** Ring-buffer size for breadcrumbs. Default 50. */
  maxBreadcrumbs?: number;
  beforeSend?: BeforeSend;
  beforeBreadcrumb?: BeforeBreadcrumb;
  transport?: TransportOptions;
  /**
   * Auto-capture performance transactions (navigation, fetch, SPA routes) by
   * patching `fetch`/History. Opt-in — default `false`. Manual
   * `trackTransaction()` works regardless.
   */
  performance?: boolean;
  /** Seed the initial screen name. */
  screen?: string;
  /**
   * Auto-track the current screen from History navigations (reuses the SPA
   * route hook). Opt-in — default `false`. `setScreen()` works regardless.
   */
  screenTracking?: boolean;
  debug?: boolean;
}

/** Fully-resolved options with all defaults applied. */
export interface ResolvedOptions {
  dsn: string;
  environment: string;
  release: string | null;
  sampleRate: number;
  maxBreadcrumbs: number;
  beforeSend?: BeforeSend;
  beforeBreadcrumb?: BeforeBreadcrumb;
  transport: Required<TransportOptions>;
  performance: boolean;
  screen?: string;
  screenTracking: boolean;
  debug: boolean;
}
