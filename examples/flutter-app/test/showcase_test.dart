import 'dart:math';

import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter_demo/showcase.dart';

/// A recording fake sink — lets us assert the driver's behavior without a
/// live SDK client.
class _FakeSink implements ShowcaseSink {
  _FakeSink(this.user);

  SinkUser? user;
  final List<SinkUser?> setUserCalls = <SinkUser?>[];
  final List<String> tracked = <String>[];
  int txns = 0;
  int flushes = 0;

  @override
  SinkUser? getUser() => user;

  @override
  void setUser(SinkUser? u) {
    user = u;
    setUserCalls.add(u);
  }

  @override
  void track(String name, {Map<String, Object?>? properties}) => tracked.add(name);

  @override
  void trackTransaction(SimTransaction txn) => txns++;

  @override
  Future<void> flush() async => flushes++;
}

List<int> _funnelCounts(String runId, int users) {
  final Random rng = makeRng(runId);
  final List<int> counts = List<int>.filled(funnelSteps.length, 0);
  for (int i = 0; i < users; i++) {
    for (final PlannedAction action in planUser(rng, i, runId).actions) {
      if (action is EventAction) {
        final int step = funnelSteps.indexOf(action.name);
        if (step >= 0) counts[step]++;
      }
    }
  }
  return counts;
}

void main() {
  test('makeRng is deterministic for a given seed', () {
    final Random a = makeRng('seed-1');
    final Random b = makeRng('seed-1');
    final List<double> seqA = <double>[a.nextDouble(), a.nextDouble(), a.nextDouble()];
    final List<double> seqB = <double>[b.nextDouble(), b.nextDouble(), b.nextDouble()];
    expect(seqA, seqB);
  });

  test('planUser emits funnel steps as an ordered prefix from product_viewed', () {
    final Random rng = makeRng('run-x');
    for (int i = 0; i < 200; i++) {
      final UserPlan plan = planUser(rng, i, 'run-x');
      final List<int> steps = plan.actions
          .whereType<EventAction>()
          .where((EventAction e) => funnelSteps.contains(e.name))
          .map((EventAction e) => funnelSteps.indexOf(e.name))
          .toList();
      expect(steps, List<int>.generate(steps.length, (int idx) => idx),
          reason: 'user $i funnel steps not a clean prefix: $steps');
      expect(plan.reached, inInclusiveRange(1, funnelSteps.length));
      expect(steps.length, plan.reached);
      expect(plan.id, 'sim_run-x_$i');
    }
  });

  test('aggregate funnel counts are monotonically non-increasing and everyone views', () {
    const int users = 300;
    final List<int> counts = _funnelCounts('cohort-a', users);
    expect(counts[0], users, reason: 'every synthetic user should emit product_viewed');
    for (int i = 1; i < counts.length; i++) {
      expect(counts[i] <= counts[i - 1], isTrue,
          reason: 'step $i (${counts[i]}) > step ${i - 1} (${counts[i - 1]})');
    }
    expect(counts.last < counts.first, isTrue, reason: 'funnel should show drop-off');
  });

  test('every planned transaction has a known op and a positive duration', () {
    final Random rng = makeRng('txn-run');
    int seen = 0;
    for (int i = 0; i < 200; i++) {
      for (final PlannedAction action in planUser(rng, i, 'txn-run').actions) {
        if (action is TxnAction) {
          seen++;
          expect(transactionOps.contains(action.txn.op), isTrue,
              reason: 'unknown op ${action.txn.op}');
          expect(action.txn.duration.inMilliseconds > 0, isTrue,
              reason: 'non-positive duration ${action.txn.duration}');
        }
      }
    }
    expect(seen, greaterThan(0));
  });

  test('runShowcase restores the pre-run user (null) and flushes once', () async {
    final _FakeSink sink = _FakeSink(null);
    final ShowcaseSummary summary =
        await runShowcase(sink, users: 25, runId: 'r1');
    expect(sink.flushes, 1);
    expect(sink.setUserCalls.last, isNull);
    expect(summary.users, 25);
    expect(summary.funnel.first.count, 25);
    expect(summary.transactions, greaterThan(0));
  });

  test('runShowcase restores a previously-set user', () async {
    final SinkUser original = SinkUser('user_demo_1', <String, Object?>{'plan': 'pro'});
    final _FakeSink sink = _FakeSink(original);
    await runShowcase(sink, users: 10, runId: 'r2');
    expect(identical(sink.setUserCalls.last, original), isTrue);
  });

  test('runShowcase clamps user count into [1, 500]', () async {
    final _FakeSink sink = _FakeSink(null);
    final ShowcaseSummary summary =
        await runShowcase(sink, users: 100000, runId: 'r3');
    expect(summary.users, 500);
  });
}
