import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

/// The verbatim obfuscated-trace capture that feeds server-side Dart
/// symbolication. Detection, header parsing, and the error-item JSON shape are
/// asserted here; end-to-end address resolution is verified on the backend.
void main() {
  const String obfuscated = '''
*** *** ***
build_id: 'a1b2c3d4e5'
isolate_dso_base: 7f0000000000
    #00 abs 00007f0000001560 virt 0000000000001560 _kDartIsolateSnapshotInstructions+0x1560
    #01 abs 00007f0000001890 virt 0000000000001890 _kDartIsolateSnapshotInstructions+0x1890
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
