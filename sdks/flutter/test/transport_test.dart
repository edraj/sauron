import 'dart:async';
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

  /// Polls [condition] every millisecond, failing after [timeout].
  Future<void> untilTrue(
    bool Function() condition, {
    Duration timeout = const Duration(seconds: 5),
  }) async {
    final Stopwatch clock = Stopwatch()..start();
    while (!condition()) {
      if (clock.elapsed > timeout) {
        fail('condition not met within $timeout');
      }
      await Future<void>.delayed(const Duration(milliseconds: 1));
    }
  }

  /// Whether [future] completes within a short grace period.
  ///
  /// Used to assert a future is still PENDING. A correct transport can never
  /// resolve the futures under test while the held request is open, so the
  /// grace period only bounds how long a regression takes to show up.
  Future<bool> settles(Future<void> future) => Future.any(<Future<bool>>[
        future.then((_) => true, onError: (Object _) => true),
        Future<bool>.delayed(const Duration(milliseconds: 200), () => false),
      ]);

  /// Stubs `post` so the FIRST request stays open until the returned completer
  /// is completed; every later request is accepted at once. [posts] counts
  /// requests so a test can wait until the held one has actually been reached.
  Completer<http.Response> holdFirstPost(List<int> posts) {
    final Completer<http.Response> first = Completer<http.Response>();
    when(() => client.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer((_) {
      posts.add(1);
      return posts.length == 1
          ? first.future
          : Future<http.Response>.value(http.Response('', 202));
    });
    return first;
  }

  // `captureException` starts a flush it does not await. A `flush()` or
  // `close()` that lands while that drain is mid-send used to return at once:
  // the envelope it had just persisted was left for the running drain to find
  // later, and `close()` shut the HTTP client under the send. CI caught it as a
  // transaction captured right after an error being "delivered" during the
  // NEXT test. The first request is held open so "in flight" is a fact here,
  // not a race.
  test(
      'flush() waits for a drain already in flight, then attempts what was '
      'queued behind it', () async {
    final List<int> posts = <int>[];
    final Completer<http.Response> firstPost = holdFirstPost(posts);
    final SauronTransport transport = buildTransport();

    transport.capture(EventItem(name: 'a', timestamp: DateTime.now().toUtc()));
    final Future<void> eager = transport.flush();
    await untilTrue(() => posts.length == 1);

    transport.capture(EventItem(name: 'b', timestamp: DateTime.now().toUtc()));
    final Future<void> awaited = transport.flush();
    expect(await settles(awaited), isFalse,
        reason:
            'flush() resolved while the first envelope was still in flight');

    firstPost.complete(http.Response('', 202));
    await awaited;
    await eager;
    expect(posts, hasLength(2));
    expect(await queue.peekAll(), isEmpty);
    transport.debugCancelTimers();
  });

  test('close() waits for a drain already in flight before closing the client',
      () async {
    final List<int> posts = <int>[];
    final Completer<http.Response> firstPost = holdFirstPost(posts);
    final SauronTransport transport = buildTransport();

    transport.capture(EventItem(name: 'a', timestamp: DateTime.now().toUtc()));
    final Future<void> eager = transport.flush();
    await untilTrue(() => posts.length == 1);

    final Future<void> closing = transport.close();
    expect(await settles(closing), isFalse,
        reason: 'close() resolved with a request still in flight');

    firstPost.complete(http.Response('', 202));
    await closing;
    await eager;
    expect(posts, hasLength(1));
    expect(await queue.peekAll(), isEmpty);
  });
}
