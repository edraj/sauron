import 'dart:io' show Directory;
import 'dart:isolate';
import 'dart:math';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

import 'context/anonymous_id_store.dart';
import 'context/device_context.dart';
import 'dsn.dart';
import 'envelope.dart';
import 'integrations/flutter_error_integration.dart';
import 'integrations/isolate_error_integration.dart';
import 'integrations/platform_dispatcher_integration.dart';
import 'integrations/widgets_binding_observer.dart';
import 'sauron_options.dart';
import 'scope.dart';
import 'stacktrace/dart_stacktrace_parser.dart';
import 'transaction.dart';
import 'transport/queue.dart';
import 'transport/transport.dart';
import 'types.dart';
import 'util/uuid.dart';
import 'workflow.dart';

/// The engine behind the [Sauron] facade: owns the scope, sampling, the
/// `beforeSend` hook, context, and the transport.
class SauronClient {
  SauronClient(this.options)
      : _scope = Scope(maxBreadcrumbs: options.maxBreadcrumbs),
        sessionId = generateUuidV4() {
    _currentScreen = options.screen;
    _scope.setTags(options.tags);
    _scope.contexts.addAll(options.contexts);
    _scope.extra.addAll(options.extra);
    if (options.isConfigured) {
      try {
        _dsn = Dsn.parse(options.dsn!);
      } on FormatException catch (error) {
        _dsn = null;
        _log('invalid DSN, SDK disabled: ${error.message}');
      }
    }
  }

  final SauronOptions options;

  /// The id of the session created when this client was constructed (at init).
  /// Attached to errors, analytics events, and transactions so the backend can
  /// tie signals onto a single session timeline.
  final String sessionId;

  /// The current screen/route name, stamped on every event and error, or null.
  String? _currentScreen;

  /// The current screen name, or null if none set.
  String? get screen => _currentScreen;

  /// The currently active workflow, stamped onto every item constructed while
  /// it is set. One workflow at a time — scoped to this client (not a module
  /// global), matching every other piece of per-client state here.
  ///
  /// **Stamping is LEAF-SITE by deliberate choice, and that is a maintenance
  /// hazard worth knowing about.** `workflow_id`/`workflow_name` are written
  /// inline at each of the three item-construction sites below
  /// ([captureException], [track], [trackTransaction]) rather than at a single
  /// choke point. [_dispatch] is the only door to the transport and would be
  /// the natural choke point, but by the time an item reaches it the item is
  /// already constructed and every field is `final` — stamping there would
  /// require a `copyWith` on all item classes purely for this.
  ///
  /// The cost of that choice: **a future capture path that builds its own item
  /// will silently ship unstamped, and no test will fail.** If you add one,
  /// stamp it here too — or take the `copyWith` redesign and move stamping
  /// into [_dispatch]. [identify] is the one construction site deliberately
  /// left unstamped (the server has no workflow columns for identify items).
  ActiveWorkflow? _currentWorkflow;

  /// The active workflow, or null if none.
  ActiveWorkflow? get workflow => _currentWorkflow;

  final Scope _scope;
  final DeviceContextProvider _deviceContext = DeviceContextProvider();
  final AnonymousIdStore _anonymousIdStore = const AnonymousIdStore();
  final DartStackTraceParser _parser = const DartStackTraceParser();
  final Random _random = Random();
  final List<EnvelopeItem> _pending = <EnvelopeItem>[];

  Dsn? _dsn;
  SauronTransport? _transport;
  bool _closed = false;

  /// The SDK storage directory, once [bootstrap] has resolved it. Held so
  /// [reset] can persist a fresh anonymous id without resolving it again.
  Directory? _storageDirectory;

  /// The persisted anonymous id, resolved during [bootstrap] and null until
  /// then — see [_analyticsDistinctId].
  String? _anonymousId;

  /// Whether the anonymous id has actually been USED as a `distinct_id`.
  ///
  /// A persisted id that was never observed anonymously must not create an
  /// alias row: the server's `process_identify` inserts a permanent
  /// `identities(app_id, alias_id, distinct_id)` row for any non-empty
  /// `anonymous_id`, so an [identify] on a first-ever launch — with no
  /// anonymous history to link — would durably mis-merge two people.
  bool _anonymousIdUsed = false;

