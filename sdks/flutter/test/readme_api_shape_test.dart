import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

/// Compile-checks the exact API shapes the README documents.
///
/// Not a behaviour test — it exists because a README example is the one piece
/// of this SDK that nothing else compiles. Signatures drift, the docs keep
/// telling people to write code that no longer builds, and no suite notices.
void main() {
  test('README trackTransaction shape compiles', () {
    // The HTTP example's call shape.
    Sauron.trackTransaction(
      name: 'POST /orders',
      op: 'http',
      duration: const Duration(milliseconds: 842),
      httpMethod: 'POST',
      httpStatus: 201,
      url: 'https://api.example.com/orders',
      status: 'ok',
      tags: <String, String>{'api': 'orders', 'tier': 'premium'},
      extra: <String, Object?>{
        'request': '{"item_id":42}',
        'response': '{"order_id":9001}',
        'response_bytes': 17,
        'request_headers': <String>['content-type'],
      },
    );

    // The SQL example's call shape.
    Sauron.trackTransaction(
      name: 'SELECT orders',
      op: 'custom',
      duration: const Duration(milliseconds: 12),
      status: 'ok',
      tags: <String, String>{'db': 'sqflite', 'table': 'orders'},
      extra: <String, Object?>{
        'statement': 'SELECT id FROM orders WHERE user_id = ?',
        'row_count': 20,
        'params': <String, Object?>{'user_id': 'u_1'},
      },
    );
  });

  test('README ActiveTransaction mutable-field shape compiles', () {
    final ActiveTransaction tx = Sauron.startTransaction(
      name: 'POST /orders',
      op: 'http',
      httpMethod: 'POST',
      extra: <String, Object?>{'request': '{}'},
    );
    // The README tells people to mutate rather than pass a partial map to
    // `end()`, because `end()` replaces wholesale.
    tx.extra!['response'] = '{"order_id":9001}';
    tx.tags = <String, String>{'api': 'orders'};
    tx.end(status: 'ok', httpStatus: 201);
  });

  test('the documented cap constant is reachable from the public entrypoint', () {
    expect(kMaxTransactionExtraBytes, 16 * 1024);
    expect(
      capTransactionExtra(<String, Object?>{'a': 'x' * (kMaxTransactionExtraBytes + 1)}),
      containsPair('_truncated', true),
    );
  });
}
