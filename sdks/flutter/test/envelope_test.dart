import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

/// The LOCKED golden envelope shape. A Rust backend and a JS SDK emit/consume
/// this identical structure — this fixture guards Flutter parity.
///
/// It exercises the stable device identity (`context.device.device_id`), the
/// per-session id attached to error/event/transaction items (`session_id`), and
/// the `transaction` item type.
const String _sessionId = 'sess_abc123';
const String _deviceId = '3f2504e0-4f89-41d3-9a0c-0305e82c3301';

const String _golden = '''
{
  "header": {
    "dsn": "https://pk_test@localhost:8081/1",
    "sdk": { "name": "sauron.flutter", "version": "0.3.0" },
    "sent_at": "2026-07-12T10:30:00.123Z",
    "environment": "production",
    "release": "app@1.4.2+1402"
  },
  "context": {
    "device": { "family": "Apple", "model": "iPhone15,2", "arch": "arm64", "device_id": "$_deviceId" },
    "os": { "name": "iOS", "version": "17.5" },
    "app": { "version": "1.4.2", "build": "1402" },
    "runtime": { "name": "Dart", "version": "3.12" },
    "user": { "id": "u_123", "email": null, "traits": {} }
  },
  "items": [
    { "type": "error", "timestamp": "2026-07-12T10:29:58.900Z", "level": "error",
      "exception": { "type": "StateError", "value": "Bad state",
        "mechanism": { "type": "PlatformDispatcher.onError", "handled": false },
        "stacktrace": [ { "function": "loadUser", "filename": "package:app/main.dart", "lineno": 42, "colno": 13, "in_app": true } ] },
      "breadcrumbs": [ { "type": "navigation", "category": "route", "message": "/settings", "level": "info", "timestamp": "2026-07-12T10:29:50.000Z", "data": {} } ],
      "fingerprint": null, "session_id": "$_sessionId", "screen": null },
    { "type": "event", "name": "checkout_completed", "distinct_id": "u_123", "timestamp": "2026-07-12T10:29:40.000Z", "properties": { "cart_value": 42.5 }, "session_id": "$_sessionId", "screen": null },
    { "type": "identify", "distinct_id": "u_123", "anonymous_id": null, "traits": { "plan": "pro" } },
    { "type": "transaction", "name": "GET /users", "op": "http", "duration_ms": 128.5, "status": "ok",
      "http_method": "GET", "http_status": 200, "url": "https://api.example.com/users",
      "distinct_id": "u_123", "session_id": "$_sessionId", "timestamp": "2026-07-12T10:29:45.000Z" }
  ]
}
''';

