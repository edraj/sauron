import 'dart:async';
import 'dart:isolate';

import 'package:flutter/widgets.dart';

import 'client.dart';
import 'integrations/run_zoned_guarded.dart';
import 'sauron_options.dart';
import 'types.dart';

/// The public, static entry point to the Sauron SDK.
///
/// ```dart
/// await Sauron.init((o) {
///   o.dsn = 'https://pk_test@localhost:8081/1';
///   o.environment = 'production';
///   o.release = 'app@1.4.2+1402';
/// }, appRunner: () => runApp(const MyApp()));
/// ```
class Sauron {
  Sauron._();

  static SauronClient? _client;

  /// The active client, or `null` before [init] / after [close].
  static SauronClient? get client => _client;

  /// Whether the SDK is initialized and enabled.
  static bool get isEnabled => _client?.isEnabled ?? false;

  /// Initializes the SDK.
  ///
  /// When [appRunner] is supplied, the app is launched inside
  /// `runZonedGuarded` with all four capture layers bound inside the zone.
  /// Without it, integrations are still installed but you are responsible for
  /// calling `runApp` yourself.
  static Future<void> init(
    void Function(SauronOptions options) configure, {
    FutureOr<void> Function()? appRunner,
  }) async {
    final SauronOptions options = SauronOptions();
    configure(options);
    final SauronClient client = SauronClient(options);
    _client = client;

    if (appRunner != null) {
      RunZonedGuardedIntegration.run(client, appRunner);
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

  /// Identifies the current user.
  static void identify(String distinctId, {Map<String, Object?>? traits}) =>
      _client?.identify(distinctId, traits: traits);

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
