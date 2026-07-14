import 'package:flutter/foundation.dart';

import '../client.dart';
import '../types.dart';

/// Layer 1: framework build/layout/paint/gesture/assertion errors reported via
/// [FlutterError.onError].
///
/// The previous handler is chained (preserving the debug "red screen" and any
/// existing console reporting).
class FlutterErrorIntegration {
  const FlutterErrorIntegration._();

  static FlutterExceptionHandler? _previous;
  static bool _installed = false;

  static void install(SauronClient client) {
    if (_installed) {
      return;
    }
    _installed = true;
    _previous = FlutterError.onError;
    FlutterError.onError = (FlutterErrorDetails details) {
      client.captureException(
        details.exception,
        stackTrace: details.stack,
        mechanism: const Mechanism(
          type: 'FlutterError.onError',
          handled: false,
        ),
      );
      // Chain the prior handler (defaults to dumping to console / presenting the
      // red error screen in debug). If none existed, keep the default behavior.
      final FlutterExceptionHandler? previous = _previous;
      if (previous != null) {
        previous(details);
      } else {
        FlutterError.presentError(details);
      }
    };
  }

  /// Restores the previous handler (used by tests / [SauronClient.close]).
  static void uninstall() {
    if (!_installed) {
      return;
    }
    FlutterError.onError = _previous;
    _previous = null;
    _installed = false;
  }
}
