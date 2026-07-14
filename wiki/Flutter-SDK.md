# Flutter SDK — `sauron_flutter`

Error reporting **+** product analytics for Flutter, from one SDK. It binds four
uncaught-error capture layers (`FlutterError.onError`, `PlatformDispatcher.onError`,
`Isolate.addErrorListener`, and a guarding zone) plus manual capture, analytics,
screens, and breadcrumbs. Source: [`sdks/flutter`](../sdks/flutter).

See also: **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Examples](Examples.md)** · the runnable demo:
[`examples/flutter-app`](../examples/flutter-app).

## Install

Add the dependency in `pubspec.yaml` (path dependency shown; use the published package
when available):

```yaml
dependencies:
  sauron_flutter:
    path: ../../sdks/flutter
```

Then:

```dart
import 'package:sauron_flutter/sauron_flutter.dart';
```

## Init

`Sauron.init` takes a configure callback and an optional `appRunner`. When `appRunner`
is supplied, the app launches inside `runZonedGuarded` with all four capture layers
bound inside the zone:

```dart
Future<void> main() async {
  await Sauron.init((o) {
    o.dsn = 'https://<public_key>@<host>/<project_id>';
    o.environment = 'production';
    o.release = 'app@1.4.2+1402';
  }, appRunner: () => runApp(const MyApp()));
}
```

Without `appRunner`, integrations are still installed but you call `runApp` yourself.

### `SauronOptions`

| Field | Type | Default |
| --- | --- | --- |
| `dsn` | `String?` | — (null/empty ⇒ SDK disabled, all calls no-op) |
| `environment` | `String` | `'production'` |
| `release` | `String?` | — |
| `screen` | `String?` | — (seed the initial screen) |
| `sampleRate` | `double` | `1.0` (errors only) |
| `maxBreadcrumbs` | `int` | `100` |
| `beforeSend` | `ErrorItem? Function(ErrorItem)` | — |
| `flushInterval` | `Duration` | `5 s` |
| `maxBatchItems` | `int` | `30` |
| `maxQueueBytes` | `int` | `5 MiB` (offline queue) |
| `gzipThresholdBytes` | `int` | `1024` |
| `attachStacktrace` | `bool` | `true` |
| `debug` | `bool` | `false` |

## API

The public entry point is the static `Sauron` class:

| Method | Signature |
| --- | --- |
| `track` | `Sauron.track(String name, {Map<String, Object?>? properties})` |
| `captureException` | `Sauron.captureException(Object error, {StackTrace? stackTrace, Mechanism? mechanism})` |
| `identify` | `Sauron.identify(String distinctId, {Map<String, Object?>? traits})` |
| `setUser` | `Sauron.setUser(SauronUser? user)` — pass `null` to clear |
| `trackTransaction` | `Sauron.trackTransaction({required String name, required Duration duration, String op = 'custom', String? status, String? httpMethod, int? httpStatus, String? url})` |
| `setScreen` | `Sauron.setScreen(String name)` — emits a `$screen` view on change |
| `screen` | `Sauron.screen` → `String?` (getter) |
| `addBreadcrumb` | `Sauron.addBreadcrumb(Breadcrumb crumb)` |
| `flush` | `Sauron.flush()` → `Future<void>` |
| `close` | `Sauron.close()` → `Future<void>` |
| `addIsolateErrorListener` | `Sauron.addIsolateErrorListener(Isolate isolate)` |

`Sauron.client` returns the active `SauronClient` (or `null`); `Sauron.isEnabled`
reports whether the SDK is initialized and enabled.

### Track an event

```dart
Sauron.track('checkout_completed', properties: {'cart_value': 42.5});
```

### Capture an exception

```dart
try {
  doWork();
} catch (e, st) {
  Sauron.captureException(e, stackTrace: st);
}
```

Uncaught errors are captured automatically via the four layers bound at init.

### Identify a user

```dart
Sauron.identify('u_123', traits: {'plan': 'pro'});
// or set the full user:
Sauron.setUser(const SauronUser(id: 'u_123', email: 'ada@example.com'));
```

### Screen tracking

```dart
Sauron.setScreen('/settings');
final current = Sauron.screen; // '/settings'
```

For automatic route tracking, attach `SauronNavigatorObserver` to your `MaterialApp`'s
`navigatorObservers` (exported from `package:sauron_flutter/sauron_flutter.dart`). The
current screen is stamped onto events and errors.

### Performance transactions

```dart
final sw = Stopwatch()..start();
// ... work ...
Sauron.trackTransaction(
  name: 'GET /users', op: 'http', duration: sw.elapsed,
  httpMethod: 'GET', httpStatus: 200, url: 'https://api.example.com/users',
);
```

### Flush

```dart
await Sauron.flush(); // drains batched + persisted envelopes
await Sauron.close();
```

## Example

See [`examples/flutter-app`](../examples/flutter-app) — a Material 3 app that exercises
all four crash layers, analytics, identify, and a synthetic funnel/journey/performance
showcase. Run it with:

```bash
cd examples/flutter-app
flutter pub get
flutter run
```

More in **[Examples](Examples.md)**.
