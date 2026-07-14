import 'dart:convert';
import 'dart:io';

/// A durable, FIFO envelope queue backed by a single JSONL file.
///
/// Each line is one compact envelope JSON string. The queue survives app
/// restarts (it lives in the app-support directory) and enforces a byte cap by
/// evicting the oldest envelopes first. The newest envelope is always retained,
/// even if it alone exceeds the cap.
class EnvelopeQueue {
  EnvelopeQueue({
    required this.directory,
    this.fileName = 'queue.jsonl',
    this.maxBytes = 5 * 1024 * 1024,
  });

  /// Directory that holds the queue file (typically the app-support dir).
  final Directory directory;

  /// Queue file name within [directory].
  final String fileName;

  /// Maximum total serialized size before FIFO eviction kicks in.
  final int maxBytes;

  final List<String> _lines = <String>[];
  bool _loaded = false;

  File get _file => File('${directory.path}/$fileName');

  /// Loads existing entries from disk (idempotent). Call once on init to drain.
  Future<void> load() async {
    if (_loaded) {
      return;
    }
    _loaded = true;
    try {
      final File file = _file;
      if (await file.exists()) {
        final String contents = await file.readAsString();
        for (final String line in const LineSplitter().convert(contents)) {
          if (line.trim().isNotEmpty) {
            _lines.add(line);
          }
        }
      }
    } on Object {
      // A corrupt/unreadable queue file must never crash the host app.
      _lines.clear();
    }
  }

  /// Number of queued envelopes.
  int get length => _lines.length;

  /// Whether the queue is empty.
  bool get isEmpty => _lines.isEmpty;

  /// Total serialized byte size (including newline separators).
  int get sizeInBytes {
    int total = 0;
    for (final String line in _lines) {
      total += utf8.encode(line).length + 1; // +1 for the newline
    }
    return total;
  }

  /// Appends [envelopeJson] and persists, evicting oldest entries if needed.
  Future<void> enqueue(String envelopeJson) async {
    await load();
    _lines.add(envelopeJson);
    _enforceCap();
    await _persist();
  }

  /// Returns the oldest envelope without removing it, or `null` if empty.
  Future<String?> peek() async {
    await load();
    return _lines.isEmpty ? null : _lines.first;
  }

  /// Snapshot of all queued envelopes, oldest first.
  Future<List<String>> peekAll() async {
    await load();
    return List<String>.unmodifiable(_lines);
  }

  /// Removes the oldest envelope (after a successful send) and persists.
  Future<void> acknowledgeFirst() async {
    await load();
    if (_lines.isNotEmpty) {
      _lines.removeAt(0);
      await _persist();
    }
  }

  /// Removes every queued envelope and persists.
  Future<void> clear() async {
    await load();
    _lines.clear();
    await _persist();
  }

  void _enforceCap() {
    while (sizeInBytes > maxBytes && _lines.length > 1) {
      _lines.removeAt(0);
    }
  }

  Future<void> _persist() async {
    try {
      final File file = _file;
      await file.parent.create(recursive: true);
      final StringBuffer buffer = StringBuffer();
      for (final String line in _lines) {
        buffer
          ..write(line)
          ..write('\n');
      }
      await file.writeAsString(buffer.toString(), flush: true);
    } on Object {
      // Persistence failures are non-fatal; the in-memory copy still works.
    }
  }
}