  /// The persisted anonymous id this install reports as `distinct_id` until
  /// [identify] names a user. Null before [bootstrap] has run.
  String? get anonymousId => _anonymousId;

  /// Whether the SDK is configured and actively able to deliver.
  ///
  /// False when the DSN never parsed, once [close] has run (a closed client is
  /// terminal), **and** once the transport has disabled itself after the
  /// gateway rejected the key with a 401/403. That last case matters: the
  /// transport silently drops everything from then on, so a client that still
  /// reported `true` would let callers — and `startWorkflow` in particular —
  /// believe state they set locally had reached the server. `_transport` is
  /// null until [bootstrap] runs, which is not a disabled state.
  bool get isEnabled =>
      _dsn != null && !_closed && (_transport?.isEnabled ?? true);

  // ---- lifecycle -------------------------------------------------------------

  /// Installs the four uncaught-error capture layers plus the lifecycle
  /// observer. Must run after `WidgetsFlutterBinding.ensureInitialized()`.
  void installIntegrations() {
    if (!isEnabled) {
      return;
    }
    FlutterErrorIntegration.install(this);
    PlatformDispatcherIntegration.install(this);
    if (!kIsWeb) {
      IsolateErrorIntegration.install(this);
    }
    SauronWidgetsBindingObserver.install(this);
  }

  /// Resolves the offline queue directory, loads device context, and starts the
  /// transport (which drains any envelopes persisted by a previous session).
  ///
  /// Pass [queueDirectory] to override storage location (used by tests).
  Future<void> bootstrap({Directory? queueDirectory}) async {
    if (!isEnabled || _transport != null) {
      return;
    }
    final Directory dir = queueDirectory ?? await _resolveQueueDirectory();
    _storageDirectory = dir;
    // Resolved before the transport exists, so nothing can be captured with a
    // half-known identity. Awaited separately from the device id below rather
    // than in parallel: both keys share one prefs file, and two overlapping
    // read-modify-writes can lose one another (see `PrefsStore`).
    _anonymousId = await _anonymousIdStore.resolve(dir);
    final EnvelopeQueue queue = EnvelopeQueue(
      directory: dir,
      maxBytes: options.maxQueueBytes,
    );
    final SauronTransport transport = SauronTransport(
      options: options,
      dsn: _dsn!,
      headerBuilder: _buildHeader,
      contextBuilder: _buildContext,
      queue: queue,
      httpClient: options.httpClient,
    );
    _transport = transport;
    await _deviceContext.load(storageDirectory: dir, app: _appDescriptor());
    transport.start();
    // Replay anything captured before the transport was ready.
    for (final EnvelopeItem item in _pending) {
      transport.capture(item);
    }
    _pending.clear();
  }

  /// The developer-supplied app descriptor, or null when neither
  /// `appVersion` nor `appBuild` was set (the `app` block is then omitted).
  AppDescriptor? _appDescriptor() {
    if (options.appVersion == null && options.appBuild == null) {
      return null;
    }
    return AppDescriptor(
      version: options.appVersion,
      build: options.appBuild,
    );
  }

  Future<Directory> _resolveQueueDirectory() async {
    final Directory base = await getApplicationSupportDirectory();
    final Directory dir = Directory('${base.path}/sauron');
    if (!await dir.exists()) {
      await dir.create(recursive: true);
    }
    return dir;
  }

  // ---- capture API -----------------------------------------------------------

