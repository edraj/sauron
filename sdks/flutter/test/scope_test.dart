import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter/src/scope.dart';

void main() {
  group('Scope metadata', () {
    test('setters accumulate tags/contexts/extra', () {
      final Scope scope = Scope();
      scope.setTag('a', '1');
      scope.setTags(<String, String>{'b': '2', 'c': '3'});
      scope.setContext('order', <String, Object?>{'id': 7});
      scope.setExtra('flag', true);

      expect(scope.tags, <String, String>{'a': '1', 'b': '2', 'c': '3'});
      expect(scope.contexts,
          <String, Map<String, Object?>>{'order': <String, Object?>{'id': 7}});
      expect(scope.extra, <String, Object?>{'flag': true});
    });

    test('setTag is last-write-wins per key', () {
      final Scope scope = Scope();
      scope.setTag('env', 'seed');
      scope.setTag('env', 'runtime');
      expect(scope.tags['env'], 'runtime');
    });

    test('setContext replaces the whole block by name', () {
      final Scope scope = Scope();
      scope.setContext('order', <String, Object?>{'id': 1, 'total': 10});
      scope.setContext('order', <String, Object?>{'id': 2});
      expect(scope.contexts['order'], <String, Object?>{'id': 2});
    });

    test('fresh scope has empty metadata maps', () {
      final Scope scope = Scope();
      expect(scope.tags, isEmpty);
      expect(scope.contexts, isEmpty);
      expect(scope.extra, isEmpty);
    });
  });
}
