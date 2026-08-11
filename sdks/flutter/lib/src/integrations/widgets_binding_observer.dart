import 'dart:ui';

import 'package:flutter/widgets.dart';

import '../client.dart';
import '../types.dart';

/// Observes app lifecycle transitions: records a breadcrumb on each change and
/// flushes the transport when the app is backgrounded (`paused`) or torn down
/// (`detached`) so buffered data is not lost.
///
/// It also flushes on `resumed`. Returning to the foreground is the SDK's cue to
/// drain envelopes queued while the app was away or offline — the SDK carries no
/// connectivity plugin, so this (plus the periodic flush timer) is what gets a
/// backlog moving again once the network returns.
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

  bool _ignoredInactiveForKeyboard = false;

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // Ignore lifecycle transitions that are likely caused by the keyboard
    // opening or closing on iOS.
    if (state == AppLifecycleState.inactive) {
      final double bottomInset = WidgetsBinding
              .instance.platformDispatcher.implicitView?.viewInsets.bottom ??
          0.0;
      if (bottomInset > 0.0) {
        _ignoredInactiveForKeyboard = true;
        return;
      }
    } else if (state == AppLifecycleState.resumed) {
      if (_ignoredInactiveForKeyboard) {
        _ignoredInactiveForKeyboard = false;
        return;
      }
    } else {
      _ignoredInactiveForKeyboard = false;
    }

    _client.addBreadcrumb(
      Breadcrumb(
        type: 'navigation',
        category: 'app.lifecycle',
        message: state.name,
      ),
    );
    if (state == AppLifecycleState.paused ||
        state == AppLifecycleState.detached ||
        state == AppLifecycleState.resumed) {
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
  /// newly-active [route], and attributes subsequent signals to it. Never
  /// throws — instrumentation must not break navigation.
  ///
  /// [recordTransactions] and [trackScreens] are independent: either may be
  /// enabled without the other.
  void _enterRoute(Route<dynamic>? route) {
    try {
      final String? name = route?.settings.name;
      if (recordTransactions) {
        _emitScreenTransaction();
        _currentRouteEnteredAt = DateTime.now().toUtc();
        _currentRouteName = name;
      }
      if (trackScreens && name != null) {
        _client.setScreen(name);
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