  /// Captures an exception, applying sampling and `beforeSend`.
  void captureException(
    Object error, {
    StackTrace? stackTrace,
    Mechanism? mechanism,
    SauronLevel level = SauronLevel.error,
    String? screen,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
  }) {
    if (!isEnabled) {
      return;
    }
    if (_random.nextDouble() >= options.normalizedSampleRate) {
      _log('event dropped by sampleRate.');
      return;
    }
    final StackTrace? stack =
        stackTrace ?? (options.attachStacktrace ? StackTrace.current : null);
    final SauronException exception = SauronException(
      type: error.runtimeType.toString(),
      value: error.toString(),
      mechanism: mechanism ?? const Mechanism(type: 'manual', handled: true),
      stacktrace: _parser.parse(stack),
    );
    // Obfuscated AOT traces are PC offsets the neutral frame model can't carry;
    // ship the verbatim trace + build-id so the server can symbolicate.
    final String? rawTrace = stack?.toString();
    final bool obfuscated = rawTrace != null && isObfuscatedDartTrace(rawTrace);
    final ErrorItem item = ErrorItem(
      exception: exception,
      timestamp: DateTime.now().toUtc(),
      level: level,
      breadcrumbs: _scope.breadcrumbs,
      sessionId: sessionId,
      // Leaf-site workflow stamp 1 of 3 — see the note on [_currentWorkflow].
      workflowId: _currentWorkflow?.workflowId,
      workflowName: _currentWorkflow?.name,
      screen: screen ?? _currentScreen,
      rawStacktrace: obfuscated ? rawTrace : null,
      debugMeta: obfuscated ? DebugMeta.fromTrace(rawTrace) : null,
      tags: _mergeTags(tags),
      contexts: _mergeContexts(contexts),
      extra: _mergeExtra(extra),
    );
    // `beforeSend` is applied uniformly for every item in [_dispatch].
    _dispatch(item);
    // Errors are worth an eager flush attempt.
    _transport?.flush();
  }

  /// The id analytics items are attributed to: the identified user when
  /// [identify] or [setUser] has named one, else the persisted anonymous id.
  ///
  /// Reading this MARKS the anonymous id as used, which is what later makes
  /// [identify] send `anonymous_id` — see [_anonymousIdUsed]. It is null only
  /// before [bootstrap] has resolved storage.
  String? get _analyticsDistinctId {
    final String? identified = _scope.distinctId;
    if (identified != null && identified.isNotEmpty) {
      return identified;
    }
    final String? anonymous = _anonymousId;
    if (anonymous == null) {
      return null;
    }
    _anonymousIdUsed = true;
    return anonymous;
  }

  /// Records a product-analytics event.
  ///
  /// `distinct_id` is the identified user when there is one, otherwise the
  /// persisted [anonymousId] — see [_analyticsDistinctId]. It is only dropped
  /// when neither exists, which after [bootstrap] cannot happen; see
  /// [_dropWithoutIdentity] for why dropping the single item is the lesser
  /// evil. This also covers the events the SDK emits through this method
  /// itself: `$screen` from [setScreen] and the `$workflow_*` lifecycle events.
  void track(
    String name, {
    Map<String, Object?>? properties,
    String? screen,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
  }) {
    if (!isEnabled) {
      return;
    }
    final String? distinctId = _analyticsDistinctId;
    if (distinctId == null || distinctId.isEmpty) {
      _dropWithoutIdentity(name);
      return;
    }
    _dispatch(
      EventItem(
        name: name,
        timestamp: DateTime.now().toUtc(),
        distinctId: distinctId,
        sessionId: sessionId,
        // Leaf-site workflow stamp 2 of 3 — see the note on [_currentWorkflow].
        workflowId: _currentWorkflow?.workflowId,
        workflowName: _currentWorkflow?.name,
        screen: screen ?? _currentScreen,
        properties: properties,
        tags: _mergeTags(tags),
        contexts: _mergeContexts(contexts),
        extra: _mergeExtra(extra),
      ),
    );
  }

  /// Sets the current screen. On an actual change, emits a `$screen` view event
  /// carrying the new screen (so dwell can be computed server-side).
  void setScreen(String name) {
    if (name == _currentScreen) {
      return;
    }
    _currentScreen = name;
    track(r'$screen', properties: <String, Object?>{'screen': name});
  }

  // ---- workflows --------------------------------------------------------------

