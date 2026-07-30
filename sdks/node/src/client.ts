import os from 'node:os';
import { randomUUID } from 'node:crypto';

import { parseDsn } from './dsn.js';
import { Transport } from './transport.js';
import { parseError } from './stacktrace.js';
import { installAutoCapture, installShutdownHooks } from './autocapture.js';
import {
  getCurrentScope,
  getGlobalScope,
  normalizeBreadcrumb,
} from './scope.js';
import { normalizeReason, normalizeWorkflowName } from './workflow.js';
import type {
  ActiveWorkflow,
  BreadcrumbInput,
  CaptureExceptionOptions,
  Context,
  EnvelopeItem,
  ErrorItem,
  ErrorUser,
  EventItem,
  IdentifyItem,
  InitOptions,
  Level,
  MetadataOptions,
  ResolvedOptions,
  TransactionInput,
  TransactionItem,
  WorkflowResult,
} from './types.js';

const DEFAULTS = {
  release: null as string | null,
  sampleRate: 1,
  flushInterval: 5000,
  maxBatch: 30,
  maxBreadcrumbs: 100,
  gzipThresholdBytes: 1024,
  maxQueueBytes: 1_048_576,
  maxRetries: 3,
  debug: false,
};

function resolveOptions(options: InitOptions): ResolvedOptions {
  if (!options || typeof options.dsn !== 'string') {
    throw new Error('[sauron] init requires a { dsn } option');
  }
  const sampleRate =
    typeof options.sampleRate === 'number' ? options.sampleRate : DEFAULTS.sampleRate;
  return {
    dsn: options.dsn,
    release: options.release ?? DEFAULTS.release,
    tags: options.tags ?? {},
    contexts: options.contexts ?? {},
    extra: options.extra ?? {},
    sampleRate: Math.min(1, Math.max(0, sampleRate)),
    flushInterval:
      typeof options.flushInterval === 'number'
        ? options.flushInterval
        : DEFAULTS.flushInterval,
    maxBatch: typeof options.maxBatch === 'number' ? options.maxBatch : DEFAULTS.maxBatch,
    maxBreadcrumbs:
      typeof options.maxBreadcrumbs === 'number'
        ? options.maxBreadcrumbs
        : DEFAULTS.maxBreadcrumbs,
    gzipThresholdBytes:
      typeof options.gzipThresholdBytes === 'number'
        ? options.gzipThresholdBytes
        : DEFAULTS.gzipThresholdBytes,
    maxQueueBytes:
      typeof options.maxQueueBytes === 'number'
        ? options.maxQueueBytes
        : DEFAULTS.maxQueueBytes,
    offlineDir: options.offlineDir ?? null,
    maxRetries:
      typeof options.maxRetries === 'number' ? options.maxRetries : DEFAULTS.maxRetries,
    autoCaptureUnhandled: options.autoCaptureUnhandled ?? false,
    autoShutdown: options.autoShutdown ?? false,
    beforeSend: options.beforeSend,
    beforeBreadcrumb: options.beforeBreadcrumb,
    fetchImpl: options.fetchImpl,
    debug: options.debug ?? DEFAULTS.debug,
  };
}

/** Minimal server-side context assembled once at init. */
function buildContext(): Context {
  return {
    device: { device_id: randomUUID() },
    os: { name: process.platform || null, version: os.release() || null },
    app: {},
    runtime: { name: 'node', version: process.versions.node ?? null },
    user: null,
  };
}

function isoNow(): string {
  return new Date().toISOString();
}

function normalizeUser(user: Partial<ErrorUser> | null | undefined): ErrorUser | null {
  if (!user) return null;
  return {
    id: user.id ?? null,
    email: user.email ?? null,
    username: user.username ?? null,
  };
}

/**
 * The Sauron server-side client. Buffers events/errors and dispatches them via
 * a background transport. Constructed by {@link init}.
 */
export class SauronClient {
  private readonly options: ResolvedOptions;
  private readonly transport: Transport;
  /** Uninstallers for any opt-in process-level hooks, torn down on {@link close}. */
  private readonly hookUninstallers: Array<() => void> = [];

