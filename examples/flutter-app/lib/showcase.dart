/// Cohort simulator — the "Run showcase" engine (mobile).
///
/// Mirrors `examples/svelte-web/src/lib/showcase.ts` so the web and mobile
/// demos produce matching dashboards. Drives the SDK through a synthetic
/// e-commerce cohort — many synthetic users (switched via `setUser`) with
/// realistic drop-off, branching paths and a spread of performance
/// transactions — so the dashboard's Funnels, Journeys and Performance screens
/// have non-trivial data to show.
///
/// This file is deliberately SDK-free: the driver takes an injected
/// [ShowcaseSink], so the planning + emission logic is a pure unit test
/// (`test/showcase_test.dart`). The real Sauron-backed sink lives in `main.dart`.
library;

import 'dart:math';

/// The ordered funnel. Reuses `checkout_completed` (the manual demo button).
const List<String> funnelSteps = <String>[
  'product_viewed',
  'product_added_to_cart',
  'checkout_started',
  'payment_info_entered',
  'checkout_completed',
];

/// Cumulative retention per funnel step — the target drop-off shape.
const List<double> _retention = <double>[1.0, 0.65, 0.43, 0.28, 0.2];

/// Known transaction ops (mirror of the SDK's transaction op set).
const List<String> transactionOps = <String>[
  'navigation',
  'http',
  'resource',
  'screen_load',
  'custom',
];

const int defaultUsers = 120;
const int maxUsers = 500;

/// One timed operation to record via `trackTransaction`.
class SimTransaction {
  const SimTransaction({
    required this.name,
    required this.op,
    required this.duration,
    this.status,
    this.httpMethod,
    this.httpStatus,
    this.url,
  });

  final String name;
  final String op;
  final Duration duration;
  final String? status;
  final String? httpMethod;
  final int? httpStatus;
  final String? url;
}

/// One planned action in a synthetic user's session.
sealed class PlannedAction {
  const PlannedAction();
}

class EventAction extends PlannedAction {
  const EventAction(this.name, [this.properties]);

  final String name;
  final Map<String, Object?>? properties;
}

class TxnAction extends PlannedAction {
  const TxnAction(this.txn);

  final SimTransaction txn;
}

class UserPlan {
  const UserPlan({required this.id, required this.reached, required this.actions});

  final String id;

  /// How many funnel steps this user reached (1..funnelSteps.length).
  final int reached;
  final List<PlannedAction> actions;
}

/// A user identity as the sink sees it.
class SinkUser {
  const SinkUser(this.id, [this.traits]);

  final String id;
  final Map<String, Object?>? traits;
}

/// The SDK surface the driver needs — injected so this file stays testable.
abstract class ShowcaseSink {
  SinkUser? getUser();
  void setUser(SinkUser? user);
  void track(String name, {Map<String, Object?>? properties});
  void trackTransaction(SimTransaction txn);
  Future<void> flush();
}

class ShowcaseProgress {
  const ShowcaseProgress(this.done, this.total, this.events, this.transactions);

  final int done;
  final int total;
  final int events;
  final int transactions;
}

class FunnelCount {
  FunnelCount(this.name, this.count);

  final String name;
  int count;
}

class ShowcaseSummary {
  const ShowcaseSummary(this.users, this.events, this.transactions, this.funnel);

  final int users;
  final int events;
  final int transactions;
  final List<FunnelCount> funnel;
}

/// Deterministic PRNG for a given seed — same seed, same cohort.
Random makeRng(String seed) => Random(seed.hashCode & 0x7fffffff);

/// A right-skewed latency in ms: most values low, a long tail toward [max].
int _skewed(Random rng, int min, int max, [double k = 2.2]) =>
    (min + (max - min) * pow(rng.nextDouble(), k)).round();