  /// Starts a named workflow. While active, its id/name are stamped onto every
  /// captured error/event/transaction, and a `$workflow_start` analytics event
  /// is emitted carrying it.
  ///
  /// Returns:
  /// - `disabled` — the client is not enabled (before init, after [close], or
  ///   the transport auto-disabled itself). No state is mutated.
  /// - `invalidName` — [name] is empty after trimming, or over 120 chars.
  /// - `alreadyActive` — another workflow is already active and [force] is
  ///   `false`. No-op other than a debug-log warning.
  /// - `ok` — the workflow started. If [force] is `true` and a workflow was
  ///   already active, it is first closed with a `$workflow_cancel` carrying
  ///   `reason: 'superseded'`.
  WorkflowResult startWorkflow(String name, {bool force = false}) {
    try {
      if (!isEnabled) {
        return const WorkflowResult(WorkflowStatus.disabled);
      }
      final String? normalized = normalizeWorkflowName(name);
      if (normalized == null) {
        _log('startWorkflow: invalid name "$name"');
        return const WorkflowResult(WorkflowStatus.invalidName);
      }

      final ActiveWorkflow? active = _currentWorkflow;
      if (active != null && !force) {
        _log(
          'startWorkflow("$normalized"): "${active.name}" is already '
          'active; pass force: true to replace it',
        );
        return const WorkflowResult(WorkflowStatus.alreadyActive);
      }

      // Mint the replacement BEFORE superseding the old workflow. If the id or
      // timestamp were computed after the `$workflow_cancel` below, a throw in
      // between would leave `_currentWorkflow` pointing at a workflow the
      // server has already been told is cancelled, while the outer catch
      // returned `disabled` claiming nothing happened — the same half-mutated
      // lie item 15 forbids, one statement over. Minting first means every
      // throw site from here on is either before any mutation (outer catch,
      // `disabled` is honest) or after both (guarded, returns `ok`).
      final ActiveWorkflow started = ActiveWorkflow(
        workflowId: generateUuidV4(),
        name: normalized,
        startedAt: DateTime.now().toUtc(),
      );

      if (active != null) {
        // Best-effort: a throwing beforeSend/transport must not stop the new
        // workflow from starting below, and must not surface as `disabled`
        // (nothing about *this* call's own precondition failed).
        try {
          _emitWorkflowClose(active, r'$workflow_cancel', reason: 'superseded');
        } on Object catch (error) {
          _log('startWorkflow: superseding cancel emit threw: $error');
        }
      }

      // Set state BEFORE emitting so $workflow_start is itself stamped with it.
      _currentWorkflow = started;
      try {
        track(
          r'$workflow_start',
          properties: <String, Object?>{
            'workflow_id': started.workflowId,
            'workflow_name': started.name,
          },
        );
      } on Object catch (error) {
        _log('startWorkflow: start emit threw: $error');
        // Fall through to `ok` anyway — the workflow IS live locally, and the
        // server materializes the row from the first stamped event via its
        // own upsert. A lost $workflow_start is recoverable; a lost local id
        // is not (see the class doc on WorkflowStatus.disabled).
      }
      return WorkflowResult(WorkflowStatus.ok, started.workflowId);
    } on Object catch (error) {
      // Reaching here means something threw BEFORE `_currentWorkflow` was
      // set above (both emits have their own catch) — so state was never
      // half-mutated and `disabled` is accurate.
      _log('startWorkflow threw: $error');
      return const WorkflowResult(WorkflowStatus.disabled);
    }
  }

  /// Ends the active workflow (or the one named [name], if given). Emits
  /// `$workflow_end` with `duration_ms` and clears the state.
  ///
  /// Returns `notActive` when no workflow is active, `nameMismatch` when
  /// [name] is given and does not match the active workflow's name (or fails
  /// normalization itself), `disabled` when the client is not enabled, else
  /// `ok`.
  WorkflowResult endWorkflow([String? name]) =>
      _closeWorkflow(r'$workflow_end', name: name);

  /// Cancels the active workflow (or the one named [name], if given). Emits
  /// `$workflow_cancel` with `duration_ms` and `reason` (default `'user'`,
  /// trimmed and capped at 120 chars) and clears the state.
  ///
  /// Same precondition/status semantics as [endWorkflow].
  WorkflowResult cancelWorkflow([String? name, String? reason]) =>
      _closeWorkflow(r'$workflow_cancel', name: name, reason: reason);