  constructor(options: InitOptions) {
    this.options = resolveOptions(options);
    const dsn = parseDsn(this.options.dsn);
    const globalScope = getGlobalScope();
    globalScope.setMaxBreadcrumbs(this.options.maxBreadcrumbs);
    globalScope.setTags(this.options.tags);
    for (const [name, block] of Object.entries(this.options.contexts)) {
      globalScope.setContext(name, block);
    }
    for (const [key, value] of Object.entries(this.options.extra)) {
      globalScope.setExtra(key, value);
    }
    this.transport = new Transport({
      dsn,
      release: this.options.release,
      context: buildContext(),
      flushInterval: this.options.flushInterval,
      maxBatch: this.options.maxBatch,
      gzipThresholdBytes: this.options.gzipThresholdBytes,
      maxQueueBytes: this.options.maxQueueBytes,
      offlineDir: this.options.offlineDir,
      maxRetries: this.options.maxRetries,
      fetchImpl: this.options.fetchImpl,
      debug: this.options.debug,
    });
    if (this.options.autoCaptureUnhandled) {
      this.hookUninstallers.push(installAutoCapture(this));
    }
    if (this.options.autoShutdown) {
      this.hookUninstallers.push(installShutdownHooks(this));
    }
  }

  /**
   * The single enqueue chokepoint — every error/event/transaction (however
   * constructed, including `captureMessage`'s inline-built item) passes
   * through here before reaching the transport. Stamping the active
   * workflow ONCE, right here, means a future capture path can't forget it
   * the way a per-construction-site stamp could. `identify` items are
   * excluded: the server has no workflow columns for them.
   *
   * Runs `beforeSend` on every item; a `null` return drops it, a returned
   * item replaces it, then it is handed to the transport.
   *
   * `beforeSend` is user-supplied, so it is guarded: a throwing hook must
   * never propagate into the caller's `track`/`captureException`/etc — that
   * would break the SDK's no-throw guarantee. On throw, the item is treated
   * as UNMODIFIED and still sent (never silently dropped); the failure is
   * only surfaced via the debug logger.
   */
  private dispatch(item: EnvelopeItem): void {
    if (item.type !== 'identify') {
      const workflow = getCurrentScope().data.workflow;
      if (workflow) {
        item.workflow_id = workflow.workflowId;
        item.workflow_name = workflow.name;
      }
    }
    const beforeSend = this.options.beforeSend;
    if (beforeSend) {
      let result: EnvelopeItem | null = item;
      try {
        result = beforeSend(item);
      } catch (err) {
        this.debugLog('beforeSend threw', err);
        result = item;
      }
      if (result == null) {
        this.debugLog('dropped by beforeSend');
        return;
      }
      this.transport.enqueue(result);
      return;
    }
    this.transport.enqueue(item);
  }

  /**
   * Add a breadcrumb to the active scope. Runs `beforeBreadcrumb` first; a
   * `null` return drops the crumb.
   */
  addBreadcrumb(crumb: BreadcrumbInput): void {
    const stamped = normalizeBreadcrumb(crumb);
    const beforeBreadcrumb = this.options.beforeBreadcrumb;
    if (beforeBreadcrumb) {
      const result = beforeBreadcrumb(stamped);
      if (result == null) return;
      getCurrentScope().addBreadcrumb(result);
      return;
    }
    getCurrentScope().addBreadcrumb(stamped);
  }

  /** Emit a performance transaction item. */
  trackTransaction(input: TransactionInput): void {
    if (typeof input?.name !== 'string' || input.name.length === 0) return;
    const distinctId = input.distinct_id ?? getCurrentScope().data.user?.id ?? undefined;
    const item: TransactionItem = {
      type: 'transaction',
      name: input.name,
      op: input.op ?? 'custom',
      duration_ms: input.duration_ms,
      timestamp: isoNow(),
    };
    if (input.status !== undefined) item.status = input.status;
    if (input.http_method !== undefined) item.http_method = input.http_method;
    if (input.http_status !== undefined) item.http_status = input.http_status;
    if (input.url !== undefined) item.url = input.url;
    if (distinctId != null) item.distinct_id = distinctId;
    this.dispatch(item);
  }

  /** Capture a product-analytics event. `distinctId` is required. */
  track(
    event: string,
    distinctId: string,
    properties?: Record<string, unknown>,
    options: MetadataOptions = {},
  ): void {
    if (typeof event !== 'string' || event.length === 0) return;
    // An empty/absent distinct id drops a MANUAL track call: the caller should
    // know who acted. The reserved workflow lifecycle events deliberately
    // bypass this via emitEvent() — see workflowDistinctId().
    if (typeof distinctId !== 'string' || distinctId.length === 0) return;
    this.emitEvent(event, distinctId, properties, options);
  }

