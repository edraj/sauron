import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

import 'wire_fixture_io.dart';

class _MockClient extends Mock implements http.Client {}

/// Captures the envelope this SDK **actually posts** into
/// `sdks/wire-fixtures/flutter.json`, where the backend's
/// `cargo test -p sauron-core --test sdk_wire_conformance` feeds it through the
/// real `serde` deserializer.
///
/// `envelope_test.dart` compares against a literal authored in this repo, so it
/// could not see the live defect: `EventItem.distinct_id` is `null` until
/// `identify()` is called, and the wire's `AnalyticsItem.distinct_id` is a
/// non-`Option` `String`. Every envelope carrying a `track`, a `$screen` or a
/// `$workflow_*` event before `identify()` — including the ones the SDK emits
/// itself — was a 400 `invalid_envelope`. The envelope is all-or-nothing, so
/// that one item took every unrelated error and transaction in the batch with
/// it, and the transport drops a 400 without retrying.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, Object?>> envelopes = <Map<String, Object?>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_wire_fixture');
    httpClient = _MockClient();
    envelopes.clear();
    when(() => httpClient.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer((Invocation invocation) async {
      final Object? body = invocation.namedArguments[const Symbol('body')];
      final List<int> bytes =
          body is String ? utf8.encode(body) : body as List<int>;
      envelopes.add((jsonDecode(utf8.decode(bytes)) as Map<String, dynamic>)
          .cast<String, Object?>());
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
      ..dsn = 'https://pk_test@localhost:8081/1'
      ..release = 'app@1.4.2+1402'
      ..appVersion = '1.4.2'
      ..appBuild = '1402'
      ..httpClient = httpClient
      // Never gzip, so the captured body is plain JSON.
      ..gzipThresholdBytes = 1 << 30
      // One envelope, flushed by captureException's eager flush at the end.
      ..flushInterval = const Duration(hours: 1)
      ..maxBatchItems = 1000;
    final SauronClient client = SauronClient(options);
    await client.bootstrap(queueDirectory: dir);
    return client;
  }

  List<Map<String, Object?>> itemsOf(Map<String, Object?> envelope) =>
      (envelope['items'] as List<dynamic>)
          .map((dynamic i) => (i as Map<String, dynamic>).cast<String, Object?>())
          .toList();

  test('posts an envelope that is captured verbatim into '
      'sdks/wire-fixtures/flutter.json', () async {
    final SauronClient client = await buildClient();

    // Deliberately BEFORE identify(): this is the shape that used to poison the
    // whole envelope. After the guard it is dropped (see the next test), so the
    // fixture proves the envelope stays parseable either way.
    client.track('viewed_pricing', properties: <String, Object?>{'plan': 'pro'});
    client.setScreen('/checkout'); // SDK-emitted `$screen`

    client.identify('u_123', traits: <String, Object?>{'plan': 'pro'});
    client.setTag('env', 'prod');
    client.addBreadcrumb(Breadcrumb(
      type: 'navigation',
      category: 'route',
      message: '/settings',
      level: SauronLevel.info,
      timestamp: DateTime.now().toUtc(),
    ));
    client.track('checkout_completed',
        properties: <String, Object?>{'cart_value': 42.5});
    client.trackTransaction(
      name: 'GET /api/users',
      duration: const Duration(milliseconds: 128),
      op: 'http',
      status: 'ok',
      httpMethod: 'GET',
      httpStatus: 200,
      url: 'https://api.example.com/api/users',
    );
    // LAST on purpose: captureException fires its own eager flush, which posts
    // everything buffered above as a single envelope.
    try {
      throw StateError('x is not valid');
    } catch (error, stack) {
      client.captureException(error, stackTrace: stack);
    }
    await Future<void>.delayed(const Duration(milliseconds: 100));
    await client.flush();
    await client.close();

    expect(envelopes, hasLength(1),
        reason: 'the fixture must be one real posted envelope');
    final Map<String, Object?> envelope = envelopes.first;
    final List<String> types = itemsOf(envelope)
        .map((Map<String, Object?> i) => i['type'] as String)
        .toList();
    for (final String required in <String>[
      'error',
      'event',
      'identify',
      'transaction',
    ]) {
      expect(types, contains(required));
    }

    writeWireFixture('flutter', envelope);
  });

  test('drops an analytics item with no distinct_id instead of poisoning the '
      'envelope', () async {
    final SauronClient client = await buildClient();

    // No identify() anywhere: `scope.distinctId` is null, so every one of these
    // would serialize `"distinct_id": null` against a non-`Option` `String`.
    client.track('viewed_pricing');
    client.setScreen('/pricing'); // SDK-emitted `$screen`
    final WorkflowResult started = client.startWorkflow('checkout');
    expect(started.status, WorkflowStatus.ok); // the workflow itself still works
    client.endWorkflow('checkout');
    // An error item has no distinct_id on the wire at all, so it must still be
    // delivered — that is the whole point of dropping per item instead of
    // failing the batch.
    client.captureException(StateError('boom'));
    await Future<void>.delayed(const Duration(milliseconds: 100));
    await client.flush();
    await client.close();

    final List<Map<String, Object?>> items =
        envelopes.expand(itemsOf).toList();
    expect(items.where((Map<String, Object?> i) => i['type'] == 'error'),
        hasLength(1),
        reason: 'the error must survive the dropped analytics items');
    for (final Map<String, Object?> item in items) {
      // `distinct_id` is `Option<String>` for a transaction and absent entirely
      // on an error, but non-`Option` for event/identify — those are the two
      // that take the whole envelope down when null.
      if (item['type'] == 'event' || item['type'] == 'identify') {
        expect(item['distinct_id'], isNotNull,
            reason: 'no event/identify item may ship distinct_id: null — the '
                'backend rejects the ENTIRE envelope, not just the item '
                '($item)');
      }
    }
    expect(items.where((Map<String, Object?> i) => i['type'] == 'event'),
        isEmpty,
        reason: 'analytics items with no identity are dropped at construction');
  });
}