  /// Shared precondition + close logic for [endWorkflow]/[cancelWorkflow].
  WorkflowResult _closeWorkflow(
    String eventName, {
    String? name,
    String? reason,
  }) {
    try {
      if (!isEnabled) {
        return const WorkflowResult(WorkflowStatus.disabled);
      }
      final ActiveWorkflow? active = _currentWorkflow;
      if (active == null) {
        return const WorkflowResult(WorkflowStatus.notActive);
      }
      if (name != null && normalizeWorkflowName(name) != active.name) {
        _log(
          '$eventName: "$name" does not match active workflow "${active.name}"',
        );
        return const WorkflowResult(WorkflowStatus.nameMismatch);
      }
      final String workflowId = active.workflowId;
      // The emit is guarded + the clear is in a `finally` relative to it, so
      // this always returns `ok` once we get this far — a throwing
      // beforeSend/transport must not leave the workflow stuck "active"
      // locally forever, nor report `disabled` for what is, from the
      // caller's point of view, a successful close.
      try {
        _emitWorkflowClose(active, eventName, reason: reason);
      } on Object catch (error) {
        _log('$eventName emit threw: $error');
      } finally {
        _currentWorkflow = null;
      }
      return WorkflowResult(WorkflowStatus.ok, workflowId);
    } on Object catch (error) {
      // Reaching here means something threw BEFORE the guarded emit block
      // above (which never rethrows) — so no state was touched.
      _log('$eventName threw: $error');
      return const WorkflowResult(WorkflowStatus.disabled);
    }
  }

  /// Emits the closing lifecycle event for [active] while it is STILL the
  /// active workflow (so `track()`/the capture-site stamping picks it up).
  /// Clearing [_currentWorkflow] is the caller's responsibility.
  void _emitWorkflowClose(
    ActiveWorkflow active,
    String eventName, {
    String? reason,
  }) {
    final int elapsedMs =
        DateTime.now().toUtc().difference(active.startedAt).inMilliseconds;
    final Map<String, Object?> properties = <String, Object?>{
      'workflow_id': active.workflowId,
      'workflow_name': active.name,
      'duration_ms': elapsedMs < 0 ? 0 : elapsedMs,
    };
    if (eventName == r'$workflow_cancel') {
      properties['reason'] = normalizeWorkflowReason(reason);
    }
    track(eventName, properties: properties);
  }

  /// Starts a stateful transaction that computes its own duration.
  /// 
  /// The returned [ActiveTransaction] must be `.end()`ed or `.cancel()`ed 
  /// to record the span. Until then, it is not sent to the server.
  ActiveTransaction startTransaction({
    required String name,
    String op = 'custom',
    String? status,
    String? httpMethod,
    int? httpStatus,
    String? url,
  }) {
    return ActiveTransaction(
      this,
      name: name,
      op: op,
      status: status,
      httpMethod: httpMethod,
      httpStatus: httpStatus,
      url: url,
    );
  }

  /// Records a performance [TransactionItem]: one timed operation
  /// (navigation, HTTP call, resource fetch, screen load, or a custom span).
  ///
  /// [duration] is serialized as fractional milliseconds
  /// (`duration.inMicroseconds / 1000.0`). The current distinct id and session
  /// id are attached automatically.
  void trackTransaction({
    required String name,
    required Duration duration,
    String op = 'custom',
    String? status,
    String? httpMethod,
    int? httpStatus,
    String? url,
  }) {
    if (!isEnabled) {
      return;
    }
    _dispatch(
      TransactionItem(
        name: name,
        op: op,
        durationMs: duration.inMicroseconds / 1000.0,
        status: status,
        httpMethod: httpMethod,
        httpStatus: httpStatus,
        url: url,
        distinctId: _analyticsDistinctId,
        sessionId: sessionId,
        // Leaf-site workflow stamp 3 of 3 — see the note on [_currentWorkflow].
        workflowId: _currentWorkflow?.workflowId,
        workflowName: _currentWorkflow?.name,
        timestamp: DateTime.now().toUtc(),
      ),
    );
  }

