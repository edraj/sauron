import 'dart:convert';

import '../types.dart';

/// Parses Dart/Flutter stack traces into normalized [StackFrame]s.
///
/// Handles both:
///  * JIT / "friendly" traces: `#0  loadUser (package:app/main.dart:42:13)`
///  * AOT / obfuscated symbolic traces: `#00 abs 7f00.. virt 00.. symbol+0x12`
///
/// No symbolication is performed on-device; AOT frames preserve the raw
/// program-counter data for server-side symbolication.
class DartStackTraceParser {
  const DartStackTraceParser();

  // #0      functionName (package:app/main.dart:42:13)
  static final RegExp _jitWithColumn =
      RegExp(r'^#\d+\s+(.+?)\s+\((.+):(\d+):(\d+)\)$');

  // #0      functionName (package:app/main.dart:42)
  static final RegExp _jitWithLine =
      RegExp(r'^#\d+\s+(.+?)\s+\((.+):(\d+)\)$');

  // #0      functionName (dart:async/zone.dart)
  static final RegExp _jitNoPosition = RegExp(r'^#\d+\s+(.+?)\s+\((.+)\)$');

  // #00 abs 00007f0000001234 virt 0000000000001234 symbol+0x1234
  static final RegExp _aotFrame = RegExp(
    r'^#\d+\s+abs\s+([0-9a-fA-F]+)(?:\s+virt\s+[0-9a-fA-F]+)?\s*(.*)$',
  );

  /// Lines that are metadata, not frames, in AOT traces.
  static final RegExp _aotNoise = RegExp(
    r'^(warning:|pid:|build_id|isolate_dso_base|\*\*\*|os:|arch:|comp\.|Dwarf)',
    caseSensitive: false,
  );

  List<StackFrame> parse(Object? stackTrace) {
    if (stackTrace == null) {
      return const <StackFrame>[];
    }
    final String text = stackTrace.toString();
    if (text.trim().isEmpty) {
      return const <StackFrame>[];
    }
    final List<StackFrame> frames = <StackFrame>[];
    for (final String rawLine in const LineSplitter().convert(text)) {
      final String line = rawLine.trim();
      if (line.isEmpty) {
        continue;
      }
      final StackFrame? frame = _parseLine(line);
      if (frame != null) {
        frames.add(frame);
      }
    }
    return frames;
  }

  StackFrame? _parseLine(String line) {
    // AOT symbolic frame — check before generic patterns.
    final Match? aot = _aotFrame.firstMatch(line);
    if (aot != null) {
      final String address = aot.group(1) ?? '';
      final String symbol = (aot.group(2) ?? '').trim();
      return StackFrame(
        function: symbol.isNotEmpty ? symbol : 'abs $address',
        filename: null,
        inApp: false,
      );
    }

    final Match? withCol = _jitWithColumn.firstMatch(line);
    if (withCol != null) {
      final String file = withCol.group(2)!;
      return StackFrame(
        function: _cleanFunction(withCol.group(1)!),
        filename: file,
        lineno: int.tryParse(withCol.group(3)!),
        colno: int.tryParse(withCol.group(4)!),
        inApp: _isInApp(file),
      );
    }

    final Match? withLine = _jitWithLine.firstMatch(line);
    if (withLine != null) {
      final String file = withLine.group(2)!;
      return StackFrame(
        function: _cleanFunction(withLine.group(1)!),
        filename: file,
        lineno: int.tryParse(withLine.group(3)!),
        inApp: _isInApp(file),
      );
    }

    final Match? noPos = _jitNoPosition.firstMatch(line);
    if (noPos != null) {
      final String file = noPos.group(2)!;
      return StackFrame(
        function: _cleanFunction(noPos.group(1)!),
        filename: file,
        inApp: _isInApp(file),
      );
    }

    // Skip AOT metadata noise silently; drop anything else unrecognized.
    return null;
  }

  String _cleanFunction(String raw) {
    // Dart frames use `new ClassName` / `ClassName.method`; keep as-is but trim.
    return raw.trim();
  }

  bool _isInApp(String filename) {
    if (filename.startsWith('dart:')) {
      return false;
    }
    if (filename.startsWith('package:flutter/') ||
        filename.startsWith('package:flutter_test/') ||
        filename.startsWith('package:sauron_flutter/')) {
      return false;
    }
    // Application code is typically `package:<app>/...` or a `file://` path.
    return filename.startsWith('package:') || filename.startsWith('file:');
  }

  /// Whether [line] is AOT metadata rather than a frame (exposed for tests).
  static bool isNoise(String line) => _aotNoise.hasMatch(line.trim());
}
