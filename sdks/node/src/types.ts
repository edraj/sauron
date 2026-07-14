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

/** An error item (manual `captureException` / `captureMessage`). */
export interface ErrorItem {
  type: 'error';
  event_id: string;
  level: Level;
  timestamp: string;
  exception: ExceptionValue;
  message: string | null;
  breadcrumbs: unknown[];
  tags: Record<string, string>;
  fingerprint: string[] | null;
  user: ErrorUser | null;
  session_id: string | null;
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
  screen: string | null;
}

/** An identity association (PostHog-style `identify`). */
export interface IdentifyItem {
  type: 'identify';
  distinct_id: string;
  anonymous_id: string | null;
  traits: Record<string, unknown>;
  timestamp: string;
}

/** Any item that can appear in an envelope's `items` array. */
export type EnvelopeItem = ErrorItem | EventItem | IdentifyItem;

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

/** A subset of the DOM `fetch` used by the transport. Injectable for tests. */
export type FetchLike = (
  url: string,
  init: {
    method: string;
    headers: Record<string, string>;
    body: string;
  },
) => Promise<{ status: number; ok?: boolean }>;

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
  environment?: string;
  release?: string | null;
  /** Error sample rate in [0, 1]. Default 1 (send everything). */
  sampleRate?: number;
  /** How often the pending batch is flushed, in ms. Default 5000. */
  flushInterval?: number;
  /** Max items per envelope before an eager flush. Default 30. */
  maxBatch?: number;
  /** Injected HTTP sender (for tests). Defaults to global `fetch`. */
  fetchImpl?: FetchLike;
  debug?: boolean;
}

/** Fully-resolved options with all defaults applied. */
export interface ResolvedOptions {
  dsn: string;
  environment: string;
  release: string | null;
  sampleRate: number;
  flushInterval: number;
  maxBatch: number;
  fetchImpl?: FetchLike;
  debug: boolean;
}

/** Extra attribution for `captureException`. */
export interface CaptureExceptionOptions {
  user?: Partial<ErrorUser> | null;
  level?: Level;
  tags?: Record<string, string>;
  handled?: boolean;
}
