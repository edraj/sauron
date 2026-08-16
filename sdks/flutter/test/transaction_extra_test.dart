import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter/src/envelope.dart';
import 'package:sauron_flutter/src/transaction_extra.dart';

/// The cap on a transaction's `extra`.
///
/// Worth its own suite because the failure it prevents is invisible from the
/// outside: transactions ship in BATCHED envelopes, and ingest rejects the
/// whole envelope past `INGEST_MAX_BODY_BYTES`. One oversized response body
/// does not lose one span — it loses every unrelated span batched alongside it,
/// with a 400 the transport drops without retrying.
void main() {
  group('capTransactionExtra', () {
    test('passes a small payload through unchanged', () {
      final Map<String, Object?> extra = <String, Object?>{
        'request': '{"page":1}',
        'retries': 2,
      };
      expect(identical(capTransactionExtra(extra), extra), isTrue);
    });

    test('replaces an oversized payload with a truncation marker', () {
      final Map<String, Object?> capped = capTransactionExtra(<String, Object?>{
        'response': 'x' * (kMaxTransactionExtraBytes + 1),
      });
      expect(capped['_truncated'], isTrue);
      expect(capped['_bytes'] as int, greaterThan(kMaxTransactionExtraBytes));
      // The whole map goes, not just the offending key.
      expect(capped.containsKey('response'), isFalse);
    });

    test('measures UTF-8 BYTES, not code units', () {
      // Under the cap by code-unit count, over it by bytes. Measured with
      // `String.length` the envelope is ~2x the size the SDK believed it was
      // sending.
      final Map<String, Object?> capped = capTransactionExtra(<String, Object?>{
        'body': 'é' * (kMaxTransactionExtraBytes - 100),
      });
      expect(capped['_truncated'], isTrue);
    });

    test('marks an unencodable payload rather than throwing', () {
      // `jsonEncode` throws on any type it has no encoder for, and attaching a
      // model object rather than its `toJson()` is the obvious mistake. An SDK
      // that crashes the app it is measuring is worse than one that drops a
      // payload.
      final Map<String, Object?> capped = capTransactionExtra(<String, Object?>{
        'model': Object(),
      });
      expect(capped['_truncated'], isTrue);
      expect(capped['_bytes'], -1);
    });

    test('uses the same limit as every other SDK', () {
      expect(kMaxTransactionExtraBytes, 16 * 1024);
    });
  });

  group('TransactionItem wire shape', () {
    TransactionItem item({
      Map<String, String>? tags,
      Map<String, Object?>? extra,
    }) =>
        TransactionItem(
          name: '/x',
          durationMs: 1,
          tags: tags,
          extra: extra,
        );

    test('omits tags and extra when null', () {
      final Map<String, Object?> json = item().toJson();
      expect(json.containsKey('tags'), isFalse);
      expect(json.containsKey('extra'), isFalse);
    });

    test('omits them when empty', () {
      final Map<String, Object?> json = item(
        tags: <String, String>{},
        extra: <String, Object?>{},
      ).toJson();
      // Absent, not `null`: a null deserializes into the backend's
      // `serde_json::Value` as `Value::Null`, which is exactly what the
      // pipeline's `object_or_empty` guard exists to never store.
      expect(json.containsKey('tags'), isFalse);
      expect(json.containsKey('extra'), isFalse);
    });

    test('emits them when set', () {
      final Map<String, Object?> json = item(
        tags: <String, String>{'tier': 'premium'},
        extra: <String, Object?>{'request': '{}'},
      ).toJson();
      expect(json['tags'], <String, String>{'tier': 'premium'});
      expect(json['extra'], <String, Object?>{'request': '{}'});
    });
  });
}