  /**
   * Build and dispatch an event item. Shared by the public {@link track} (which
   * validates first) and the reserved workflow lifecycle emits (which must be
   * able to send an intentionally empty `distinct_id`).
   */
  private emitEvent(
    event: string,
    distinctId: string,
    properties?: Record<string, unknown>,
    options: MetadataOptions = {},
  ): void {
    const item: EventItem = {
      type: 'event',
      name: event,
      distinct_id: distinctId,
      properties: properties ?? {},
      timestamp: isoNow(),
      session_id: null,
      screen: null,
      ...getCurrentScope().mergeMetadata(options),
    };
    this.dispatch(item);
  }

  /** Capture a native `Error` (or error-like value) as an error item. */
  captureException(error: unknown, options: CaptureExceptionOptions = {}): void {
    if (this.options.sampleRate < 1 && Math.random() >= this.options.sampleRate) {
      return;
    }
    const { type, value } = describeError(error);
    const item: ErrorItem = {
      type: 'error',
      event_id: randomUUID(),
      level: options.level ?? 'error',
      timestamp: isoNow(),
      exception: {
        type,
        value,
        mechanism: { type: 'generic', handled: options.handled ?? true },
        stacktrace: parseError(error),
      },
      message: null,
      breadcrumbs: [],
      tags: options.tags ?? {},
      contexts: options.contexts ?? {},
      extra: options.extra ?? {},
      fingerprint: options.fingerprint ?? null,
      user: normalizeUser(options.user),
      session_id: null,
      screen: null,
    };
    getCurrentScope().applyToErrorItem(item);
    this.dispatch(item);
  }

  /** Capture a bare message as an error item (no exception payload). */
  captureMessage(message: string, level: Level = 'info', options: MetadataOptions = {}): void {
    const item: ErrorItem = {
      type: 'error',
      event_id: randomUUID(),
      level,
      timestamp: isoNow(),
      exception: {
        type: 'Message',
        value: message,
        mechanism: { type: 'generic', handled: true },
        stacktrace: [],
      },
      message,
      breadcrumbs: [],
      tags: options.tags ?? {},
      contexts: options.contexts ?? {},
      extra: options.extra ?? {},
      fingerprint: null,
      user: null,
      session_id: null,
      screen: null,
    };
    getCurrentScope().applyToErrorItem(item);
    this.dispatch(item);
  }

  /** Associate traits with a distinct id. */
  identify(distinctId: string, traits?: Record<string, unknown>): void {
    if (typeof distinctId !== 'string' || distinctId.length === 0) return;
    const item: IdentifyItem = {
      type: 'identify',
      distinct_id: distinctId,
      anonymous_id: null,
      traits: traits ?? {},
      timestamp: isoNow(),
    };
    this.dispatch(item);
  }

  /**
   * Whether this client can still deliver telemetry — false once the
   * transport has auto-disabled itself on a 401/403. Gated on this (not just
   * "does a client object exist") so `startWorkflow` can't mutate local scope
   * state and emit an event the transport would silently drop underneath it.
   */
  isEnabled(): boolean {
    return this.transport.isEnabled();
  }

  /** Debug-gated warning log, matching the transport's own `[sauron]`-prefixed convention. */
  private debugLog(message: string, ...args: unknown[]): void {
    if (this.options.debug) {
      // eslint-disable-next-line no-console
      console.warn(`[sauron] ${message}`, ...args);
    }
  }

  /**
   * Emit the closing lifecycle event (`$workflow_end`/`$workflow_cancel`) for
   * `active` while it is STILL the current scope's workflow (so the item-level
   * `workflow_id`/`workflow_name` stamped by `dispatch()` are its own, not
   * `null`/absent), through `track()` so scope tags/contexts/extra apply.
   * Never mutates scope state itself — callers own clearing it, in a `finally`
   * relative to this call, so a throw here can't leave state half-mutated.
   *
   * The `distinctId` comes from {@link SauronClient.workflowDistinctId} — read
   * its note on the `'system'` fallback's effect on unique-user counts.
   */
  private emitWorkflowClose(
    active: ActiveWorkflow,
    eventName: '$workflow_end' | '$workflow_cancel',
    reason?: string,
  ): void {
    const properties: Record<string, unknown> = {
      workflow_id: active.workflowId,
      workflow_name: active.name,
      duration_ms: Math.max(0, Date.now() - Date.parse(active.startedAt)),
    };
    if (eventName === '$workflow_cancel') {
      properties.reason = normalizeReason(reason);
    }
    this.emitEvent(eventName, this.workflowDistinctId(), properties);
  }

