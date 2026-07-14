import 'dart:ui' as ui;

import '../client.dart';
import '../types.dart';

/// Layer 2: async errors with no framework callback — platform-channel
/// failures, bare async gaps — surfaced via [ui.PlatformDispatcher.onError].
///
/// Returns `true` so the engine treats the error as handled (it has been
/// captured), while still chaining any previously-installed handler.
class PlatformDispatcherIntegration {
  const PlatformDispatcherIntegration._();

  static ui.ErrorCallback? _previous;
  static bool _installed = false;

  static void install(SauronClient client) {
    if (_installed) {
      return;
    }
    _installed = true;
    final ui.PlatformDispatcher dispatcher = ui.PlatformDispatcher.instance;
    _previous = dispatcher.onError;
    dispatcher.onError = (Object error, StackTrace stack) {
      client.captureException(
        error,
        stackTrace: stack,
        mechanism: const Mechanism(
          type: 'PlatformDispatcher.onError',
          handled: false,
        ),
      );
      _previous?.call(error, stack);
      return true;
    };
  }

  /// Restores the previous handler.
  static void uninstall() {
    if (!_installed) {
      return;
    }
    ui.PlatformDispatcher.instance.onError = _previous;
    _previous = null;
    _installed = false;
  }
}
