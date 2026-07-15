import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// `beforeSend` was widened in 0.3.0 from an errors-only hook to an any-item
/// hook: it now runs on EVERY outgoing item (error / event / identify /
/// transaction). We drive `track`/`captureException`/`trackTransaction`, capture
/// the posted envelope bodies via a mock HTTP client, and assert on which item
/// types actually reached the wire.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, Object?>> items = <Map<String, Object?>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_before_send_test');
    httpClient = _MockClient();
    items.clear();
    when(() => httpClient.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer((Invocation invocation) async {
      final Object? body = invocation.namedArguments[const Symbol('body')];
      final List<int> bytes =
          body is String ? utf8.encode(body) : body as List<int>;
      final Map<String, dynamic> env =
          jsonDecode(utf8.decode(bytes)) as Map<String, dynamic>;
      for (final dynamic item in env['items'] as List<dynamic>) {
        items.add((item as Map<String, dynamic>).cast<String, Object?>());
      }
      return http.Response('', 202);
    });
  });

  tearDown(() async {
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  Future<SauronClient> buildClient(BeforeSendCallback beforeSend) async {
    final SauronOptions options = SauronOptions()
      ..dsn = 'https://pk_test@localhost:9/1'
      ..httpClient = httpClient
      // Never gzip in tests so the posted body is plain JSON.
      ..gzipThresholdBytes = 1 << 30
      ..beforeSend = beforeSend;
    final SauronClient client = SauronClient(options);
    await client.bootstrap(queueDirectory: dir);
    return client;
  }

  List<Map<String, Object?>> ofType(String type) =>
      items.where((Map<String, Object?> i) => i['type'] == type).toList();

  List<Object?> names() =>
      ofType('event').map((Map<String, Object?> e) => e['name']).toList();

  test('beforeSend can drop a non-error item (an event)', () async {
    final SauronClient client = await buildClient((Object item) {
      if (item is EventItem && item.name == 'secret') {
        return null; // drop this analytics event
      }
      return item;
    });
    client.track('kept');
    client.track('secret');
    await client.flush();
    await client.close();

    expect(names(), contains('kept'));
    expect(names(), isNot(contains('secret')));
  });

  test('an error-only beforeSend still drops errors and passes events',
      () async {
    // The pre-0.3.0 usage pattern: only act on errors. Non-errors are returned
    // unchanged, so they still reach the wire.
    final SauronClient client = await buildClient((Object item) {
      if (item is ErrorItem) {
        return null; // drop all errors
      }
      return item;
    });
    client.track('kept');
    client.captureException(StateError('boom'));
    // captureException fires its own unawaited flush; let it settle.
    await Future<void>.delayed(const Duration(milliseconds: 50));
    await client.flush();
    await client.close();

    expect(ofType('error'), isEmpty);
    expect(names(), contains('kept'));
  });

  test('beforeSend can drop a transaction item', () async {
    final SauronClient client = await buildClient((Object item) {
      if (item is TransactionItem) {
        return null;
      }
      return item;
    });
    client.trackTransaction(
      name: 'GET /users',
      op: 'http',
      duration: const Duration(milliseconds: 12),
    );
    client.track('kept');
    await client.flush();
    await client.close();

    expect(ofType('transaction'), isEmpty);
    expect(names(), contains('kept'));
  });

  test('beforeSend can replace/mutate a non-error item', () async {
    // Returning a different item replaces the original on the wire.
    final SauronClient client = await buildClient((Object item) {
      if (item is EventItem && item.name == 'raw') {
        return EventItem(
          name: 'redacted',
          timestamp: item.timestamp,
          distinctId: item.distinctId,
          sessionId: item.sessionId,
          screen: item.screen,
          properties: const <String, Object?>{},
        );
      }
      return item;
    });
    client.track('raw', properties: <String, Object?>{'ssn': '123-45-6789'});
    await client.flush();
    await client.close();

    expect(names(), contains('redacted'));
    expect(names(), isNot(contains('raw')));
    final Map<String, Object?> redacted =
        ofType('event').firstWhere((Map<String, Object?> e) => e['name'] == 'redacted');
    expect(redacted['properties'], isEmpty);
  });
}
