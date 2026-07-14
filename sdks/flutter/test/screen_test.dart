import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// The Flutter SDK only runs `beforeSend` on errors; analytics events flow
/// through the transport untouched. To assert what `track`/`setScreen` put on
/// the wire we capture the posted envelope bodies via a mock HTTP client and
/// decode their `items`.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, Object?>> items = <Map<String, Object?>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_screen_test');
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

  Future<SauronClient> buildClient({String? screen}) async {
    final SauronOptions options = SauronOptions()
      ..dsn = 'https://pk_test@localhost:9/1'
      ..httpClient = httpClient
      // Never gzip in tests so the posted body is plain JSON.
      ..gzipThresholdBytes = 1 << 30
      ..screen = screen;
    final SauronClient client = SauronClient(options);
    await client.bootstrap(queueDirectory: dir);
    return client;
  }

  List<Map<String, Object?>> events() =>
      items.where((Map<String, Object?> i) => i['type'] == 'event').toList();

  test('setScreen stamps screen and emits \$screen once on change', () async {
    final SauronClient client = await buildClient();
    client.setScreen('Home');
    client.setScreen('Home'); // no-op — same screen
    client.track('tapped');
    await client.flush();
    await client.close();

    final List<Map<String, Object?>> views = events()
        .where((Map<String, Object?> e) => e['name'] == r'$screen')
        .toList();
    expect(views, hasLength(1));
    expect((views.first['properties'] as Map<String, dynamic>)['screen'],
        'Home');

    final Map<String, Object?> tapped =
        events().firstWhere((Map<String, Object?> e) => e['name'] == 'tapped');
    expect(tapped['screen'], 'Home');
  });

  test('client.screen reflects the current screen', () async {
    final SauronClient client = await buildClient();
    expect(client.screen, isNull);
    client.setScreen('Home');
    expect(client.screen, 'Home');
    await client.close();
  });

  test('a per-call screen overrides the current screen', () async {
    final SauronClient client = await buildClient();
    client.setScreen('Home');
    client.track('tapped', screen: 'Checkout');
    await client.flush();
    await client.close();

    final Map<String, Object?> tapped =
        events().firstWhere((Map<String, Object?> e) => e['name'] == 'tapped');
    expect(tapped['screen'], 'Checkout');
  });

  test('options.screen seeds the initial screen', () async {
    final SauronClient client = await buildClient(screen: 'Splash');
    expect(client.screen, 'Splash');
    client.track('viewed');
    await client.flush();
    await client.close();

    final Map<String, Object?> viewed =
        events().firstWhere((Map<String, Object?> e) => e['name'] == 'viewed');
    expect(viewed['screen'], 'Splash');
  });

  test('captureException attaches the current screen', () async {
    final SauronClient client = await buildClient();
    client.setScreen('Home');
    client.captureException(StateError('boom'));
    // captureException kicks off its own (unawaited) flush; let it settle,
    // then flush again so the error envelope is drained before we assert.
    await Future<void>.delayed(const Duration(milliseconds: 50));
    await client.flush();
    await client.close();

    final Map<String, Object?> error =
        items.firstWhere((Map<String, Object?> i) => i['type'] == 'error');
    expect(error['screen'], 'Home');
  });
}
