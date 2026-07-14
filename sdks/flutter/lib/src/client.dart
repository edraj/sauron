import 'dart:io' show Directory;
import 'dart:isolate';
import 'dart:math';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

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
import 'transport/connectivity.dart';
import 'transport/queue.dart';
import 'transport/transport.dart';
import 'types.dart';
import 'util/uuid.dart';

/// The engine behind the [Sauron] facade: owns the scope, sampling, the
/// `beforeSend` hook, context, and the transport.
class SauronClient {
  SauronClient(this.options)
      : _scope = Scope(maxBreadcrumbs: options.maxBreadcrumbs),
        sessionId = generateUuidV4() {
    _currentScreen = options.screen;
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

  final Scope _scope;
  final DeviceContextProvider _deviceContext = DeviceContextProvider();
  final DartStackTraceParser _parser = const DartStackTraceParser();
  final Random _random = Random();
  final List<EnvelopeItem> _pending = <EnvelopeItem>[];

  Dsn? _dsn;
  SauronTransport? _transport;

  /// Whether the SDK is configured and active.
  bool get isEnabled => _dsn != null;

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
      connectivity: ConnectivityMonitor(),
    );
    _transport = transport;
    await _deviceContext.load(storageDirectory: dir);
    transport.start();
    // Replay anything captured before the transport was ready.
    for (final EnvelopeItem item in _pending) {
      transport.capture(item);
    }
    _pending.clear();
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
    ErrorItem item = ErrorItem(
      exception: exception,
      timestamp: DateTime.now().toUtc(),
      level: level,
      breadcrumbs: _scope.breadcrumbs,
      sessionId: sessionId,
      screen: screen ?? _currentScreen,
    );
    final BeforeSendCallback? beforeSend = options.beforeSend;
    if (beforeSend != null) {
      final ErrorItem? processed = beforeSend(item);
      if (processed == null) {
        _log('event dropped by beforeSend.');
        return;
      }
      item = processed;
    }
    _dispatch(item);
    // Errors are worth an eager flush attempt.
    _transport?.flush();
  }

  /// Records a product-analytics event.
  void track(String name, {Map<String, Object?>? properties, String? screen}) {
    if (!isEnabled) {
      return;
    }
    _dispatch(
      EventItem(
        name: name,
        timestamp: DateTime.now().toUtc(),
        distinctId: _scope.distinctId,
        sessionId: sessionId,
        screen: screen ?? _currentScreen,
        properties: properties,
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
        distinctId: _scope.distinctId,
        sessionId: sessionId,
        timestamp: DateTime.now().toUtc(),
      ),
    );
  }

  /// Identifies the current user and records an identify event.
  void identify(String distinctId, {Map<String, Object?>? traits}) {
    if (!isEnabled) {
      return;
    }
    final SauronUser? existing = _scope.user;
    _scope.user = SauronUser(
      id: distinctId,
      email: existing?.email,
      traits: traits ?? existing?.traits ?? const <String, Object?>{},
    );
    _dispatch(
      IdentifyItem(distinctId: distinctId, traits: traits),
    );
  }

  /// Adds a breadcrumb to the current scope.
  void addBreadcrumb(Breadcrumb crumb) => _scope.addBreadcrumb(crumb);

  /// Sets (or clears) the current user.
  void setUser(SauronUser? user) => _scope.user = user;

  /// Flushes buffered + persisted envelopes.
  Future<void> flush() async => _transport?.flush();

  /// Flushes and tears down the client.
  Future<void> close() async {
    await _transport?.close();
    _transport = null;
  }

  /// Registers an error listener on a user-spawned [isolate].
  void addIsolateErrorListener(Isolate isolate) {
    if (!isEnabled || kIsWeb) {
      return;
    }
    IsolateErrorIntegration.addIsolate(isolate, this);
  }

  // ---- internals -------------------------------------------------------------

  void _dispatch(EnvelopeItem item) {
    final SauronTransport? transport = _transport;
    if (transport != null) {
      transport.capture(item);
    } else {
      _pending.add(item);
    }
  }

  EnvelopeHeader _buildHeader(DateTime sentAt) => EnvelopeHeader(
        dsn: _dsn!.toString(),
        sentAt: sentAt,
        environment: options.environment,
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
