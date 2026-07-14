import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

void main() {
  const DartStackTraceParser parser = DartStackTraceParser();

  group('JIT / friendly traces', () {
    const String jit = '''
#0      loadUser (package:app/main.dart:42:13)
#1      main.<anonymous closure> (package:app/main.dart:10:3)
#2      _rootRun (dart:async/zone.dart:1399:13)
#3      _CustomZone.run (dart:async/zone.dart)
''';

    test('parses function, filename, line and column', () {
      final List<StackFrame> frames = parser.parse(jit);
      expect(frames, hasLength(4));

      final StackFrame first = frames.first;
      expect(first.function, 'loadUser');
      expect(first.filename, 'package:app/main.dart');
      expect(first.lineno, 42);
      expect(first.colno, 13);
      expect(first.inApp, isTrue);
    });

    test('marks dart: frames as not in-app', () {
      final List<StackFrame> frames = parser.parse(jit);
      final StackFrame rootRun = frames[2];
      expect(rootRun.function, '_rootRun');
      expect(rootRun.filename, 'dart:async/zone.dart');
      expect(rootRun.lineno, 1399);
      expect(rootRun.colno, 13);
      expect(rootRun.inApp, isFalse);
    });

    test('parses frames that omit line/column', () {
      final List<StackFrame> frames = parser.parse(jit);
      final StackFrame last = frames[3];
      expect(last.function, '_CustomZone.run');
      expect(last.filename, 'dart:async/zone.dart');
      expect(last.lineno, isNull);
      expect(last.colno, isNull);
    });

    test('parses a real StackTrace object', () {
      final List<StackFrame> frames = parser.parse(StackTrace.current);
      expect(frames, isNotEmpty);
    });
  });

  group('AOT / obfuscated traces', () {
    const String aot = '''
Warning: This VM has been configured to produce stack traces that violate the Dart standard.
*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
pid: 12345, tid: 6789, name 1.ui
build_id: 'a1b2c3d4e5f6'
isolate_dso_base: 7f0000000000
#00 abs 00007f0000001234 virt 0000000000001234 _kDartIsolateSnapshotInstructions+0x1234
#01 abs 00007f0000005678 virt 0000000000005678 _kDartIsolateSnapshotInstructions+0x5678
''';

    test('skips metadata noise and parses only frames', () {
      final List<StackFrame> frames = parser.parse(aot);
      expect(frames, hasLength(2));
    });

    test('preserves the raw program-counter symbol for symbolication', () {
      final List<StackFrame> frames = parser.parse(aot);
      expect(frames.first.function, contains('0x1234'));
      expect(frames.first.filename, isNull);
      expect(frames.first.lineno, isNull);
      expect(frames.first.inApp, isFalse);
    });

    test('recognizes AOT metadata lines as noise', () {
      expect(DartStackTraceParser.isNoise('build_id: \'abc\''), isTrue);
      expect(DartStackTraceParser.isNoise('pid: 1, tid: 2, name x'), isTrue);
      expect(
        DartStackTraceParser.isNoise('#0 loadUser (package:app/main.dart:1:2)'),
        isFalse,
      );
    });
  });

  group('edge cases', () {
    test('null and empty traces yield no frames', () {
      expect(parser.parse(null), isEmpty);
      expect(parser.parse(''), isEmpty);
      expect(parser.parse('   \n  \n'), isEmpty);
    });
  });
}