  /**
   * The `distinct_id` to attribute a workflow lifecycle event to: the scope's
   * user id, else **the empty string**.
   *
   * Empty is deliberate and correct — do NOT "fix" this to a sentinel like
   * `'system'`, an anonymous/device id, or anything derived from the workflow
   * id. The server was built for exactly this case:
   *
   *   - `backend/crates/sauron-pipeline/src/process.rs` — both `bump_workflow`
   *     call sites pass `Some(distinct_id.as_str()).filter(|s| !s.is_empty())`,
   *     so an empty id is stored as SQL `NULL` on the `workflows` row.
   *   - `backend/crates/sauron-db/src/repo.rs` — the per-workflow rollup
   *     computes `COUNT(DISTINCT w.distinct_id) AS unique_users`, and
   *     `COUNT(DISTINCT ...)` skips NULLs.
   *
   * So an anonymous workflow run contributes *nothing* to `unique_users`,
   * which is honest. Any non-empty sentinel would instead collapse every
   * anonymous run of a workflow (`password_reset`, `guest_checkout`, …) into
   * one fake bucket, silently reporting ~1 unique user no matter how many
   * distinct invocations occurred.
   *
   * `EventItem.distinct_id` is a required string on the wire
   * (`backend/crates/sauron-core/src/envelope.rs`), so the field is still
   * always sent — it is just `""`. This is why the lifecycle emits route
   * through {@link emitEvent} rather than the public {@link track}, whose
   * empty-`distinctId` guard would otherwise drop them entirely.
   */
  private workflowDistinctId(): string {
    return getCurrentScope().data.user?.id ?? '';
  }

  /**
   * Start a named workflow on the current scope (the `AsyncLocalStorage`
   * child inside `withScope`/`runWithAsyncScope`, else the process-wide
   * global scope) — request-isolated, so concurrent requests never observe
   * or clobber each other's workflow. `force: true` supersedes an
   * already-active workflow (emitting `$workflow_cancel` with
   * `reason: 'superseded'` for it first); otherwise an active workflow makes
   * this a no-op returning `already_active`.
   *
   * The workflow id is a fresh `randomUUID()`, minted here — never derived
   * from anything deterministic. The server's rollup key is
   * `(app_id, workflow_id)` app-wide, so a reused/derived id would merge
   * counters from unrelated requests/environments into one row.
   *
   * Never throws: an unexpected failure before any side effect is reported as
   * `disabled`, and `disabled` always means literally nothing happened — no
   * event on the wire, no state change. A failure emitting `$workflow_start`
   * AFTER the scope's workflow field was set is still reported as `ok` — the
   * workflow IS live locally, and a lost start event is recoverable
   * server-side (the row materializes from the next stamped item via the same
   * upsert); a lost local id would not be.
   */
  startWorkflow(name: string, options?: { force?: boolean }): WorkflowResult {
    try {
      if (!this.isEnabled()) return { status: 'disabled' };
      const normalized = normalizeWorkflowName(name);
      if (!normalized) {
        this.debugLog('startWorkflow: invalid name', name);
        return { status: 'invalid_name' };
      }

      const scope = getCurrentScope();
      const active = scope.data.workflow;
      if (active && !options?.force) {
        this.debugLog(
          `startWorkflow("${normalized}"): "${active.name}" is already active; pass { force: true } to replace it`,
        );
        return { status: 'already_active' };
      }

      // Mint the replacement BEFORE superseding anything. If `randomUUID()` or
      // `isoNow()` throws, the outer catch returns `disabled` — which promises
      // the caller that nothing happened and their old workflow is still
      // running. Minting after the supersede emit would break that promise:
      // `$workflow_cancel` for the old workflow would already be on the wire
      // while `scope.data.workflow` still held it, so the caller's eventual
      // `endWorkflow()` would emit a SECOND terminal lifecycle event for a
      // workflow row the server already recorded as cancelled.
      const workflow: ActiveWorkflow = {
        workflowId: randomUUID(),
        name: normalized,
        startedAt: isoNow(),
      };

      if (active) {
        // force: supersede the old workflow. Emitted while it is still
        // `scope.data.workflow`, so `dispatch()` stamps the cancel with it.
        try {
          this.emitWorkflowClose(active, '$workflow_cancel', 'superseded');
        } catch (emitErr) {
          this.debugLog('startWorkflow: superseding $workflow_cancel emit threw', emitErr);
        }
      }

      // Set state BEFORE emitting so $workflow_start is itself stamped with it.
      scope.data.workflow = workflow;
      try {
        this.emitEvent('$workflow_start', this.workflowDistinctId(), {
          workflow_id: workflow.workflowId,
          workflow_name: workflow.name,
        });
      } catch (emitErr) {
        this.debugLog('startWorkflow: $workflow_start emit threw (workflow stays active)', emitErr);
      }
      return { status: 'ok', workflowId: workflow.workflowId };
    } catch (err) {
      this.debugLog('startWorkflow threw', err);
      return { status: 'disabled' };
    }
  }

