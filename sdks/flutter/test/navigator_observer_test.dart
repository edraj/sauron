import 'dart:convert';
import 'dart:io';

import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// [SauronNavigatorObserver] exposes two independent switches:
/// [SauronNavigatorObserver.recordTransactions] (navigation timings) and
/// [SauronNavigatorObserver.trackScreens] (screen attribution). These tests pin
/// that each works on its own — enabling screen tracking must not require
/// opting into transactions.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, Object?>> items = <Map<String, Object?>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_navobs_test');
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

  Route<void> route(String? name) => PageRouteBuilder<void>(
        settings: RouteSettings(name: name),
        pageBuilder: (_, __, ___) => const SizedBox.shrink(),
      );

  List<Map<String, Object?>> transactions() => items
      .where((Map<String, Object?> i) => i['type'] == 'transaction')
      .toList();

  test('trackScreens drives setScreen with recordTransactions disabled',
      () async {
    final SauronClient client = await buildClient();
    final SauronNavigatorObserver observer = SauronNavigatorObserver(
      client,
      recordTransactions: false,
    );

    observer.didPush(route('Home'), null);

    expect(client.screen, 'Home');
    await client.close();
  });

  test('screens keep tracking across pushes when transactions are disabled',
      () async {
    final SauronClient client = await buildClient();
    final SauronNavigatorObserver observer = SauronNavigatorObserver(
      client,
      recordTransactions: false,
    );

    observer.didPush(route('Home'), null);
    observer.didPush(route('Checkout'), route('Home'));

    expect(client.screen, 'Checkout');

    await client.flush();
    await client.close();
    // Screen tracking must not smuggle in transactions.
    expect(transactions(), isEmpty);
  });

  test('trackScreens disabled leaves the screen untouched', () async {
    final SauronClient client = await buildClient();
    final SauronNavigatorObserver observer = SauronNavigatorObserver(
      client,
      trackScreens: false,
    );

    observer.didPush(route('Home'), null);

    expect(client.screen, isNull);
    await client.close();
  });

  test('recordTransactions still times route dwell', () async {
    final SauronClient client = await buildClient();
    final SauronNavigatorObserver observer = SauronNavigatorObserver(client);

    observer.didPush(route('Home'), null);
    await Future<void>.delayed(const Duration(milliseconds: 10));
    // Leaving Home emits the navigation transaction for it.
    observer.didPush(route('Checkout'), route('Home'));

    await client.flush();
    await client.close();

    final List<Map<String, Object?>> navs = transactions()
        .where((Map<String, Object?> t) => t['op'] == 'navigation')
        .toList();
    expect(navs, hasLength(1));
    expect(navs.single['name'], 'Home');
  });

  test('unnamed routes are ignored by screen tracking', () async {
    final SauronClient client = await buildClient();
    final SauronNavigatorObserver observer = SauronNavigatorObserver(
      client,
      recordTransactions: false,
    );

    observer.didPush(route('Home'), null);
    observer.didPush(route(null), route('Home'));

    expect(client.screen, 'Home');
    await client.close();
  });
}
