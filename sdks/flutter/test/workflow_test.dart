import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// Workflows: named, explicitly-bounded spans of activity started via
/// `startWorkflow`/`endWorkflow`/`cancelWorkflow`. While one is active, its
/// id/name are stamped onto every captured error/event/transaction, and three
/// reserved lifecycle analytics events (`$workflow_start`/`$workflow_end`/
/// `$workflow_cancel`) are emitted through the client's own `track()`.
///
/// Uses the same mock-HTTP capture harness as `screen_test.dart`: posted
/// envelope bodies are decoded and their `items` collected so we can assert
/// exactly what left the device.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory dir;
  late _MockClient httpClient;
  final List<Map<String, Object?>> items = <Map<String, Object?>>[];

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_workflow_test');
    httpClient = _MockClient();
    items.clear();
    when(() => httpClient.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer((Invocation invocation) async {
      final Object? body = invocation.namedArguments[const Symbol('body')];
      final List<int> bytes =
          body is String ? utf8.encode(body) : body as List<int>;
      final Map<String, dynamic> env =
          jsonDecode(utf8.decode(bytes)) as Map<String, dynamic>;
      for (final dynamic item in env['items'] as List<dynamic>) {
        items.add((item as Map<String, dynamic>).cast<String, Object?>());
      }
      return http.Response('', 202);
    });
  });

  tearDown(() async {
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  Future<SauronClient> buildClient({BeforeSendCallback? beforeSend}) async {
    final SauronOptions options = SauronOptions()
      ..dsn = 'https://pk_test@localhost:9/1'
      ..httpClient = httpClient
      // Never gzip in tests so the posted body is plain JSON.
      ..gzipThresholdBytes = 1 << 30
      ..beforeSend = beforeSend;
    final SauronClient client = SauronClient(options);
    await client.bootstrap(queueDirectory: dir);
    return client;
  }

  List<Map<String, Object?>> events() =>
      items.where((Map<String, Object?> i) => i['type'] == 'event').toList();

  Map<String, Object?> eventNamed(String name) =>
      events().firstWhere((Map<String, Object?> e) => e['name'] == name);

  List<Map<String, Object?>> eventsNamed(String name) =>
      events().where((Map<String, Object?> e) => e['name'] == name).toList();

  test('start emits \$workflow_start stamped with the new workflow',
      () async {
    final SauronClient client = await buildClient();
    final WorkflowResult r = client.startWorkflow('checkout');
    expect(r.status, WorkflowStatus.ok);
    expect(r.workflowId, isNotNull);
    await client.flush();
    await client.close();

    final Map<String, Object?> item = eventNamed(r'$workflow_start');
    expect(item['workflow_id'], r.workflowId);
    expect(item['workflow_name'], 'checkout');
    final Map<String, Object?> properties =
        item['properties'] as Map<String, Object?>;
    expect(properties['workflow_id'], r.workflowId);
    expect(properties['workflow_name'], 'checkout');
  });

  test('stamps subsequent track, captureException and trackTransaction calls',
      () async {
    final SauronClient client = await buildClient();
    final WorkflowResult r = client.startWorkflow('checkout');
    client.track('tapped');
    client.captureException(StateError('boom'));
    client.trackTransaction(
      name: 'GET /users',
      duration: const Duration(milliseconds: 12),
    );
    // captureException fires its own unawaited flush; let it settle.
    await Future<void>.delayed(const Duration(milliseconds: 50));
    await client.flush();
    await client.close();

    final Map<String, Object?> tapped = eventNamed('tapped');
    expect(tapped['workflow_id'], r.workflowId);
    expect(tapped['workflow_name'], 'checkout');

    final Map<String, Object?> error =
        items.firstWhere((Map<String, Object?> i) => i['type'] == 'error');
    expect(error['workflow_id'], r.workflowId);
    expect(error['workflow_name'], 'checkout');

    final Map<String, Object?> transaction = items
        .firstWhere((Map<String, Object?> i) => i['type'] == 'transaction');
    expect(transaction['workflow_id'], r.workflowId);
    expect(transaction['workflow_name'], 'checkout');
  });

  test('omits both keys on error/event/transaction when no workflow is active',
      () async {
    final SauronClient client = await buildClient();
    client.track('plain');
    client.captureException(StateError('boom'));
    client.trackTransaction(
      name: 'GET /users',
      duration: const Duration(milliseconds: 12),
    );
    await Future<void>.delayed(const Duration(milliseconds: 50));
    await client.flush();
    await client.close();

    final Map<String, Object?> event = eventNamed('plain');
    final Map<String, Object?> error =
        items.firstWhere((Map<String, Object?> i) => i['type'] == 'error');
    final Map<String, Object?> transaction = items
        .firstWhere((Map<String, Object?> i) => i['type'] == 'transaction');

    for (final Map<String, Object?> item in <Map<String, Object?>>[
      event,
      error,
      transaction,
    ]) {
      // `containsKey`, not a null-equality check — a regression that emits
      // `workflow_id: null` instead of omitting the key must fail this.
      expect(item.containsKey('workflow_id'), isFalse);
      expect(item.containsKey('workflow_name'), isFalse);
    }
  });

  test('start while active returns alreadyActive and emits nothing', () async {
    final SauronClient client = await buildClient();
    final WorkflowResult first = client.startWorkflow('checkout');
    final WorkflowResult second = client.startWorkflow('refund');
    expect(second.status, WorkflowStatus.alreadyActive);
    expect(second.workflowId, isNull);
    expect(client.workflow?.workflowId, first.workflowId);
    expect(client.workflow?.name, 'checkout');
    await client.flush();
    await client.close();

    expect(eventsNamed(r'$workflow_start'), hasLength(1));
    expect(eventsNamed(r'$workflow_cancel'), isEmpty);
  });

  test('force cancels with reason superseded then starts the new one',
      () async {
    final SauronClient client = await buildClient();
    final WorkflowResult first = client.startWorkflow('checkout');
    final WorkflowResult second =
        client.startWorkflow('refund', force: true);
    expect(second.status, WorkflowStatus.ok);
    expect(second.workflowId, isNot(first.workflowId));
    expect(client.workflow?.workflowId, second.workflowId);
    expect(client.workflow?.name, 'refund');
    await client.flush();
    await client.close();

    final Map<String, Object?> cancelled = eventNamed(r'$workflow_cancel');
    expect(cancelled['workflow_id'], first.workflowId);
    expect(cancelled['workflow_name'], 'checkout');
    final Map<String, Object?> cancelProps =
        cancelled['properties'] as Map<String, Object?>;
    expect(cancelProps['reason'], 'superseded');

    // Two $workflow_start events exist now (checkout, then refund) — take
    // the later one.
    final Map<String, Object?> started = eventsNamed(r'$workflow_start').last;
    expect(started['workflow_id'], second.workflowId);
    expect(started['workflow_name'], 'refund');
  });

  test('end emits \$workflow_end with duration_ms and clears the field',
      () async {
    final SauronClient client = await buildClient();
    final WorkflowResult started = client.startWorkflow('checkout');
    await Future<void>.delayed(const Duration(milliseconds: 5));
    final WorkflowResult ended = client.endWorkflow();
    expect(ended.status, WorkflowStatus.ok);
    expect(ended.workflowId, started.workflowId);
    expect(client.workflow, isNull);
    await client.flush();
    await client.close();

    final Map<String, Object?> item = eventNamed(r'$workflow_end');
    expect(item['workflow_id'], started.workflowId);
    expect(item['workflow_name'], 'checkout');
    final Map<String, Object?> properties =
        item['properties'] as Map<String, Object?>;
    expect(properties['duration_ms'], isA<int>());
    expect((properties['duration_ms'] as int) >= 0, isTrue);
    expect(properties.containsKey('reason'), isFalse);
  });

  test('end with a mismatched name is a no-op returning nameMismatch',
      () async {
    final SauronClient client = await buildClient();
    client.startWorkflow('checkout');
    final WorkflowResult result = client.endWorkflow('refund');
    expect(result.status, WorkflowStatus.nameMismatch);
    expect(client.workflow?.name, 'checkout');
    await client.flush();
    await client.close();

    expect(eventsNamed(r'$workflow_end'), isEmpty);
  });

  test('end with none active returns notActive', () async {
    final SauronClient client = await buildClient();
    final WorkflowResult result = client.endWorkflow();
    expect(result.status, WorkflowStatus.notActive);
    await client.close();
  });

  test('cancel defaults reason to user and caps a long reason at 120 chars',
      () async {
    final SauronClient client = await buildClient();
    client.startWorkflow('a');
    final WorkflowResult firstCancel = client.cancelWorkflow();
    expect(firstCancel.status, WorkflowStatus.ok);

    client.startWorkflow('b');
    final String longReason = 'x' * 200;
    client.cancelWorkflow(null, longReason);
    await client.flush();
    await client.close();

    final List<Map<String, Object?>> cancels = eventsNamed(r'$workflow_cancel');
    expect(cancels, hasLength(2));
    final Map<String, Object?> defaulted =
        (cancels[0]['properties'] as Map<String, Object?>);
    expect(defaulted['reason'], 'user');
    final Map<String, Object?> capped =
        (cancels[1]['properties'] as Map<String, Object?>);
    expect((capped['reason'] as String).length, 120);
    expect(capped['reason'], 'x' * 120);
  });

  test('cancel with a mismatched name is a no-op returning nameMismatch',
      () async {
    final SauronClient client = await buildClient();
    client.startWorkflow('checkout');
    final WorkflowResult result = client.cancelWorkflow('refund');
    expect(result.status, WorkflowStatus.nameMismatch);
    expect(client.workflow?.name, 'checkout');
    await client.close();
  });

  test('rejects empty and over-long names, mutating nothing', () async {
    final SauronClient client = await buildClient();
    expect(client.startWorkflow('').status, WorkflowStatus.invalidName);
    expect(client.startWorkflow('   ').status, WorkflowStatus.invalidName);
    expect(
      client.startWorkflow('x' * 121).status,
      WorkflowStatus.invalidName,
    );
    expect(client.workflow, isNull);

    // Exactly at the cap is valid.
    expect(client.startWorkflow('x' * 120).status, WorkflowStatus.ok);
    await client.close();
  });

  test('client.workflow reflects the current workflow', () async {
    final SauronClient client = await buildClient();
    expect(client.workflow, isNull);
    final WorkflowResult r = client.startWorkflow('checkout');
    expect(client.workflow?.workflowId, r.workflowId);
    expect(client.workflow?.name, 'checkout');
    client.endWorkflow();
    expect(client.workflow, isNull);
    await client.close();
  });

  test('after close(), startWorkflow returns disabled and does not throw',
      () async {
    final SauronClient client = await buildClient();
    await client.close();

    expect(
      () => client.startWorkflow('checkout'),
      returnsNormally,
    );
    final WorkflowResult result = client.startWorkflow('checkout');
    expect(result.status, WorkflowStatus.disabled);
    expect(client.workflow, isNull);
  });

  test('after close(), endWorkflow/cancelWorkflow return disabled and do not throw',
      () async {
    final SauronClient client = await buildClient();
    client.startWorkflow('checkout');
    await client.close();

    expect(client.endWorkflow().status, WorkflowStatus.disabled);
    expect(client.cancelWorkflow().status, WorkflowStatus.disabled);
  });

  test('a 401 disables the transport, and startWorkflow then returns disabled',
      () async {
    final SauronClient client = await buildClient();
    // The gateway now rejects the key mid-session (revoked/rotated).
    when(() => httpClient.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer((_) async => http.Response('', 401));

    client.track('anything');
    await client.flush(); // drains → 401 → the transport disables itself

    // Without this, startWorkflow would mint an id, set local state, and have
    // its $workflow_start silently dropped by the disabled transport —
    // leaving client.workflow reporting a workflow the server never heard of.
    expect(client.isEnabled, isFalse);
    final WorkflowResult result = client.startWorkflow('checkout');
    expect(result.status, WorkflowStatus.disabled);
    expect(result.workflowId, isNull);
    expect(client.workflow, isNull);
    await client.close();
  });

  test('endWorkflow returns ok and still clears state when the emit throws',
      () async {
    final SauronClient client = await buildClient(
      beforeSend: (Object item) {
        if (item is EventItem && item.name == r'$workflow_end') {
          throw StateError('beforeSend blew up on the closing emit');
        }
        return item;
      },
    );
    final WorkflowResult started = client.startWorkflow('checkout');
    expect(started.status, WorkflowStatus.ok);

    final WorkflowResult ended = client.endWorkflow();
    // `disabled` here would be a lie — from the caller's point of view the
    // workflow IS closed. And the state must not be left stuck active.
    expect(ended.status, WorkflowStatus.ok);
    expect(ended.workflowId, started.workflowId);
    expect(client.workflow, isNull);
    await client.close();
  });

  test('startWorkflow returns ok with the id when the start emit throws',
      () async {
    final SauronClient client = await buildClient(
      beforeSend: (Object item) {
        if (item is EventItem && item.name == r'$workflow_start') {
          throw StateError('beforeSend blew up on the start emit');
        }
        return item;
      },
    );
    final WorkflowResult started = client.startWorkflow('checkout');
    // The workflow IS live locally; the server materializes it from the next
    // stamped event. A lost $workflow_start is recoverable, a lost id is not.
    expect(started.status, WorkflowStatus.ok);
    expect(started.workflowId, isNotNull);
    expect(client.workflow?.workflowId, started.workflowId);
    await client.close();
  });

  test('a throwing supersede emit still starts the replacement workflow',
      () async {
    final SauronClient client = await buildClient(
      beforeSend: (Object item) {
        if (item is EventItem && item.name == r'$workflow_cancel') {
          throw StateError('beforeSend blew up on the superseding cancel');
        }
        return item;
      },
    );
    final WorkflowResult first = client.startWorkflow('checkout');
    final WorkflowResult second = client.startWorkflow('refund', force: true);
    // Never `disabled`, and never left pointing at the superseded workflow.
    expect(second.status, WorkflowStatus.ok);
    expect(second.workflowId, isNot(first.workflowId));
    expect(client.workflow?.workflowId, second.workflowId);
    expect(client.workflow?.name, 'refund');
    await client.close();
  });
}