  /** Shared precondition + close logic for `endWorkflow`/`cancelWorkflow`. */
  private closeWorkflow(
    eventName: '$workflow_end' | '$workflow_cancel',
    name?: string,
    reason?: string,
  ): WorkflowResult {
    try {
      if (!this.isEnabled()) return { status: 'disabled' };
      const scope = getCurrentScope();
      const active = scope.data.workflow;
      if (!active) return { status: 'not_active' };
      if (name !== undefined && normalizeWorkflowName(name) !== active.name) {
        this.debugLog(`${eventName}: "${name}" does not match active workflow "${active.name}"`);
        return { status: 'name_mismatch' };
      }
      const workflowId = active.workflowId;
      try {
        this.emitWorkflowClose(active, eventName, reason);
      } catch (emitErr) {
        this.debugLog(`${eventName} emit threw`, emitErr);
      } finally {
        // Clear AFTER emitting (so the closing event still carries the
        // workflow it closes) but UNCONDITIONALLY — even if the emit above
        // threw, endWorkflow/cancelWorkflow must still return `ok` below
        // rather than leaving state half-mutated.
        scope.data.workflow = null;
      }
      return { status: 'ok', workflowId };
    } catch (err) {
      this.debugLog(`${eventName} threw`, err);
      return { status: 'disabled' };
    }
  }

  /**
   * End the active workflow (or the one named `name`, if given). Emits
   * `$workflow_end` with `duration_ms` and clears the scope's workflow field.
   * A no-op returning `not_active` (nothing active) or `name_mismatch` (`name`
   * given but does not match, including a `name` that fails normalization).
   */
  endWorkflow(name?: string): WorkflowResult {
    return this.closeWorkflow('$workflow_end', name);
  }

  /**
   * Cancel the active workflow (or the one named `name`, if given). Emits
   * `$workflow_cancel` with `duration_ms` and `reason` (default `'user'`,
   * trimmed and capped at 120 chars) and clears the scope's workflow field.
   */
  cancelWorkflow(name?: string, options?: { reason?: string }): WorkflowResult {
    return this.closeWorkflow('$workflow_cancel', name, options?.reason);
  }

  /** Send any buffered items immediately. */
  flush(): Promise<void> {
    return this.transport.flush();
  }

  /** Flush then stop the background timer, and remove any opt-in process hooks. */
  close(): Promise<void> {
    for (const uninstall of this.hookUninstallers.splice(0)) uninstall();
    // Clear (never auto-cancel — an abandoned workflow is a legitimate,
    // server-derived 30-minute outcome) any workflow left on the shared
    // global scope, so a later init() sharing the same process-wide scope
    // doesn't inherit a stale one.
    getGlobalScope().data.workflow = null;
    return this.transport.close();
  }
}

/** Derive `{type, value}` from an arbitrary thrown value. */
export function describeError(error: unknown): { type: string; value: string | null } {
  if (error instanceof Error) {
    return { type: error.name || 'Error', value: error.message || null };
  }
  if (typeof error === 'string') {
    return { type: 'Error', value: error };
  }
  if (error && typeof error === 'object') {
    const name = (error as { name?: unknown }).name;
    const message = (error as { message?: unknown }).message;
    return {
      type: typeof name === 'string' && name ? name : 'Error',
      value: typeof message === 'string' ? message : null,
    };
  }
  return { type: 'Error', value: error === undefined ? null : String(error) };
}
