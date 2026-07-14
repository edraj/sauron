import 'package:flutter/widgets.dart';

import '../client.dart';
import '../types.dart';

/// Observes app lifecycle transitions: records a breadcrumb on each change and
/// flushes the transport when the app is backgrounded (`paused`) or torn down
/// (`detached`) so buffered data is not lost.
class SauronWidgetsBindingObserver with WidgetsBindingObserver {
  SauronWidgetsBindingObserver(this._client);

  final SauronClient _client;

  static SauronWidgetsBindingObserver? _instance;

  /// Registers a singleton observer on the widgets binding.
  static void install(SauronClient client) {
    if (_instance != null) {
      return;
    }
    final SauronWidgetsBindingObserver observer =
        SauronWidgetsBindingObserver(client);
    _instance = observer;
    WidgetsBinding.instance.addObserver(observer);
  }

  /// Removes the observer.
  static void uninstall() {
    final SauronWidgetsBindingObserver? observer = _instance;
    if (observer != null) {
      WidgetsBinding.instance.removeObserver(observer);
      _instance = null;
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _client.addBreadcrumb(
      Breadcrumb(
        type: 'navigation',
        category: 'app.lifecycle',
        message: state.name,
      ),
    );
    if (state == AppLifecycleState.paused ||
        state == AppLifecycleState.detached) {
      _client.flush();
    }
  }
}

/// Records navigation breadcrumbs for a [Navigator]. Attach via
/// `navigatorObservers: [SauronNavigatorObserver(client)]`.
///
/// When [recordTransactions] is enabled (the default), it also emits a
/// `navigation` performance transaction whenever the user leaves a route,
/// timed by how long that route was on screen (its dwell duration). The
/// route's `settings.name` is used as the transaction name, so unnamed routes
/// contribute nothing.
class SauronNavigatorObserver extends NavigatorObserver {
  SauronNavigatorObserver(
    this._client, {
    this.recordTransactions = true,
    this.trackScreens = true,
  });

  final SauronClient _client;

  /// Whether to emit a `navigation` transaction on each route change.
  final bool recordTransactions;

  /// Whether to drive [SauronClient.setScreen] from named routes on each route
  /// change (so events/errors are attributed to the active screen). Unnamed
  /// routes are ignored.
  final bool trackScreens;

  DateTime? _currentRouteEnteredAt;
  String? _currentRouteName;

  void _record(String operation, Route<dynamic>? route) {
    final String name = route?.settings.name ?? '<unnamed>';
    _client.addBreadcrumb(
      Breadcrumb(
        type: 'navigation',
        category: 'route',
        message: name,
        data: <String, Object?>{'operation': operation},
      ),
    );
  }

  /// Emits a transaction for the route we're leaving, then starts timing the
  /// newly-active [route]. Never throws — instrumentation must not break
  /// navigation.
  void _enterRoute(Route<dynamic>? route) {
    if (!recordTransactions) {
      return;
    }
    try {
      _emitScreenTransaction();
      _currentRouteEnteredAt = DateTime.now().toUtc();
      _currentRouteName = route?.settings.name;
      if (trackScreens && route?.settings.name != null) {
        _client.setScreen(route!.settings.name!);
      }
    } on Object {
      // Never let instrumentation crash navigation.
    }
  }

  void _emitScreenTransaction() {
    final DateTime? enteredAt = _currentRouteEnteredAt;
    final String? name = _currentRouteName;
    if (enteredAt == null || name == null) {
      return;
    }
    _client.trackTransaction(
      name: name,
      op: 'navigation',
      duration: DateTime.now().toUtc().difference(enteredAt),
    );
  }

  @override
  void didPush(Route<dynamic> route, Route<dynamic>? previousRoute) {
    _record('push', route);
    _enterRoute(route);
    super.didPush(route, previousRoute);
  }

  @override
  void didPop(Route<dynamic> route, Route<dynamic>? previousRoute) {
    _record('pop', route);
    // We're returning to previousRoute; time it from here.
    _enterRoute(previousRoute);
    super.didPop(route, previousRoute);
  }

  @override
  void didReplace({Route<dynamic>? newRoute, Route<dynamic>? oldRoute}) {
    _record('replace', newRoute);
    _enterRoute(newRoute);
    super.didReplace(newRoute: newRoute, oldRoute: oldRoute);
  }

  @override
  void didRemove(Route<dynamic> route, Route<dynamic>? previousRoute) {
    _record('remove', route);
    _enterRoute(previousRoute);
    super.didRemove(route, previousRoute);
  }
}
