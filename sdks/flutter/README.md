# sauron_flutter

Error reporting + product analytics for Flutter — the Flutter SDK for the
**Sauron** platform (Sentry-style crash reporting fused with PostHog-style
product analytics).

- Captures **uncaught errors across all four Flutter/Dart layers**.
- Records **breadcrumbs** (navigation, lifecycle, custom).
- `track()` / `identify()` for product analytics.
- **Batches → gzips → persists** envelopes to an offline JSONL queue that
  **survives app restarts**, retries on connectivity, and honors the full
  ingest response policy.

## Install

```yaml
dependencies:
  sauron_flutter:
    path: ../sdks/flutter
```

## Quick start

```dart
import 'package:flutter/widgets.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

Future<void> main() async {
  await Sauron.init((o) {
    o.dsn = 'https://pk_test@localhost:8081/1';
    o.environment = 'production';
    o.release = 'app@1.4.2+1402';
    o.sampleRate = 1.0;
    o.maxBreadcrumbs = 100;
    o.beforeSend = (event) => event; // return null to drop
    o.flushInterval = const Duration(seconds: 5);
  }, appRunner: () => runApp(const MyApp()));
}
```

`appRunner` launches your app inside `runZonedGuarded`, with all four capture
layers bound **inside** the zone.

## API

```dart
Sauron.captureException(error, stackTrace: stack, mechanism: mechanism);
Sauron.track('checkout_completed', properties: {'cart_value': 42.5});
Sauron.identify('u_123', traits: {'plan': 'pro'});
Sauron.addBreadcrumb(Breadcrumb.navigation('/settings'));
Sauron.setUser(const SauronUser(id: 'u_123'));
await Sauron.flush();
await Sauron.close();
Sauron.addIsolateErrorListener(isolate); // user-spawned isolates
```

Add navigation breadcrumbs automatically:

```dart
MaterialApp(
  navigatorObservers: [SauronNavigatorObserver(Sauron.client!)],
);
```

## The four capture layers

All are composed **inside** `runZonedGuarded` and bound inside the zone:

1. **`FlutterError.onError`** — framework build/layout/paint/gesture/assert
   errors. The previous handler is chained; the debug red screen is preserved.
2. **`PlatformDispatcher.instance.onError`** — async errors with no framework
   callback (platform-channel failures, bare async gaps). Returns `true`.
3. **`Isolate.current.addErrorListener`** — uncaught isolate errors, gated on
   `!kIsWeb` (`dart:isolate` is absent on web). Use
   `Sauron.addIsolateErrorListener` for isolates you spawn.
4. **`runZonedGuarded`** — the outermost catch-all, which also covers
   binding/init before the other three install.

## Transport & offline queue

- Envelopes are batched, then **gzipped** (`dart:io` `GZipCodec` on
  mobile/desktop; skipped on web and for payloads under ~1&nbsp;KB).
- Failed/offline envelopes are persisted to a **JSONL file** in the
  app-support directory, enforcing a byte cap with **FIFO eviction**, and are
  **drained on init** (surviving app restarts).
- `connectivity_plus`'s `onConnectivityChanged` (now
  `List<ConnectivityResult>`) is used as a *hint* to drain — the authoritative
  success signal is always the HTTP response.

### Wire contract

```
POST /api/{project_id}/envelope
Content-Type: application/json
Content-Encoding: gzip            # when compressed
X-Sauron-Key: <public_key>
```

Response handling:

| Status        | Action                                   |
| ------------- | ---------------------------------------- |
| 202 / 200     | success — drop                           |
| 400           | drop, no retry                           |
| 401 / 403     | drop **and disable** the SDK             |
| 413           | split the envelope, retry the halves     |
| 429           | honor `Retry-After`                      |
| 408 / 5xx / network | backoff + jitter (cap 30s), retry  |

## Platform notes

The offline queue (`dart:io` files) and the isolate layer (`dart:isolate`)
target mobile/desktop. gzip degrades gracefully on web via a compile-time
conditional import; on web the SDK runs without on-disk persistence.

## Testing

```
flutter pub get
flutter analyze
flutter test
```

The golden-shape test in `test/envelope_test.dart` guards byte-for-byte parity
with the Rust backend and the JS SDK.
