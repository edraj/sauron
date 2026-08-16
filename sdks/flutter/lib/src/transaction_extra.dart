import 'dart:convert';

/// Largest serialized `extra` a single transaction may carry, in bytes.
///
/// Transactions are the highest-volume signal and they ship in BATCHED
/// envelopes, so one oversized payload does not fail alone — ingest rejects the
/// whole envelope past `INGEST_MAX_BODY_BYTES` (1 MiB by default) and every
/// unrelated span batched with it is lost. Since the motivating use of
/// transaction `extra` is request and response bodies, that is not a remote
/// hazard.
///
/// Kept identical across all five SDKs. If it moves, it moves everywhere.
const int kMaxTransactionExtraBytes = 16 * 1024;

/// Caps a transaction's `extra`, substituting a marker when it is too large.
///
/// Replaces the WHOLE map rather than trimming keys: a half-written JSON value
/// is worse than an honest marker, and per-key trimming would make the result
/// depend on key iteration order, which differs across the five SDKs. The
/// marker is deliberately readable on the dashboard — `_truncated` says data
/// was dropped rather than silently serving a short object that looks complete.
///
/// A value that cannot be encoded at all (a cycle, an arbitrary object)
/// becomes the same marker with `_bytes: -1`, because the alternative is
/// throwing from inside `trackTransaction` — and an SDK that crashes the app it
/// is measuring is worse than one that drops a payload. That case is not
/// hypothetical in Dart: `jsonEncode` throws on any type it has no encoder for,
/// and a developer attaching a model object rather than its `toJson()` is the
/// obvious mistake.
Map<String, Object?> capTransactionExtra(
  Map<String, Object?> extra, {
  int maxBytes = kMaxTransactionExtraBytes,
}) {
  final int size;
  try {
    // `utf8.encode(jsonEncode(...))` rather than `jsonEncode(...).length`:
    // the latter counts UTF-16 code units, undercounting every non-ASCII byte —
    // which is exactly what a response body full of user text is made of.
    size = utf8.encode(jsonEncode(extra)).length;
  } catch (_) {
    return <String, Object?>{'_truncated': true, '_bytes': -1};
  }
  if (size <= maxBytes) {
    return extra;
  }
  return <String, Object?>{'_truncated': true, '_bytes': size};
}
