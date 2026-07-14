import 'dart:isolate';

import 'package:flutter/foundation.dart';

import '../client.dart';
import '../types.dart';

/// Layer 3: uncaught errors on the current (and, optionally, user-spawned)
/// isolates via [Isolate.addErrorListener].
///
/// The error listener delivers a two-element list `[error, stackTrace]` where
/// both entries are already serialized to `String`. Gated on `!kIsWeb` because
/// `dart:isolate` is absent on the web.
class IsolateErrorIntegration {
  const IsolateErrorIntegration._();

  static RawReceivePort? _port;
  static bool _installed = false;

  static void install(SauronClient client) {
    if (kIsWeb || _installed) {
      return;
    }
    _installed = true;
    final RawReceivePort port = RawReceivePort(
      (dynamic message) => _handle(client, message),
    );
    Isolate.current.addErrorListener(port.sendPort);
    _port = port;
  }

  /// Attaches an error listener to a user-spawned [isolate].
  static void addIsolate(Isolate isolate, SauronClient client) {
    if (kIsWeb) {
      return;
    }
    final RawReceivePort port = RawReceivePort(
      (dynamic message) => _handle(client, message),
    );
    isolate.addErrorListener(port.sendPort);
  }

  static void _handle(SauronClient client, dynamic message) {
    if (message is! List || message.length < 2) {
      return;
    }
    final Object errorRepr = message[0] as Object? ?? 'Unknown isolate error';
    final Object? stackRepr = message[1];
    final StackTrace? stack = stackRepr is String && stackRepr.isNotEmpty
        ? StackTrace.fromString(stackRepr)
        : null;
    client.captureException(
      errorRepr,
      stackTrace: stack,
      mechanism: const Mechanism(
        type: 'Isolate.addErrorListener',
        handled: false,
      ),
      level: SauronLevel.fatal,
    );
  }

  /// Closes the current-isolate error port.
  static void uninstall() {
    _port?.close();
    _port = null;
    _installed = false;
  }
}
