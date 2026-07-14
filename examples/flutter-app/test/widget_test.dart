// Smoke test for the Sauron Flutter SDK demo app.
//
// The SDK itself is not initialized here (no `Sauron.init`), so the static
// facade calls are safe no-ops — we only verify the demo UI renders and that
// a non-throwing action can be tapped.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:sauron_flutter_demo/main.dart';

void main() {
  const DemoConfig config = DemoConfig(
    dsn: kDefaultDsn,
    environment: kDefaultEnvironment,
    release: kDefaultRelease,
    distinctId: kDefaultDistinctId,
  );

  // The demo is a long scrolling list; use a tall surface so every lazily-built
  // ListView child is realized and findable.
  setUp(() => TestWidgetsFlutterBinding.ensureInitialized());

  Future<void> pumpDemo(WidgetTester tester) async {
    await tester.binding.setSurfaceSize(const Size(1200, 3600));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(const SauronDemoApp(config: config));
  }

  testWidgets('renders the demo home screen', (WidgetTester tester) async {
    await pumpDemo(tester);

    expect(find.text('Sauron — Flutter SDK Demo'), findsOneWidget);
    expect(find.text('Throw uncaught (sync)'), findsOneWidget);
    expect(find.text('track: checkout_completed'), findsOneWidget);
    expect(find.textContaining('Open the Sauron dashboard'), findsOneWidget);
  });

  testWidgets('tapping a safe action appends to the activity log',
      (WidgetTester tester) async {
    await pumpDemo(tester);

    expect(find.text('No actions yet — tap a button above.'), findsOneWidget);

    // Tap "track: screen_viewed" (a no-op without an initialized SDK, but it
    // still records an activity-log entry). It is the second "Track" button.
    await tester.tap(find.widgetWithText(FilledButton, 'Track').last);
    await tester.pump();

    expect(find.text('track("screen_viewed")'), findsOneWidget);
    expect(find.text('No actions yet — tap a button above.'), findsNothing);
  });
}
