import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// App version/build are supplied by the developer at init — the SDK does not
/// read them off the platform. These tests assert what lands in the envelope's
/// `context.app` block.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, Object?>> contexts = <Map<String, Object?>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_app_ctx_test');
    httpClient = _MockClient();
    contexts.clear();
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
      contexts.add((env['context'] as Map<String, dynamic>).cast<String, Object?>());
      return http.Response('', 202);
    });
  });

  tearDown(() async {
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  Future<SauronClient> buildClient({
    String? appVersion,
    String? appBuild,
  }) async {
    final SauronOptions options = SauronOptions()
      ..dsn = 'https://pk_test@localhost:9/1'
      ..httpClient = httpClient
      ..gzipThresholdBytes = 1 << 30
      ..appVersion = appVersion
      ..appBuild = appBuild;
    final SauronClient client = SauronClient(options);
    // An analytics item needs an identity: `distinct_id` is non-`Option` on the
    // wire, so `track`/`setScreen`/`startWorkflow` DROP the item when the scope
    // has no user (sending `null` would 400 the whole envelope). `setUser` sets
    // it without emitting an extra `identify` item.
    client.setUser(const SauronUser(id: 'u_123'));
    await client.bootstrap(queueDirectory: dir);
    return client;
  }

  Object? appBlock() => contexts.single['app'];

  test('appVersion and appBuild land in context.app', () async {
    final SauronClient client = await buildClient(
      appVersion: '1.4.2',
      appBuild: '1402',
    );
    client.track('ping');
    await client.flush();
    await client.close();

    expect(appBlock(), <String, Object?>{'version': '1.4.2', 'build': '1402'});
  });

  test('appVersion alone leaves build null', () async {
    final SauronClient client = await buildClient(appVersion: '1.4.2');
    client.track('ping');
    await client.flush();
    await client.close();

    expect(appBlock(), <String, Object?>{'version': '1.4.2', 'build': null});
  });

  test('neither set omits the app block entirely', () async {
    final SauronClient client = await buildClient();
    client.track('ping');
    await client.flush();
    await client.close();

    expect(appBlock(), isNull);
  });

  test('app context does not depend on platform plugins', () async {
    // No platform channels are available under flutter_test, so a value here
    // can only have come from the options the developer supplied.
    final SauronClient client = await buildClient(appVersion: '9.9.9');
    client.track('ping');
    await client.flush();
    await client.close();

    expect((appBlock()! as Map<String, Object?>)['version'], '9.9.9');
  });
}
