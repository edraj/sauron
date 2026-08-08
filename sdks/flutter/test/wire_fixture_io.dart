import 'dart:convert';
import 'dart:io';

/// Writer for `sdks/wire-fixtures/<sdk>.json` — the envelopes the backend's
/// `cargo test -p sauron-core --test sdk_wire_conformance` feeds through the
/// REAL `serde` deserializer.
///
/// Two categories are pinned so regenerating is a NO-OP:
///
/// 1. the intrinsically dynamic fields (`timestamp`, `event_id`, …);
/// 2. everything the **toolchain** supplies rather than the SDK — stack-frame
///    identity strings (under `package:test` these are `declarer.dart` and
///    `invoker.dart`, the runner's own internals) and the host/runtime values in
///    `context.os` / `.runtime` / `.device`. Without this a Dart SDK bump
///    rewrote a committed file with no wire change at all, which makes a CI diff
///    gate noisy and leaves a tracked file dirty after a plain `flutter test`.
///
/// What is deliberately NOT normalized is the part that proves something: item
/// shape, key set, nullability, and the frame COUNT.
const String _timestamp = '2026-07-12T10:30:00.123Z';

const Map<String, String> _stringSubs = <String, String>{
  'timestamp': _timestamp,
  'sent_at': _timestamp,
  'event_id': '0123456789abcdef0123456789abcdef',
  'session_id': 'sess_fixture',
  'device_id': '3f2504e0-4f89-41d3-9a0c-0305e82c3301',
  'workflow_id': 'wf_fixture',
  'raw_stacktrace': '<normalized>',
  'build_id': '<normalized>',
  'isolate_dso_base': '<normalized>',
};

/// Stack-frame identity: where the test ran, not what the SDK emits.
const Map<String, String> _frameIdentity = <String, String>{
  'function': '<fn>',
  'module': '<module>',
  'filename': '<file>',
  'abs_path': '<file>',
};

/// `'<parent>.<key>'` paths carrying host- or runtime-derived values.
///
/// `context.device` / `.os` / `.runtime` are free-form `serde_json::Value` on
/// the wire, so their contents prove nothing — while `runtime.version` is the
/// Dart SDK version. `runtime.name` is left alone deliberately: it is an SDK
/// constant, not a host value.
const Set<String> _hostDerived = <String>{
  'os.name',
  'os.version',
  'runtime.version',
  'device.family',
  'device.model',
  'device.arch',
};

Object? _normalize(Object? node, [String key = '', String parentKey = '']) {
  if (node is Map) {
    return <String, Object?>{
      for (final MapEntry<Object?, Object?> e in node.entries)
        e.key as String: _normalize(e.value, e.key as String, key),
    };
  }
  if (node is List) {
    // List children keep the container's key AND its parent, so a frame inside
    // `stacktrace: [...]` is still seen as living under it.
    return node.map((Object? v) => _normalize(v, key, parentKey)).toList();
  }
  if (node is String) {
    if (_hostDerived.contains('$parentKey.$key')) {
      return '<host>';
    }
    final String? frame = _frameIdentity[key];
    if (frame != null) {
      return frame;
    }
    final String? sub = _stringSubs[key];
    if (sub != null) {
      return sub;
    }
    return node;
  }
  if (node is int) {
    if (key == 'lineno') {
      return 42;
    }
    if (key == 'colno') {
      return 13;
    }
  }
  // `null` falls through untouched: nullability is part of what the fixture
  // proves and must never be papered over with a placeholder.
  return node;
}

/// Locate `sdks/wire-fixtures/` by walking up from the test's working
/// directory, so this does not depend on where `flutter test` was invoked from.
File wireFixtureFile(String sdk) {
  Directory dir = Directory.current.absolute;
  for (int i = 0; i < 8; i++) {
    final Directory candidate =
        Directory('${dir.path}${Platform.pathSeparator}wire-fixtures');
    if (candidate.existsSync()) {
      return File('${candidate.path}${Platform.pathSeparator}$sdk.json');
    }
    final Directory parent = dir.parent;
    if (parent.path == dir.path) {
      break;
    }
    dir = parent;
  }
  throw StateError(
    'could not find sdks/wire-fixtures/ above ${Directory.current.path} — '
    'see sdks/wire-fixtures/README.md',
  );
}

/// Write one captured envelope as this SDK's committed wire fixture.
void writeWireFixture(String sdk, Map<String, Object?> envelope) {
  final File file = wireFixtureFile(sdk);
  file.parent.createSync(recursive: true);
  const JsonEncoder encoder = JsonEncoder.withIndent('  ');
  file.writeAsStringSync('${encoder.convert(_normalize(envelope))}\n');
}
