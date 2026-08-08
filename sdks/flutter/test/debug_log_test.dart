import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// `debug: true` should make the SDK's outgoing traffic visible: every item the
/// server accepted is printed, so you can see what actually left the device
/// rather than inferring it from the absence of errors.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  const MethodChannel pathProvider =
      MethodChannel('plugins.flutter.io/path_provider');

  late Directory dir;
  late _MockClient httpClient;
  final List<String> logged = <String>[];
  late DebugPrintCallback originalDebugPrint;

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_debug_log_test');
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(
            pathProvider, (MethodCall _) async => dir.path);

    httpClient = _MockClient();
    when(() => httpClient.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer((_) async => http.Response('', 202));

    logged.clear();
    originalDebugPrint = debugPrint;
    debugPrint = (String? message, {int? wrapWidth}) {
      if (message != null) {
        logged.add(message);
      }
    };
  });

  // `flush()` returns early while a drain is already in flight, so a delivery
  // can land a few turns later. Settle before asserting, and before handing the
  // console back, so a straggler cannot print into the next test.
  Future<void> settle() =>
      Future<void>.delayed(const Duration(milliseconds: 50));

  tearDown(() async {
    await Sauron.close();
    await settle();
    debugPrint = originalDebugPrint;
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(pathProvider, null);
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  String joined() => logged.join('\n');

  test('logs every delivered item when debug is on', () async {
    await Sauron.init(SauronOptions(
      dsn: 'https://pk_test@localhost:9/1',
      httpClient: httpClient,
      debug: true,
    ));

    Sauron.setScreen('Checkout');
    Sauron.identify('u_123', traits: <String, Object?>{'plan': 'pro'});
    Sauron.track('checkout_completed',
        properties: <String, Object?>{'cart_value': 42.5});
    Sauron.captureException(StateError('card declined'));
    Sauron.trackTransaction(
      name: 'GET /orders',
      op: 'http',
      duration: const Duration(milliseconds: 120),
      httpStatus: 200,
    );
    await Sauron.flush();
    await settle();

    expect(joined(), contains('delivered'));
    expect(joined(), contains('identify u_123'));
    expect(joined(), contains('"plan":"pro"'));
    expect(joined(), contains('event checkout_completed'));
    expect(joined(), contains('"cart_value":42.5'));
    expect(joined(), contains('error StateError: Bad state: card declined'));
    expect(joined(), contains('screen=Checkout'));
    expect(joined(), contains('transaction GET /orders op=http 120.0ms'));
  });

  test('stays silent when debug is off', () async {
    await Sauron.init(SauronOptions(
      dsn: 'https://pk_test@localhost:9/1',
      httpClient: httpClient,
    ));

    Sauron.setUser(const SauronUser(id: 'u_123'));
    Sauron.track('checkout_completed');
    await Sauron.flush();
    await settle();

    expect(logged, isEmpty);
  });

  test('warns even with debug off when an item is dropped for no distinct_id',
      () async {
    // The ONE thing this SDK says out loud without `debug: true`, and
    // deliberately so: an unidentified analytics item is DROPPED (its
    // `distinct_id` is non-`Option` on the wire, and sending `null` would make
    // the gateway reject the entire envelope). That drop used to be invisible in
    // every build, which is exactly why it shipped. Printed once per client so a
    // hot analytics loop cannot flood the log.
    await Sauron.init(SauronOptions(
      dsn: 'https://pk_test@localhost:9/1',
      httpClient: httpClient,
    ));

    Sauron.track('checkout_completed');
    Sauron.track('viewed_pricing');
    await Sauron.flush();
    await settle();

    expect(logged, hasLength(1));
    expect(logged.single, contains('dropped analytics item'));
    expect(logged.single, contains('checkout_completed'));
    expect(logged.single, contains('identify'));
  });

  test('a long value is truncated onto one line', () async {
    await Sauron.init(SauronOptions(
      dsn: 'https://pk_test@localhost:9/1',
      httpClient: httpClient,
      debug: true,
    ));

    Sauron.captureException(StateError('x' * 500));
    await Sauron.flush();
    await settle();

    final String line = logged.firstWhere((String l) => l.contains('error '));
    expect(line, contains('...'));
    expect(line.length, lessThan(220));
  });
}
