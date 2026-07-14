import 'dart:async';
import 'dart:isolate';

import 'package:flutter/material.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

Future<void> main() async {
  await Sauron.init(
    (SauronOptions o) {
      // A local dev DSN — point this at your Sauron ingest gateway.
      o.dsn = 'https://pk_test@localhost:8081/1';
      o.environment = 'production';
      o.release = 'sauron_example@1.0.0+1';
      o.sampleRate = 1.0;
      o.maxBreadcrumbs = 100;
      o.debug = true;
      o.flushInterval = const Duration(seconds: 5);
      o.beforeSend = (ErrorItem event) => event;
    },
    appRunner: () => runApp(const SauronExampleApp()),
  );
}

class SauronExampleApp extends StatelessWidget {
  const SauronExampleApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Sauron Example',
      theme: ThemeData(colorSchemeSeed: Colors.deepPurple, useMaterial3: true),
      navigatorObservers: <NavigatorObserver>[
        if (Sauron.client != null) SauronNavigatorObserver(Sauron.client!),
      ],
      home: const HomePage(),
    );
  }
}

class HomePage extends StatelessWidget {
  const HomePage({super.key});

  /// Layer 1 — thrown inside a gesture callback → `FlutterError.onError`.
  void _throwInGesture() {
    throw StateError('Boom from a gesture callback (FlutterError layer)');
  }

  /// Layer 2 — an unhandled async error → `PlatformDispatcher.onError`.
  void _throwAsync() {
    Future<void>.delayed(const Duration(milliseconds: 50), () {
      throw Exception('Boom from an async gap (PlatformDispatcher layer)');
    });
  }

  /// Layer 4 — a bare error scheduled on the zone.
  void _throwInTimer() {
    Timer(const Duration(milliseconds: 50), () {
      throw ArgumentError('Boom from a Timer (runZonedGuarded layer)');
    });
  }

  /// Layer 3 — an uncaught error on a spawned isolate.
  Future<void> _crashIsolate() async {
    final Isolate isolate = await Isolate.spawn<String>(
      _isolateEntry,
      'payload',
      paused: true,
    );
    Sauron.addIsolateErrorListener(isolate);
    isolate.resume(isolate.pauseCapability!);
  }

  static void _isolateEntry(String message) {
    throw StateError('Boom from a spawned isolate: $message');
  }

  /// Manual capture with an explicit mechanism.
  void _captureManually() {
    try {
      throw const FormatException('A handled, manually captured error');
    } on FormatException catch (error, stack) {
      Sauron.captureException(
        error,
        stackTrace: stack,
        mechanism: const Mechanism(type: 'manual', handled: true),
      );
    }
  }

  void _trackEvent() {
    Sauron.addBreadcrumb(Breadcrumb.ui('Tapped: track event'));
    Sauron.track(
      'checkout_completed',
      properties: <String, Object?>{'cart_value': 42.5, 'currency': 'USD'},
    );
  }

  void _identifyUser() {
    Sauron.identify('u_123', traits: <String, Object?>{'plan': 'pro'});
    Sauron.setUser(const SauronUser(id: 'u_123', email: 'dev@example.com'));
  }

  @override
  Widget build(BuildContext context) {
    final List<(String, VoidCallback)> actions = <(String, VoidCallback)>[
      ('Throw in gesture (FlutterError)', _throwInGesture),
      ('Throw async (PlatformDispatcher)', _throwAsync),
      ('Throw in Timer (runZonedGuarded)', _throwInTimer),
      ('Crash a spawned isolate', () => unawaited(_crashIsolate())),
      ('Capture manually', _captureManually),
      ('track() an event', _trackEvent),
      ('identify() a user', _identifyUser),
      ('flush() now', () => unawaited(Sauron.flush())),
    ];

    return Scaffold(
      appBar: AppBar(title: const Text('Sauron SDK Example')),
      body: ListView.separated(
        padding: const EdgeInsets.all(16),
        itemCount: actions.length,
        separatorBuilder: (_, __) => const SizedBox(height: 12),
        itemBuilder: (BuildContext context, int index) {
          final (String label, VoidCallback onTap) = actions[index];
          return FilledButton(
            onPressed: onTap,
            child: Text(label),
          );
        },
      ),
    );
  }
}
