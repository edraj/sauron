import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, Object?>> items = <Map<String, Object?>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_transaction_test');
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
    await Sauron.close();
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  Future<void> initSauron() async {
    final SauronOptions options = SauronOptions()
      ..dsn = 'https://pk_test@localhost:9/1'
      ..httpClient = httpClient
      ..gzipThresholdBytes = 1 << 30;
    
    // We just need a client to capture items. We can pass appRunner: null
    await Sauron.init(options);
  }

  List<Map<String, Object?>> ofType(String type) =>
      items.where((Map<String, Object?> i) => i['type'] == type).toList();

  test('ActiveTransaction records a transaction with computed duration on end()', () async {
    await initSauron();

    final tx = Sauron.startTransaction(
      name: 'Test Transaction',
      op: 'task',
      httpMethod: 'POST',
    );

    // Simulate work
    await Future.delayed(const Duration(milliseconds: 50));

    tx.end(status: 'ok', httpStatus: 200);

    await Sauron.flush();

    final transactions = ofType('transaction');
    expect(transactions, hasLength(1));
    final item = transactions.first;
    expect(item['name'], 'Test Transaction');
    expect(item['op'], 'task');
    expect(item['status'], 'ok');
    expect(item['http_method'], 'POST');
    expect(item['http_status'], 200);
    expect((item['duration_ms'] as num) >= 50.0, isTrue, reason: 'duration should be >= 50ms');
  });

  test('ActiveTransaction uses default properties on end() if not provided', () async {
    await initSauron();

    final tx = Sauron.startTransaction(
      name: 'Default Transaction',
    );

    tx.end();

    await Sauron.flush();

    final transactions = ofType('transaction');
    expect(transactions, hasLength(1));
    final item = transactions.first;
    expect(item['name'], 'Default Transaction');
    expect(item['op'], 'custom');
    expect(item['status'], isNull);
  });

  test('ActiveTransaction can be cancelled with a reason', () async {
    await initSauron();

    final tx = Sauron.startTransaction(
      name: 'Cancelled Transaction',
    );

    tx.cancel('user_aborted');

    await Sauron.flush();

    final transactions = ofType('transaction');
    expect(transactions, hasLength(1));
    final item = transactions.first;
    expect(item['name'], 'Cancelled Transaction');
    expect(item['status'], 'cancelled: user_aborted');
  });

  test('ActiveTransaction can be cancelled without a reason', () async {
    await initSauron();

    final tx = Sauron.startTransaction(
      name: 'Cancelled Transaction 2',
    );

    tx.cancel();

    await Sauron.flush();

    final transactions = ofType('transaction');
    expect(transactions, hasLength(1));
    final item = transactions.first;
    expect(item['name'], 'Cancelled Transaction 2');
    expect(item['status'], 'cancelled');
  });

  test('ActiveTransaction ignores subsequent end() or cancel() calls', () async {
    await initSauron();

    final tx = Sauron.startTransaction(name: 'Only Once');
    tx.end();
    tx.end();
    tx.cancel();

    await Sauron.flush();

    final transactions = ofType('transaction');
    expect(transactions, hasLength(1));
  });
}
