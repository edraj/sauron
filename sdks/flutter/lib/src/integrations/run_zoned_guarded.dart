import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import '../client.dart';
import '../types.dart';

/// Layer 4: the outermost catch-all. Runs the app inside [runZonedGuarded] so
/// any error not caught by the other three layers — including failures during
/// binding init before those layers install — still reaches Sauron.
///
/// The four capture layers are composed and bound **inside** the zone.
///
/// Layer 4 is only available when Sauron owns binding initialization. A zone
/// can only be forked safely before `WidgetsFlutterBinding.ensureInitialized()`
/// runs: the binding remembers the zone it was built in, and `runApp` asserts
/// it is still in that zone (`BindingBase.debugCheckZone`). An app that has to
/// touch the binding first — to read a persisted DSN, initialize Firebase, lock
/// orientation — has already fixed the zone by the time [run] is reached, so
/// forking one here would report "Zone mismatch." from inside `runApp` and
/// leave every zone-scoped callback split across two zones.
///
/// In that case the app is run in the caller's zone instead. Layers 1–3 still
/// cover every uncaught error: [PlatformDispatcher.onError] is Flutter's
/// supported catch-all for async errors outside a guarded zone, which is
/// exactly what layer 4 would have caught.
class RunZonedGuardedIntegration {
  const RunZonedGuardedIntegration._();

  static Future<void> run(
    SauronClient client,
    FutureOr<void> Function() appRunner,
  ) async {
    if (_bindingIsInitialized()) {
      if (client.options.debug) {
        debugPrint(
          '[Sauron] the Flutter binding was already initialized, so the app '
          'runs in the current zone (runZonedGuarded would trip "Zone '
          'mismatch." in runApp). Uncaught errors are still captured via '
          'PlatformDispatcher.onError. To enable the runZonedGuarded layer, '
          'move WidgetsFlutterBinding.ensureInitialized() and any pre-runApp '
          'setup into the appRunner callback.',
        );
      }
      client.installIntegrations();
      await client.bootstrap();
      await appRunner();
      return;
    }

    runZonedGuarded<Future<void>>(
      () async {
        // Bind inside the zone so binding-owned callbacks run here too.
        WidgetsFlutterBinding.ensureInitialized();
        client.installIntegrations();
        await client.bootstrap();
        await appRunner();
      },
      (Object error, StackTrace stack) {
        client.captureException(
          error,
          stackTrace: stack,
          mechanism: const Mechanism(
            type: 'runZonedGuarded',
            handled: false,
          ),
        );
      },
    );
  }

  /// Whether the binding already exists, and with it the zone `runApp` must
  /// run in.
  ///
  /// There is no mode-independent accessor that reports this without building
  /// the binding — `WidgetsBinding.instance` throws a [FlutterError] in debug
  /// and a [TypeError] in release — and `BindingBase.debugBindingType()` is
  /// always null in release. So probe the getter.
  static bool _bindingIsInitialized() {
    try {
      WidgetsBinding.instance;
      return true;
    } catch (_) {
      return false;
    }
  }
}