  /// Identifies the current user and records an identify event.
  ///
  /// The item carries `anonymous_id` only when the anonymous id was actually
  /// used as a `distinct_id` first, so the server can stitch that activity onto
  /// the named user. See [_anonymousIdUsed] for why a speculative alias is
  /// worse than none.
  void identify(String distinctId, {Map<String, Object?>? traits}) {
    if (!isEnabled) {
      return;
    }
    final String? aliasOf = _anonymousIdUsed ? _anonymousId : null;
    final SauronUser? existing = _scope.user;
    _scope.user = SauronUser(
      id: distinctId,
      email: existing?.email,
      traits: traits ?? existing?.traits ?? const <String, Object?>{},
    );
    _dispatch(
      IdentifyItem(
        distinctId: distinctId,
        anonymousId: aliasOf,
        traits: traits,
      ),
    );
  }

  /// Forgets the current person: clears the scope user and mints a fresh
  /// anonymous id, persisting it.
  ///
  /// **Call this on logout.** Without it the next person to use the device
  /// inherits the persisted anonymous id, and their first [identify] aliases
  /// that id — and with it the previous person's anonymous activity — onto the
  /// new account, permanently, server-side.
  ///
  /// Unlike the browser SDK, [setUser] with `null` does NOT do this for you:
  /// persisting the new id is asynchronous and [setUser] is not, so an
  /// unawaited file write hidden inside a setter would leave logout's most
  /// consequential side effect racing app teardown.
  Future<void> reset() async {
    _scope.user = null;
    _anonymousIdUsed = false;
    final Directory? dir = _storageDirectory;
    if (dir == null) {
      // Never bootstrapped: there is no persisted id to replace.
      return;
    }
    _anonymousId = await _anonymousIdStore.mintFresh(dir);
  }

  /// Adds a breadcrumb to the current scope.
  void addBreadcrumb(Breadcrumb crumb) => _scope.addBreadcrumb(crumb);

  /// Sets (or clears) the current user.
  ///
  /// Clearing it stops attributing activity to that user, but keeps the
  /// anonymous id — on logout call [reset] instead.
  void setUser(SauronUser? user) => _scope.user = user;

  /// Sets a single scope tag (last-write-wins by key).
  void setTag(String key, String value) => _scope.setTag(key, value);

  /// Merges scope tags (last-write-wins by key).
  void setTags(Map<String, String> values) => _scope.setTags(values);

  /// Sets (replaces) a named scope context block.
  void setContext(String name, Map<String, Object?> block) =>
      _scope.setContext(name, block);

  /// Sets a single scope extra value (last-write-wins by key).
  void setExtra(String key, Object? value) => _scope.setExtra(key, value);

  /// Flushes buffered + persisted envelopes.
  Future<void> flush() async => _transport?.flush();

  /// Flushes and tears down the client: drains the transport, uninstalls the
  /// capture layers (handing the global hooks back to whoever owned them), and
  /// disables the client.
  ///
  /// Terminal and idempotent. A closed client cannot be restarted — anything
  /// captured afterwards is dropped rather than buffered, so a long-lived
  /// process that closes the SDK does not accumulate events forever.
  Future<void> close() async {
    if (_closed) {
      return;
    }
    _closed = true;
    _uninstallIntegrations();
    await _transport?.close();
    _transport = null;
    // Anything still waiting on a transport will never be sent.
    _pending.clear();
    // Clear (not cancel) any active workflow — an abandoned workflow is a
    // legitimate server-derived outcome (30 min of inactivity, computed on
    // read); fabricating a $workflow_cancel here would misreport it.
    _currentWorkflow = null;
  }

  void _uninstallIntegrations() {
    FlutterErrorIntegration.uninstall();
    PlatformDispatcherIntegration.uninstall();
    if (!kIsWeb) {
      IsolateErrorIntegration.uninstall();
    }
    SauronWidgetsBindingObserver.uninstall();
  }

  /// Registers an error listener on a user-spawned [isolate].
  void addIsolateErrorListener(Isolate isolate) {
    if (!isEnabled || kIsWeb) {
      return;
    }
    IsolateErrorIntegration.addIsolate(isolate, this);
  }

  // ---- internals -------------------------------------------------------------

  /// Effective tags = scope (init defaults + runtime setters) then per-call,
  /// last-write-wins by key. Empty result is omitted on the wire.
  Map<String, String> _mergeTags(Map<String, String>? call) =>
      <String, String>{..._scope.tags, if (call != null) ...call};