/// Plan one synthetic user's actions. Pure — no SDK calls.
UserPlan planUser(Random rng, int index, String runId) {
  int reached = 1;
  for (int i = 1; i < funnelSteps.length; i++) {
    final double cond = _retention[i] / _retention[i - 1];
    if (rng.nextDouble() < cond) {
      reached++;
    } else {
      break;
    }
  }

  final List<PlannedAction> actions = <PlannedAction>[];
  void event(String name, [Map<String, Object?>? properties]) =>
      actions.add(EventAction(name, properties));
  void txn(SimTransaction t) => actions.add(TxnAction(t));

  // Entry: some users arrive via search.
  if (rng.nextDouble() < 0.4) {
    event('search_performed', <String, Object?>{'query': 'wireless headphones'});
  }

  // Landing: route load + product list fetch (+ sometimes a screen/resource load).
  txn(SimTransaction(
    name: 'ProductList',
    op: 'navigation',
    duration: Duration(milliseconds: _skewed(rng, 200, 1400)),
  ));
  txn(SimTransaction(
    name: 'GET /api/products',
    op: 'http',
    duration: Duration(milliseconds: _skewed(rng, 80, 1800)),
    httpMethod: 'GET',
    httpStatus: rng.nextDouble() < 0.03 ? 500 : 200,
    url: '/api/products',
  ));
  txn(SimTransaction(
    name: 'ProductListScreen',
    op: 'screen_load',
    duration: Duration(milliseconds: _skewed(rng, 120, 1600)),
  ));
  if (rng.nextDouble() < 0.5) {
    txn(SimTransaction(
      name: 'assets/catalog.json',
      op: 'resource',
      duration: Duration(milliseconds: _skewed(rng, 40, 600)),
    ));
  }

  // Step 0 — everyone views a product.
  event(funnelSteps[0], <String, Object?>{'product_id': 'sku_${index % 40}'});
  if (rng.nextDouble() < 0.3) event('viewed_recommendations');

  // Step 1 — add to cart.
  if (reached >= 2) {
    event(funnelSteps[1], <String, Object?>{'product_id': 'sku_${index % 40}'});
  }

  // Step 2 — start checkout (+ the checkout API call + screen load).
  if (reached >= 3) {
    event(funnelSteps[2]);
    txn(SimTransaction(
      name: 'CheckoutScreen',
      op: 'screen_load',
      duration: Duration(milliseconds: _skewed(rng, 150, 1800)),
    ));
    txn(SimTransaction(
      name: 'POST /api/checkout',
      op: 'http',
      duration: Duration(milliseconds: _skewed(rng, 150, 2600)),
      httpMethod: 'POST',
      httpStatus: rng.nextDouble() < 0.04 ? 500 : 200,
      url: '/api/checkout',
    ));
    if (rng.nextDouble() < 0.2) {
      event('applied_coupon', <String, Object?>{'code': 'SAVE10'});
    }
  }

  // Step 3 — enter payment.
  if (reached >= 4) event(funnelSteps[3]);

  // Step 4 — order complete.
  if (reached >= 5) {
    final double value = ((19 + rng.nextDouble() * 480) * 100).round() / 100;
    event(funnelSteps[4], <String, Object?>{'cart_value': value, 'currency': 'USD'});
  }

  return UserPlan(id: 'sim_${runId}_$index', reached: reached, actions: actions);
}

int _runCounter = 0;
String _defaultRunId() {
  _runCounter += 1;
  return 'run$_runCounter';
}

/// Drive the [sink] through a cohort of [users] synthetic users. Restores the
/// pre-run identity and flushes on the way out, even if something throws.
Future<ShowcaseSummary> runShowcase(
  ShowcaseSink sink, {
  int users = defaultUsers,
  String? runId,
  int yieldEvery = 12,
  void Function(ShowcaseProgress)? onProgress,
}) async {
  final int total = users.clamp(1, maxUsers);
  final String rid = runId ?? _defaultRunId();
  final int step = yieldEvery < 1 ? 1 : yieldEvery;
  final Random rng = makeRng(rid);

  final SinkUser? previousUser = sink.getUser();
  final List<FunnelCount> funnel =
      <FunnelCount>[for (final String n in funnelSteps) FunnelCount(n, 0)];
  int events = 0;
  int transactions = 0;

  try {
    for (int i = 0; i < total; i++) {
      final UserPlan plan = planUser(rng, i, rid);
      sink.setUser(SinkUser(plan.id, <String, Object?>{'plan': 'sim', 'cohort': rid}));

      for (final PlannedAction action in plan.actions) {
        if (action is EventAction) {
          sink.track(action.name, properties: action.properties);
          events++;
          final int s = funnelSteps.indexOf(action.name);
          if (s >= 0) funnel[s].count++;
        } else if (action is TxnAction) {
          sink.trackTransaction(action.txn);
          transactions++;
        }
      }

      final bool last = i == total - 1;
      if (onProgress != null && (i % step == 0 || last)) {
        onProgress(ShowcaseProgress(i + 1, total, events, transactions));
      }
      if (!last && (i + 1) % step == 0) {
        await Future<void>.delayed(Duration.zero);
      }
    }
  } finally {
    sink.setUser(previousUser);
    await sink.flush();
  }

  return ShowcaseSummary(total, events, transactions, funnel);
}
