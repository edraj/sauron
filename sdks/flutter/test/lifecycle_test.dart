import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// `close()` is terminal. Once it returns, the client must stop accepting
/// work: anything captured afterwards is dropped rather than buffered, and the
/// globally-installed capture layers are handed back to whoever owned them.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, Object?>> items = <Map<String, Object?>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_lifecycle_test');
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

  Future<SauronClient> buildClient() async {
    final SauronOptions options = SauronOptions()
      ..dsn = 'https://pk_test@localhost:9/1'
      ..httpClient = httpClient
      ..gzipThresholdBytes = 1 << 30;
    final SauronClient client = SauronClient(options);
    await client.bootstrap(queueDirectory: dir);
    return client;
  }

  List<Map<String, Object?>> events() =>
      items.where((Map<String, Object?> i) => i['type'] == 'event').toList();

  test('close() disables the client', () async {
    final SauronClient client = await buildClient();
    expect(client.isEnabled, isTrue);

    await client.close();

    expect(client.isEnabled, isFalse);
  });

  test('events captured after close() are dropped, not buffered', () async {
    final SauronClient client = await buildClient();
    await client.close();

    client.track('after_close');
    client.identify('u_1');
    client.trackTransaction(
      name: 'late',
      duration: const Duration(milliseconds: 5),
    );
    client.captureException(StateError('late boom'));

    // A leaked pending buffer would replay everything on the next bootstrap.
    await client.bootstrap(queueDirectory: dir);
    await client.flush();

    expect(items, isEmpty);
  });

  test('a post-close capture loop does not grow the pending buffer', () async {
    final SauronClient client = await buildClient();
    await client.close();

    for (int i = 0; i < 5000; i++) {
      client.track('spam_$i');
    }

    await client.bootstrap(queueDirectory: dir);
    await client.flush();

    expect(events(), isEmpty);
  });

  test('close() restores the previous FlutterError handler', () async {
    final FlutterExceptionHandler? original = FlutterError.onError;
    final SauronClient client = await buildClient();

    client.installIntegrations();
    expect(FlutterError.onError, isNot(same(original)));

    await client.close();

    expect(FlutterError.onError, same(original));
  });

  test('close() is idempotent', () async {
    final SauronClient client = await buildClient();
    await client.close();
    await client.close();
    expect(client.isEnabled, isFalse);
  });

  test('items captured before bootstrap still replay', () async {
    final SauronOptions options = SauronOptions()
      ..dsn = 'https://pk_test@localhost:9/1'
      ..httpClient = httpClient
      ..gzipThresholdBytes = 1 << 30;
    final SauronClient client = SauronClient(options);

    // No transport yet — this must be buffered, not dropped.
    client.track('early');

    await client.bootstrap(queueDirectory: dir);
    await client.flush();
    await client.close();

    expect(
      events().map((Map<String, Object?> e) => e['name']),
      contains('early'),
    );
  });
}
