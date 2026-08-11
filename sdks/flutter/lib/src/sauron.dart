import 'dart:async';
import 'dart:isolate';

import 'package:flutter/widgets.dart';

import 'client.dart';
import 'integrations/run_zoned_guarded.dart';
import 'sauron_options.dart';
import 'transaction.dart';
import 'types.dart';
import 'workflow.dart';

/// The public, static entry point to the Sauron SDK.
///
/// ```dart
/// await Sauron.init(
///   SauronOptions(
///     dsn: 'https://pk_test@localhost:8081/1',
///     release: 'app@1.4.2+1402',
///   ),
///   appRunner: () => runApp(const MyApp()),
/// );
/// ```
class Sauron {
  Sauron._();

  static SauronClient? _client;

  /// The active client, or `null` before [init] / after [close].
  static SauronClient? get client => _client;

  /// Whether the SDK is initialized and enabled.
  static bool get isEnabled => _client?.isEnabled ?? false;

  /// Initializes the SDK with the given [options].
  ///
  /// When [appRunner] is supplied, the app is launched inside
  /// `runZonedGuarded` with all four capture layers bound inside the zone.
  /// Without it, integrations are still installed but you are responsible for
  /// calling `runApp` yourself.
  ///
  /// Calling `WidgetsFlutterBinding.ensureInitialized()` before [init] is
  /// supported, but it pins the zone `runApp` must run in, so the
  /// `runZonedGuarded` layer is skipped to avoid a "Zone mismatch." from
  /// inside `runApp`. Uncaught errors are still captured by the other three
  /// layers. To keep all four, do that setup inside [appRunner] instead.
  static Future<void> init(
    SauronOptions options, {
    FutureOr<void> Function()? appRunner,
  }) async {
    final SauronClient client = SauronClient(options);
    _client = client;

    if (appRunner != null) {
      await RunZonedGuardedIntegration.run(client, appRunner);
    } else {
      WidgetsFlutterBinding.ensureInitialized();
      client.installIntegrations();
      await client.bootstrap();
    }
  }

  /// Captures an exception manually.
  static void captureException(
    Object error, {
    StackTrace? stackTrace,
    Mechanism? mechanism,
    SauronLevel level = SauronLevel.error,
    String? screen,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
  }) =>
      _client?.captureException(
        error,
        stackTrace: stackTrace,
        mechanism: mechanism,
        level: level,
        screen: screen,
        tags: tags,
        contexts: contexts,
        extra: extra,
      );

  /// Records a product-analytics event.
  static void track(
    String name, {
    Map<String, Object?>? properties,
    Map<String, String>? tags,
    Map<String, Map<String, Object?>>? contexts,
    Map<String, Object?>? extra,
  }) =>
      _client?.track(
        name,
        properties: properties,
        tags: tags,
        contexts: contexts,
        extra: extra,
      );

  /// Sets the current screen (emits a `$screen` view on change).
  static void setScreen(String name) => _client?.setScreen(name);

  /// The current screen name, or null.
  static String? get screen => _client?.screen;

  /// Starts a named workflow — a bounded span of activity stamped onto every
  /// error/event/transaction captured while it is active. See
  /// [SauronClient.startWorkflow] for the full status contract.
  ///
  /// Before `init` / after `close`, returns `disabled` like every other
  /// facade member.
  static WorkflowResult startWorkflow(String name, {bool force = false}) =>
      _client?.startWorkflow(name, force: force) ??
      const WorkflowResult(WorkflowStatus.disabled);

  /// Ends the active workflow (or the one named [name]). See
  /// [SauronClient.endWorkflow].
  static WorkflowResult endWorkflow([String? name]) =>
      _client?.endWorkflow(name) ?? const WorkflowResult(WorkflowStatus.disabled);

  /// Cancels the active workflow (or the one named [name]). See
  /// [SauronClient.cancelWorkflow].
  static WorkflowResult cancelWorkflow([String? name, String? reason]) =>
      _client?.cancelWorkflow(name, reason) ??
      const WorkflowResult(WorkflowStatus.disabled);

  /// The active workflow, or null if none (including before `init` / after
  /// `close`).
  static ActiveWorkflow? get workflow => _client?.workflow;

  /// Records a performance transaction: one timed operation (navigation, HTTP
  /// call, resource fetch, screen load, or a custom span).
  ///
  /// ```dart
  /// Sauron.trackTransaction(
  ///   name: 'GET /users',
  ///   op: 'http',
  ///   duration: stopwatch.elapsed,
  ///   httpMethod: 'GET',
  ///   httpStatus: 200,
  ///   url: 'https://api.example.com/users',
  /// );
  /// ```
  static void trackTransaction({
    required String name,
    required Duration duration,
    String op = 'custom',
    String? status,
    String? httpMethod,
    int? httpStatus,
    String? url,
  }) =>
      _client?.trackTransaction(
        name: name,
        duration: duration,
        op: op,
        status: status,
        httpMethod: httpMethod,
        httpStatus: httpStatus,
        url: url,
      );

  /// Starts a stateful transaction that computes its own duration.
  /// 
  /// The returned [ActiveTransaction] must be `.end()`ed or `.cancel()`ed 
  /// to record the span. Until then, it is not sent to the server.
  static ActiveTransaction startTransaction({
    required String name,
    String op = 'custom',
    String? status,
    String? httpMethod,
    int? httpStatus,
    String? url,
  }) =>
      _client?.startTransaction(
        name: name,
        op: op,
        status: status,
        httpMethod: httpMethod,
        httpStatus: httpStatus,
        url: url,
      ) ??
      ActiveTransaction(
        null,
        name: name,
        op: op,
        status: status,
        httpMethod: httpMethod,
        httpStatus: httpStatus,
        url: url,
      );

  /// Identifies the current user.
  static void identify(String distinctId, {Map<String, Object?>? traits}) =>
      _client?.identify(distinctId, traits: traits);

  /// The persisted anonymous id this install reports until [identify] names a
  /// user, or null before [init] has completed.
  static String? get anonymousId => _client?.anonymousId;

  /// Forgets the current person: clears the user and mints a fresh anonymous
  /// id. **Call this on logout** — see [SauronClient.reset].
  static Future<void> reset() async {
    await _client?.reset();
  }

  /// Adds a breadcrumb.
  static void addBreadcrumb(Breadcrumb crumb) =>
      _client?.addBreadcrumb(crumb);

  /// Sets (or clears) the current user.
  static void setUser(SauronUser? user) => _client?.setUser(user);

  /// Sets a single scope tag (last-write-wins by key).
  static void setTag(String key, String value) =>
      _client?.setTag(key, value);

  /// Merges scope tags (last-write-wins by key).
  static void setTags(Map<String, String> values) =>
      _client?.setTags(values);

  /// Sets (replaces) a named scope context block.
  static void setContext(String name, Map<String, Object?> block) =>
      _client?.setContext(name, block);

  /// Sets a single scope extra value (last-write-wins by key).
  static void setExtra(String key, Object? value) =>
      _client?.setExtra(key, value);

  /// Flushes buffered + persisted data.
  static Future<void> flush() async => _client?.flush();

  /// Flushes and shuts down the SDK.
  static Future<void> close() async {
    await _client?.close();
    _client = null;
  }

  /// Registers an error listener on a user-spawned [isolate].
  static void addIsolateErrorListener(Isolate isolate) =>
      _client?.addIsolateErrorListener(isolate);
}
