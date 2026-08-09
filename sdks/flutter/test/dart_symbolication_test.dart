import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

/// The verbatim obfuscated-trace capture that feeds server-side Dart
/// symbolication. Detection, header parsing, and the error-item JSON shape are
/// asserted here; end-to-end address resolution is verified on the backend.
void main() {
  // Carries `, vm_dso_base: ...` because real Dart AOT does. Without it this
  // fixture — and the backend's twin in dart_trace.rs — was unrealistic in the
  // same way on both sides of the wire, which let a parse bug that broke EVERY
  // real trace pass both suites. With it, the assertions below fail unless
  // DebugMeta.fromTrace keeps only the leading token.
  const String obfuscated = '''
*** *** ***
build_id: 'a1b2c3d4e5'
isolate_dso_base: 7f0000000000, vm_dso_base: 7f0000000000
    #00 abs 00007f0000001560 virt 0000000000001560 _kDartIsolateSnapshotInstructions+0x1560
    #01 abs 00007f0000001890 virt 0000000000001890 _kDartIsolateSnapshotInstructions+0x1890
''';

  /// Verbatim header captured from a real device (Redmi `camellia`, Android 13,
  /// Flutter 3.44.8) on 2026-08-08.
  const String realDeviceTrace = '''
*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
os: android arch: arm64 comp: yes sim: no
build_id: 'b7188509e5f19c541ab806422af8410e'
isolate_dso_base: 7b9c2b7000, vm_dso_base: 7b9c2b7000
isolate_instructions: 7b9c38da80, vm_instructions: 7b9c377000
    #00 abs 0000007b9c4bc9b7 virt 00000000002059b7 _kDartIsolateSnapshotInstructions+0x12ef37
''';

  const String readable = '''
#0      MyWidget.build (package:app/main.dart:42:5)
#1      StatelessElement.build (package:flutter/src/widgets/framework.dart:100)
''';

  test('detects obfuscated vs readable traces', () {
    expect(isObfuscatedDartTrace(obfuscated), isTrue);
    expect(isObfuscatedDartTrace(readable), isFalse);
  });

  test('parses build_id and dso_base from the header', () {
    final DebugMeta dm = DebugMeta.fromTrace(obfuscated, os: 'android');
    expect(dm.buildId, 'a1b2c3d4e5');
    expect(dm.isolateDsoBase, '7f0000000000');
    expect(dm.os, 'android');
  });

  test('real-device header stores a bare address, not the rest of the line', () {
    final DebugMeta dm = DebugMeta.fromTrace(realDeviceTrace, os: 'android');
    expect(dm.buildId, 'b7188509e5f19c541ab806422af8410e');
    // Regression: this used to be '7b9c2b7000, vm_dso_base: 7b9c2b7000', which
    // the backend's hex parse rejected, so dso_base was null for every real
    // trace and the `abs - dso_base` fallback could resolve none of them.
    expect(dm.isolateDsoBase, '7b9c2b7000');
    expect(dm.isolateDsoBase, isNot(contains('vm_dso_base')));
    expect(dm.isolateDsoBase, isNot(contains(',')));
  });

  test('error item carries raw_stacktrace + debug_meta when set', () {
    final ErrorItem item = ErrorItem(
      exception: const SauronException(
        type: 'StateError',
        value: 'boom',
        mechanism: Mechanism(type: 'flutterError', handled: false),
      ),
      timestamp: DateTime.utc(2026, 7, 15),
      rawStacktrace: obfuscated,
      debugMeta: DebugMeta.fromTrace(obfuscated),
    );
    final Map<String, Object?> json = item.toJson();
    expect(json['raw_stacktrace'], obfuscated);
    final Map<String, Object?> dm = json['debug_meta']! as Map<String, Object?>;
    expect(dm['build_id'], 'a1b2c3d4e5');
    expect(dm['isolate_dso_base'], '7f0000000000');
  });

  test('debug_meta is null for a readable trace item', () {
    final ErrorItem item = ErrorItem(
      exception: const SauronException(
        type: 'StateError',
        value: 'boom',
        mechanism: Mechanism(type: 'manual', handled: true),
      ),
      timestamp: DateTime.utc(2026, 7, 15),
    );
    expect(item.toJson()['debug_meta'], isNull);
    expect(item.toJson()['raw_stacktrace'], isNull);
  });
}