  /// Effective contexts merge by BLOCK NAME — a per-call block replaces the
  /// same-named scope block.
  Map<String, Map<String, Object?>> _mergeContexts(
          Map<String, Map<String, Object?>>? call) =>
      <String, Map<String, Object?>>{
        ..._scope.contexts,
        if (call != null) ...call,
      };

  /// Effective extra = scope then per-call, shallow last-write-wins by key.
  Map<String, Object?> _mergeExtra(Map<String, Object?>? call) =>
      <String, Object?>{..._scope.extra, if (call != null) ...call};

  /// The single door to the transport — every captured item passes through
  /// here, and `bootstrap`'s replay of `_pending` only re-delivers items that
  /// already came through it.
  ///
  /// It is deliberately **not** where `workflow_id`/`workflow_name` are
  /// stamped: items arrive fully constructed with `final` fields, so stamping
  /// here would need a `copyWith` on every item class. See the note on
  /// [_currentWorkflow] for the consequence a new capture path must respect.
  void _dispatch(EnvelopeItem item) {
    EnvelopeItem outgoing = item;
    final BeforeSendCallback? beforeSend = options.beforeSend;
    if (beforeSend != null) {
      Object? processed;
      try {
        processed = beforeSend(item);
      } on Object catch (error) {
        // A throwing beforeSend must not propagate into host code — telemetry
        // never throws is a guarantee this SDK makes. Treat it as if the hook
        // had returned the item unchanged (NOT as a drop — that's the
        // deliberate `return null` case handled below).
        _log('beforeSend threw, dispatching "${item.type}" unmodified: $error');
        processed = item;
      }
      if (processed == null) {
        _log('${item.type} dropped by beforeSend.');
        return;
      }
      // The hook may return a mutated or replacement item.
      outgoing = processed as EnvelopeItem;
    }
    final SauronTransport? transport = _transport;
    if (transport != null) {
      transport.capture(outgoing);
    } else {
      _pending.add(outgoing);
    }
  }

  /// How many analytics items have been dropped for want of an identity, and
  /// whether the un-gated explanation has been printed yet.
  int _droppedWithoutIdentity = 0;
  bool _identityWarningPrinted = false;

  /// Drop one analytics item that has no `distinct_id`, loudly.
  ///
  /// The wire's `AnalyticsItem.distinct_id` is a non-`Option` `String`, so
  /// sending `null` is not "a null field on one item" — the gateway fails to
  /// deserialize the ENTIRE envelope (`400 invalid_envelope`), the transport
  /// classifies 400 as non-retryable and drops the batch, and every unrelated
  /// error, transaction and identify batched alongside it dies too. Containing
  /// the failure to the one item that cannot be attributed is strictly less
  /// destructive.
  ///
  /// Since the anonymous id landed, the only way here is to track before
  /// `Sauron.init` has finished — [bootstrap] resolves an id unconditionally,
  /// falling back to an in-memory one when storage is unwritable.
  ///
  /// The first drop prints regardless of [SauronOptions.debug]: this used to be
  /// completely silent, which is exactly why it survived. Subsequent drops go to
  /// the debug log so a hot analytics loop cannot flood the console.
  void _dropWithoutIdentity(String name) {
    _droppedWithoutIdentity++;
    final String detail =
        'dropped analytics item "$name": no distinct_id. Neither an identified '
        'user nor an anonymous id exists yet — await Sauron.init(...) before '
        'track(), setScreen() or startWorkflow(). Sending it would make the '
        'ingest gateway reject the whole envelope, losing every error batched '
        'with it. (dropped so far: $_droppedWithoutIdentity)';
    if (!_identityWarningPrinted) {
      _identityWarningPrinted = true;
      debugPrint('[Sauron] $detail');
      return;
    }
    _log(detail);
  }

  EnvelopeHeader _buildHeader(DateTime sentAt) => EnvelopeHeader(
        dsn: _dsn!.toString(),
        sentAt: sentAt,
        release: options.release,
      );

  SauronContext _buildContext() => _deviceContext.current.copyWith(
        user: _scope.user ?? const SauronUser(),
      );

  void _log(String message) {
    if (options.debug) {
      debugPrint('[Sauron] $message');
    }
  }
}
