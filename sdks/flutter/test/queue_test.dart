import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter/src/transport/queue.dart';

void main() {
  late Directory dir;

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_queue_test');
  });

  tearDown(() async {
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  group('FIFO ordering', () {
    test('preserves insertion order', () async {
      final EnvelopeQueue queue = EnvelopeQueue(directory: dir);
      await queue.enqueue('{"a":1}');
      await queue.enqueue('{"b":2}');
      await queue.enqueue('{"c":3}');

      expect(
        await queue.peekAll(),
        <String>['{"a":1}', '{"b":2}', '{"c":3}'],
      );
    });

    test('acknowledgeFirst removes the oldest entry', () async {
      final EnvelopeQueue queue = EnvelopeQueue(directory: dir);
      await queue.enqueue('{"a":1}');
      await queue.enqueue('{"b":2}');

      expect(await queue.peek(), '{"a":1}');
      await queue.acknowledgeFirst();
      expect(await queue.peek(), '{"b":2}');
      expect(queue.length, 1);
    });
  });

  group('byte cap with FIFO eviction', () {
    test('evicts oldest entries once the cap is exceeded', () async {
      // Each line is 10 chars + 1 newline = 11 bytes. Cap of 25 holds 2 lines.
      final EnvelopeQueue queue = EnvelopeQueue(directory: dir, maxBytes: 25);
      await queue.enqueue('A' * 10);
      await queue.enqueue('B' * 10);
      await queue.enqueue('C' * 10);

      final List<String> remaining = await queue.peekAll();
      expect(remaining, hasLength(2));
      expect(remaining.first, 'B' * 10); // oldest ('A') evicted
      expect(remaining.last, 'C' * 10);
      expect(queue.sizeInBytes, lessThanOrEqualTo(25));
    });

    test('always retains the newest entry even if it alone exceeds the cap',
        () async {
      final EnvelopeQueue queue = EnvelopeQueue(directory: dir, maxBytes: 5);
      await queue.enqueue('this-single-line-is-way-too-big');
      expect(queue.length, 1);
      expect(await queue.peek(), 'this-single-line-is-way-too-big');
    });
  });

  group('durability across restart', () {
    test('a fresh queue instance drains what a prior instance wrote', () async {
      final EnvelopeQueue writer = EnvelopeQueue(directory: dir);
      await writer.enqueue('{"x":1}');
      await writer.enqueue('{"y":2}');

      // Simulate an app restart: a brand new instance over the same directory.
      final EnvelopeQueue reader = EnvelopeQueue(directory: dir);
      await reader.load();

      expect(
        await reader.peekAll(),
        <String>['{"x":1}', '{"y":2}'],
      );
    });

    test('clear empties the persisted file', () async {
      final EnvelopeQueue queue = EnvelopeQueue(directory: dir);
      await queue.enqueue('{"x":1}');
      await queue.clear();

      final EnvelopeQueue reopened = EnvelopeQueue(directory: dir);
      await reopened.load();
      expect(reopened.isEmpty, isTrue);
    });
  });
}
