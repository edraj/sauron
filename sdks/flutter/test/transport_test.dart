import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';
import 'package:sauron_flutter/src/transport/queue.dart';
import 'package:sauron_flutter/src/transport/transport.dart';

class _MockClient extends Mock implements http.Client {}

void main() {
  late Directory dir;
  late _MockClient client;
  late EnvelopeQueue queue;
  late Dsn dsn;

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_transport_test');
    client = _MockClient();
    queue = EnvelopeQueue(directory: dir);
    dsn = Dsn.parse('https://pk_test@localhost:8081/1');
  });

  tearDown(() async {
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  SauronTransport buildTransport({SauronOptions? options}) {
    return SauronTransport(
      options: options ?? SauronOptions(),
      dsn: dsn,
      queue: queue,
      httpClient: client,
      headerBuilder: (DateTime sentAt) => EnvelopeHeader(
        dsn: dsn.toString(),
        sentAt: sentAt,
        environment: 'test',
      ),
      contextBuilder: () => const SauronContext(),
    );
  }

  void stubStatus(int status, {Map<String, String>? headers}) {
    when(() => client.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer(
      (_) async => http.Response('', status, headers: headers ?? const {}),
    );
  }

  test('202 success drains the queue', () async {
    stubStatus(202);
    final SauronTransport transport = buildTransport();
    transport.capture(EventItem(name: 'x', timestamp: DateTime.now().toUtc()));
    await transport.flush();

    verify(() => client.post(any(),
        headers: any(named: 'headers'), body: any(named: 'body'))).called(1);
    expect(await queue.peekAll(), isEmpty);
    transport.debugCancelTimers();
  });

  test('sends the correct auth header and endpoint', () async {
    stubStatus(202);
    final SauronTransport transport = buildTransport();
    transport.capture(EventItem(name: 'x', timestamp: DateTime.now().toUtc()));
    await transport.flush();

    final List<dynamic> captured = verify(() => client.post(
          captureAny(),
          headers: captureAny(named: 'headers'),
          body: any(named: 'body'),
        )).captured;
    final Uri uri = captured[0] as Uri;
    final Map<String, String> headers = captured[1] as Map<String, String>;

    expect(uri.toString(), 'https://localhost:8081/api/1/envelope');
    expect(headers['X-Sauron-Key'], 'pk_test');
    expect(headers['Content-Type'], 'application/json');
    transport.debugCancelTimers();
  });

  test('gzips large payloads and sets Content-Encoding', () async {
    stubStatus(202);
    final SauronTransport transport =
        buildTransport(options: SauronOptions()..gzipThresholdBytes = 16);
    transport.capture(
      EventItem(
        name: 'big',
        timestamp: DateTime.now().toUtc(),
        properties: <String, Object?>{
          'blob': List<int>.generate(500, (int i) => i).join(','),
        },
      ),
    );
    await transport.flush();

    final List<dynamic> captured = verify(() => client.post(
          any(),
          headers: captureAny(named: 'headers'),
          body: any(named: 'body'),
        )).captured;
    final Map<String, String> headers = captured[0] as Map<String, String>;
    expect(headers['Content-Encoding'], 'gzip');
    transport.debugCancelTimers();
  });

  test('5xx keeps the envelope queued for retry', () async {
    stubStatus(500);
    final SauronTransport transport = buildTransport();
    transport.capture(EventItem(name: 'x', timestamp: DateTime.now().toUtc()));
    await transport.flush();

    expect(await queue.peekAll(), hasLength(1)); // retained
    transport.debugCancelTimers();
  });

  test('400 drops the envelope without retry', () async {
    stubStatus(400);
    final SauronTransport transport = buildTransport();
    transport.capture(EventItem(name: 'x', timestamp: DateTime.now().toUtc()));
    await transport.flush();

    expect(await queue.peekAll(), isEmpty);
    transport.debugCancelTimers();
  });

  test('401 disables the transport and drops', () async {
    stubStatus(401);
    final SauronTransport transport = buildTransport();
    transport.capture(EventItem(name: 'x', timestamp: DateTime.now().toUtc()));
    await transport.flush();

    expect(transport.isEnabled, isFalse);
    expect(await queue.peekAll(), isEmpty);

    // Further captures are ignored once disabled.
    transport.capture(EventItem(name: 'y', timestamp: DateTime.now().toUtc()));
    expect(transport.bufferedItemCount, 0);
    transport.debugCancelTimers();
  });

  test('413 splits the envelope into smaller ones', () async {
    // First call returns 413, subsequent calls succeed.
    final List<int> statuses = <int>[413, 202, 202];
    int call = 0;
    when(() => client.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer((_) async {
      final int status = statuses[call.clamp(0, statuses.length - 1)];
      call++;
      return http.Response('', status);
    });

    final SauronTransport transport = buildTransport();
    transport
      ..capture(EventItem(name: 'a', timestamp: DateTime.now().toUtc()))
      ..capture(EventItem(name: 'b', timestamp: DateTime.now().toUtc()));
    await transport.flush();

    // The oversized envelope was split and the halves delivered.
    expect(await queue.peekAll(), isEmpty);
    expect(call, greaterThanOrEqualTo(3));
    transport.debugCancelTimers();
  });
}
