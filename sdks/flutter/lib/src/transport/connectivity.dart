import 'dart:async';

import 'package:connectivity_plus/connectivity_plus.dart';

/// Thin wrapper over `connectivity_plus` that notifies when the device
/// (re)gains network access, so the transport can drain its offline queue.
///
/// Note: `onConnectivityChanged` now emits a **`List<ConnectivityResult>`**.
/// Connectivity is treated as a *hint* — the authoritative signal of a
/// successful send is always the HTTP response.
class ConnectivityMonitor {
  ConnectivityMonitor({Connectivity? connectivity})
      : _connectivity = connectivity ?? Connectivity();

  final Connectivity _connectivity;
  StreamSubscription<List<ConnectivityResult>>? _subscription;

  /// Starts listening. [onOnline] fires whenever connectivity transitions to a
  /// state with at least one non-`none` interface.
  void start(void Function() onOnline) {
    _subscription ??= _connectivity.onConnectivityChanged.listen(
      (List<ConnectivityResult> results) {
        if (_isOnline(results)) {
          onOnline();
        }
      },
      // Swallow platform errors — connectivity is only a hint.
      onError: (Object _) {},
    );
  }

  /// One-shot connectivity check. Returns `true` when likely online.
  Future<bool> get isOnline async {
    try {
      final List<ConnectivityResult> results =
          await _connectivity.checkConnectivity();
      return _isOnline(results);
    } on Object {
      // If the platform can't tell us, assume online and let HTTP decide.
      return true;
    }
  }

  static bool _isOnline(List<ConnectivityResult> results) =>
      results.any((ConnectivityResult r) => r != ConnectivityResult.none);

  /// Stops listening and releases resources.
  Future<void> dispose() async {
    await _subscription?.cancel();
    _subscription = null;
  }
}
