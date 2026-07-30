import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:mocktail/mocktail.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

class _MockClient extends Mock implements http.Client {}

/// Most real apps must touch the binding before they can configure the SDK —
/// reading a persisted DSN, initializing Firebase, locking orientation — so
/// `main()` calls `WidgetsFlutterBinding.ensureInitialized()` first, in the
/// root zone. `runApp` must then run in that **same** zone, or Flutter's
/// `BindingBase.debugCheckZone` reports "Zone mismatch." from inside `runApp`
/// and every zone-scoped callback becomes unpredictable.
///
/// `flutter_test` no-ops `debugCheckZone` (its own zones never match), so these
/// tests assert the invariant directly: the zone `appRunner` runs in must be
/// the zone the caller was in when it initialized the binding.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  const MethodChannel pathProvider =
      MethodChannel('plugins.flutter.io/path_provider');
  const String dsn = 'https://pk_test@localhost:9/1';

  late Directory dir;
  late _MockClient httpClient;

  setUpAll(() {
    registerFallbackValue(Uri.parse('https://example.com'));
  });

  setUp(() async {
    dir = await Directory.systemTemp.createTemp('sauron_zone_test');
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(
            pathProvider, (MethodCall _) async => dir.path);

    httpClient = _MockClient();
    when(() => httpClient.post(
          any(),
          headers: any(named: 'headers'),
          body: any(named: 'body'),
        )).thenAnswer((_) async => http.Response('', 202));
  });

  tearDown(() async {
    await Sauron.close();
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(pathProvider, null);
    if (await dir.exists()) {
      await dir.delete(recursive: true);
    }
  });

  test('appRunner runs in the caller zone when the binding is already up',
      () async {
    // Stands in for the host app's `WidgetsFlutterBinding.ensureInitialized()`:
    // by this point the binding exists and belongs to this zone.
    final Zone callerZone = Zone.current;
    final Completer<Zone> runnerZone = Completer<Zone>();

    await Sauron.init(
      SauronOptions(dsn: dsn, httpClient: httpClient),
      appRunner: () => runnerZone.complete(Zone.current),
    );

    expect(
      await runnerZone.future,
      same(callerZone),
      reason: 'appRunner ran in a forked zone, so runApp() would report '
          '"Zone mismatch." against the already-initialized binding',
    );
  });

  test('layers 1-3 are installed even when the zone layer is skipped',
      () async {
    final Completer<void> ran = Completer<void>();

    await Sauron.init(
      SauronOptions(dsn: dsn, httpClient: httpClient),
      appRunner: () => ran.complete(),
    );
    await ran.future;

    // Layer 2 is the catch-all that replaces runZonedGuarded outside a guarded
    // zone; it must be wired even on the path that does not fork one.
    expect(PlatformDispatcher.instance.onError, isNotNull);
    expect(FlutterError.onError, isNotNull);
  });
}
