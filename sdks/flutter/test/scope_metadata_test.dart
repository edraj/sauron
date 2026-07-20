import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// Drives the client directly (as the other client tests do), capturing posted
/// envelope bodies via a mock HTTP client, and asserts the SDK-side merge of
/// init-default scope + runtime setters + per-call overrides on error/event.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, Object?>> items = <Map<String, Object?>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_scope_meta_test');
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

  Future<SauronClient> buildClient({bool seed = true}) async {
    final SauronOptions options = SauronOptions()
      ..dsn = 'https://pk_test@localhost:9/1'
      ..httpClient = httpClient
      // Never gzip in tests so the posted body is plain JSON.
      ..gzipThresholdBytes = 1 << 30;
    if (seed) {
      options
        ..tags = <String, String>{'env_tag': 'seed'}
        ..contexts = <String, Map<String, Object?>>{
          'order': <String, Object?>{'id': 1},
        }
        ..extra = <String, Object?>{'boot': true};
    }
    final SauronClient client = SauronClient(options);
    await client.bootstrap(queueDirectory: dir);
    return client;
  }

  List<Map<String, Object?>> events() =>
      items.where((Map<String, Object?> i) => i['type'] == 'event').toList();

  Future<Map<String, Object?>> errorAfter(
      SauronClient client, Future<void> Function() act) async {
    await act();
    // captureException fires its own unawaited flush; let it settle, then flush.
    await Future<void>.delayed(const Duration(milliseconds: 50));
    await client.flush();
    await client.close();
    return items.firstWhere((Map<String, Object?> i) => i['type'] == 'error');
  }

  test('init defaults seed the scope and are emitted on track', () async {
    final SauronClient client = await buildClient();
    client.track('viewed');
    await client.flush();
    await client.close();

    final Map<String, Object?> viewed =
        events().firstWhere((Map<String, Object?> e) => e['name'] == 'viewed');
    expect(viewed['tags'], <String, Object?>{'env_tag': 'seed'});
    expect(viewed['contexts'],
        <String, Object?>{'order': <String, Object?>{'id': 1}});
    expect(viewed['extra'], <String, Object?>{'boot': true});
  });

  test('runtime setters + per-call override merge per top-level key', () async {
    final SauronClient client = await buildClient();
    client.setTag('env_tag', 'runtime'); // overrides the seed
    client.setTag('extra_tag', 'x');
    client.setContext('cart', <String, Object?>{'items': 2});
    client.setExtra('flag', 'on');
    // Per-call: tag env_tag wins by key; contexts.order block replaced.
    client.track(
      'checkout',
      tags: <String, String>{'env_tag': 'call'},
      contexts: <String, Map<String, Object?>>{
        'order': <String, Object?>{'id': 99},
      },
    );
    await client.flush();
    await client.close();

    final Map<String, Object?> checkout = events()
        .firstWhere((Map<String, Object?> e) => e['name'] == 'checkout');
    expect(checkout['tags'],
        <String, Object?>{'env_tag': 'call', 'extra_tag': 'x'});
    expect(checkout['contexts'], <String, Object?>{
      'order': <String, Object?>{'id': 99},
      'cart': <String, Object?>{'items': 2},
    });
    expect(checkout['extra'], <String, Object?>{'boot': true, 'flag': 'on'});
  });

  test('captureException merges scope + per-call tags/extra', () async {
    final SauronClient client = await buildClient();
    client.setTag('feature', 'checkout');
    final Map<String, Object?> error = await errorAfter(client, () async {
      client.captureException(
        StateError('boom'),
        tags: <String, String>{'severity': 'high'},
        extra: <String, Object?>{'retries': 3},
      );
    });
    expect(error['tags'], <String, Object?>{
      'env_tag': 'seed',
      'feature': 'checkout',
      'severity': 'high',
    });
    expect(error['contexts'],
        <String, Object?>{'order': <String, Object?>{'id': 1}});
    expect(error['extra'], <String, Object?>{'boot': true, 'retries': 3});
  });

  test('no scope + no per-call metadata omits the keys', () async {
    final SauronClient client = await buildClient(seed: false);
    client.track('bare');
    await client.flush();
    await client.close();

    final Map<String, Object?> bare =
        events().firstWhere((Map<String, Object?> e) => e['name'] == 'bare');
    expect(bare.containsKey('tags'), isFalse);
    expect(bare.containsKey('contexts'), isFalse);
    expect(bare.containsKey('extra'), isFalse);
  });
}