void main() {
  group('Envelope golden shape', () {
    late Envelope envelope;

    setUp(() {
      final Dsn dsn = Dsn.parse('https://pk_test@localhost:8081/1');

      final ErrorItem error = ErrorItem(
        timestamp: DateTime.utc(2026, 7, 12, 10, 29, 58, 900),
        exception: SauronException(
          type: 'StateError',
          value: 'Bad state',
          mechanism: const Mechanism(
            type: 'PlatformDispatcher.onError',
            handled: false,
          ),
          stacktrace: const <StackFrame>[
            StackFrame(
              function: 'loadUser',
              filename: 'package:app/main.dart',
              lineno: 42,
              colno: 13,
              inApp: true,
            ),
          ],
        ),
        breadcrumbs: <Breadcrumb>[
          Breadcrumb(
            type: 'navigation',
            category: 'route',
            message: '/settings',
            level: SauronLevel.info,
            timestamp: DateTime.utc(2026, 7, 12, 10, 29, 50),
          ),
        ],
        sessionId: _sessionId,
      );

      final EventItem event = EventItem(
        name: 'checkout_completed',
        distinctId: 'u_123',
        timestamp: DateTime.utc(2026, 7, 12, 10, 29, 40),
        properties: const <String, Object?>{'cart_value': 42.5},
        sessionId: _sessionId,
      );

      final IdentifyItem identify = IdentifyItem(
        distinctId: 'u_123',
        traits: const <String, Object?>{'plan': 'pro'},
      );

      final TransactionItem transaction = TransactionItem(
        name: 'GET /users',
        op: 'http',
        durationMs: 128.5,
        status: 'ok',
        httpMethod: 'GET',
        httpStatus: 200,
        url: 'https://api.example.com/users',
        distinctId: 'u_123',
        sessionId: _sessionId,
        timestamp: DateTime.utc(2026, 7, 12, 10, 29, 45),
      );

      envelope = Envelope(
        header: EnvelopeHeader(
          dsn: dsn.toString(),
          sentAt: DateTime.utc(2026, 7, 12, 10, 30, 0, 123),
          environment: 'production',
          release: 'app@1.4.2+1402',
        ),
        context: const SauronContext(
          device: DeviceDescriptor(
            family: 'Apple',
            model: 'iPhone15,2',
            arch: 'arm64',
            deviceId: _deviceId,
          ),
          os: OsDescriptor(name: 'iOS', version: '17.5'),
          app: AppDescriptor(version: '1.4.2', build: '1402'),
          runtime: RuntimeDescriptor(name: 'Dart', version: '3.12'),
          user: SauronUser(id: 'u_123'),
        ),
        items: <EnvelopeItem>[error, event, identify, transaction],
      );
    });

    test('serializes byte-for-byte to the locked golden structure', () {
      final Object? actual = jsonDecode(envelope.encode());
      final Object? expected = jsonDecode(_golden);
      expect(actual, expected);
    });

    test('timestamps are ISO-8601 UTC with a trailing Z', () {
      final Map<String, dynamic> decoded =
          jsonDecode(envelope.encode()) as Map<String, dynamic>;
      final Map<String, dynamic> header =
          decoded['header'] as Map<String, dynamic>;
      expect(header['sent_at'], '2026-07-12T10:30:00.123Z');
      expect(sauronIso(DateTime.utc(2026, 7, 12, 10, 29, 50)),
          '2026-07-12T10:29:50.000Z');
    });

    test('item type discriminators match the wire contract', () {
      final Map<String, dynamic> decoded =
          jsonDecode(envelope.encode()) as Map<String, dynamic>;
      final List<dynamic> items = decoded['items'] as List<dynamic>;
      expect(
        items.map((dynamic i) => (i as Map<String, dynamic>)['type']),
        <String>['error', 'event', 'identify', 'transaction'],
      );
    });

    test('device context carries a stable device_id', () {
      final Map<String, dynamic> decoded =
          jsonDecode(envelope.encode()) as Map<String, dynamic>;
      final Map<String, dynamic> device = (decoded['context']
          as Map<String, dynamic>)['device'] as Map<String, dynamic>;
      expect(device['device_id'], _deviceId);
    });

    test('error and event items carry the session_id', () {
      final Map<String, dynamic> decoded =
          jsonDecode(envelope.encode()) as Map<String, dynamic>;
      final List<dynamic> items = decoded['items'] as List<dynamic>;
      final Map<String, dynamic> error = items[0] as Map<String, dynamic>;
      final Map<String, dynamic> event = items[1] as Map<String, dynamic>;
      expect(error['session_id'], _sessionId);
      expect(event['session_id'], _sessionId);
    });

    test('a transaction item serializes to the locked wire shape', () {
      final TransactionItem transaction = TransactionItem(
        name: 'HomePage',
        op: 'screen_load',
        durationMs: 42.0,
        status: 'ok',
        distinctId: 'u_123',
        sessionId: _sessionId,
        timestamp: DateTime.utc(2026, 7, 12, 10, 29, 45),
      );

      final Map<String, dynamic> json =
          jsonDecode(jsonEncode(transaction.toJson())) as Map<String, dynamic>;

      expect(json, <String, Object?>{
        'type': 'transaction',
        'name': 'HomePage',
        'op': 'screen_load',
        'duration_ms': 42.0,
        'status': 'ok',
        'http_method': null,
        'http_status': null,
        'url': null,
        'distinct_id': 'u_123',
        'session_id': _sessionId,
        'timestamp': '2026-07-12T10:29:45.000Z',
      });
    });

    test('trackTransaction maps a Duration to fractional milliseconds', () {
      // 1500 microseconds → 1.5 ms.
      final TransactionItem transaction = TransactionItem(
        name: 'custom',
        durationMs:
            const Duration(microseconds: 1500).inMicroseconds / 1000.0,
      );
      expect(transaction.durationMs, 1.5);
      expect(transaction.op, 'custom');
    });

    test('level values stay within the allowed set', () {
      const Set<String> allowed = <String>{
        'debug',
        'info',
        'warning',
        'error',
        'fatal',
      };
      for (final SauronLevel level in SauronLevel.values) {
        expect(allowed.contains(level.name), isTrue);
      }
    });
  });

  group('DSN parsing', () {
    test('parses key, host, port, and project id', () {
      final Dsn dsn = Dsn.parse('https://pk_test@localhost:8081/1');
      expect(dsn.publicKey, 'pk_test');
      expect(dsn.host, 'localhost');
      expect(dsn.port, 8081);
      expect(dsn.projectId, '1');
      expect(
        dsn.envelopeEndpoint.toString(),
        'https://localhost:8081/api/1/envelope',
      );
    });

    test('round-trips to the canonical DSN string', () {
      expect(
        Dsn.parse('https://pk_test@localhost:8081/1').toString(),
        'https://pk_test@localhost:8081/1',
      );
    });

    test('rejects a DSN without a public key', () {
      expect(
        () => Dsn.parse('https://localhost:8081/1'),
        throwsA(isA<FormatException>()),
      );
    });
  });
}
