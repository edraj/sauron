import 'dart:async';

import 'package:flutter/widgets.dart';

import '../client.dart';
import '../types.dart';

/// Layer 4: the outermost catch-all. Runs the app inside [runZonedGuarded] so
/// any error not caught by the other three layers — including failures during
/// binding init before those layers install — still reaches Sauron.
///
/// The four capture layers are composed and bound **inside** the zone.
class RunZonedGuardedIntegration {
  const RunZonedGuardedIntegration._();

  static void run(SauronClient client, FutureOr<void> Function() appRunner) {
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
}
