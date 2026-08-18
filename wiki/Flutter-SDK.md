# Flutter SDK — `sauron_flutter`

Error reporting **+** product analytics for Flutter, from one SDK (**v1.8.0**). It binds
four uncaught-error capture layers (`FlutterError.onError`, `PlatformDispatcher.onError`,
`Isolate.addErrorListener`, and a guarding zone) plus manual capture, analytics,
screens, and breadcrumbs. Source: [`sdks/flutter`](../sdks/flutter).

See also: **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Examples](Examples.md)** · the runnable demo:
[`examples/flutter-app`](../examples/flutter-app).

## What's new in 0.3.0

- **`beforeSend` now runs on every item** — previously errors only. It is invoked for
  **every** outgoing item (error, event, identify, transaction, breadcrumb batch) just
  before it is enqueued. `BeforeSendCallback` widened from `ErrorItem? Function(ErrorItem)`
  to **`Object? Function(Object item)`**. Existing error-only logic keeps working (an
  error is still passed through as an item); guard on the runtime type if you only want a
  subset, e.g. `if (item is! ErrorItem) return item;`. See `CHANGELOG.md`.

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

`Sauron.init` takes a `SauronOptions` and an optional `appRunner`. When `appRunner`
is supplied, the app launches inside `runZonedGuarded` with all four capture layers
bound inside the zone:

```dart
Future<void> main() async {
  await Sauron.init(
    SauronOptions(
      dsn: 'https://<public_key>@<host>/<environment_id>',
      release: 'app@1.4.2+1402',
    ),
    appRunner: () => runApp(const MyApp()),
  );
}
```

Without `appRunner`, integrations are still installed but you call `runApp` yourself.
Uncaught errors are captured automatically via the four layers bound at init.

### `SauronOptions`

| Field | Type | Default |
| --- | --- | --- |
| `dsn` | `String?` | — (null/empty ⇒ SDK disabled, all calls no-op) |
| `release` | `String?` | — |
| `screen` | `String?` | — (seed the initial screen) |
| `tags` | `Map<String, String>` | `{}` — default scope tags |
| `contexts` | `Map<String, Map<String, Object?>>` | `{}` — default scope context blocks |
| `extra` | `Map<String, Object?>` | `{}` — default freeform extra |
| `sampleRate` | `double` | `1.0` (errors only) |
| `maxBreadcrumbs` | `int` | `100` |
| `beforeSend` | `Object? Function(Object item)` | — (any-item; return `null` to drop) |
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
| `captureException` | `Sauron.captureException(Object error, {StackTrace? stackTrace, Mechanism? mechanism, SauronLevel level, String? screen, Map<String, String>? tags, Map<String, Map<String, Object?>>? contexts, Map<String, Object?>? extra})` |
| `identify` | `Sauron.identify(String distinctId, {Map<String, Object?>? traits})` → `Future<void>` — **await it.** Also auto-detects a login by a different user than last time on this device (a forgotten `reset()` on logout) and mints a fresh anonymous id + rotates the session id first, so a switch never ships a cross-user alias. |
| `reset` | `Sauron.reset()` → `Future<void>` — **call on logout.** Clears the last-identified record and mints a fresh anonymous id and session id, so the next person is not merged into the previous one's history. Skipping it on a shared device is permanent. |
| `anonymousId` | `Sauron.anonymousId` → `String?` (getter) — the persisted `anon_<uuidv4>` events are attributed to before `identify()` |
| `setUser` | `Sauron.setUser(SauronUser? user)` — pass `null` to clear |
| `setTag` / `setTags` | `Sauron.setTag(String key, String value)` · `Sauron.setTags(Map<String, String> values)` |
| `setContext` | `Sauron.setContext(String name, Map<String, Object?> block)` |
| `setExtra` | `Sauron.setExtra(String key, Object? value)` |
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

### Identify a user

```dart
await Sauron.identify('u_123', traits: {'plan': 'pro'});
// or set the full user:
Sauron.setUser(const SauronUser(id: 'u_123', email: 'ada@example.com'));
```

### Reset on logout — MUST CALL

```dart
await Sauron.reset(); // on logout
```

The anonymous id is persisted in `<app-support>/sauron/sauron_prefs.json` under
`sauron.anon_id` and survives app restarts — that is what makes the Active
Users report count people rather than launches. Because it is durable, **not
calling `reset()` on logout aliases the next person to the last one**: on a
shared or kiosk device, the next anonymous user reuses the stored id, and
their activity is merged into the previous account server-side, forever.

As a safety net, `identify()` also persists a short one-way digest (never the
raw id) of the last user who identified, under `sauron.last_identified` in the
same prefs file — the same key name and digest algorithm the browser SDK uses
in `localStorage`. If the next `identify()` on this device is for a DIFFERENT
person, the SDK mints a fresh anonymous id and rotates the session id before
sending, so a forgotten `reset()` corrupts only that one guest window instead
of every one after it. This cannot undo an alias already sent under the old
id, so still call `reset()` on logout regardless. See
[the Flutter SDK README](../sdks/flutter/README.md#the-anonymous-id) for the
full detail, including why that digest is not a security boundary.

### Tags, contexts & extra

Attach your own metadata to errors and events. A scope setter lifts a value onto every
later capture; `init` options seed defaults; per-call args merge on top:

```dart
Sauron.setTag('checkout_step', 'payment');          // one filterable tag
Sauron.setTags({'region': 'eu-central', 'tier': 'pro'});
Sauron.setContext('cart', {'item_count': 3, 'total': 42.5}); // a named structured block
Sauron.setExtra('experiment_bucket', 'B');          // a loose one-off value

// or scoped to a single capture:
Sauron.captureException(e, stackTrace: st, tags: {'severity': 'high'});
```

**Tags** are a flat `key → value` map (indexed for filtering); **contexts** are named
structured blocks; **extra** is loose values — all developer-set, and distinct from the
SDK's machine-collected `context` (device/OS/platform). See
**[Best Practices §4](Best-Practices.md)** for when to use which, the
**[Dashboard](Dashboard.md)** for where they appear, and **[Search](Search.md)** to
filter by them.

### Breadcrumbs

```dart
Sauron.addBreadcrumb(Breadcrumb(
  type: 'db', category: 'query', message: 'SELECT users',
  level: SauronLevel.info, data: {'ms': 4},
));
// or the convenience constructors:
Sauron.addBreadcrumb(Breadcrumb.navigation('/settings'));
Sauron.addBreadcrumb(Breadcrumb.ui('tapped checkout'));
Sauron.addBreadcrumb(Breadcrumb.log('cache warmed'));
```

Crumbs ring-buffer at `maxBreadcrumbs` (default 100) and attach to errors captured
afterwards. `Breadcrumb.navigation`/`ui`/`log` are shorthand factories.

### `beforeSend` (any item)

`beforeSend` runs on **every** outgoing item just before it is enqueued (0.3.0 behavioral
change — see above). Return the item to send it, or `null` to drop it:

```dart
await Sauron.init(SauronOptions(
  dsn: dsn,
  beforeSend: (item) {
    if (item is EventItem) return null; // drop analytics events
    return item;                        // send everything else (incl. errors)
  },
));
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

### Transport: gzip & offline queue

Batches auto-flush every `flushInterval` (or at `maxBatchItems`). Payloads at or above
`gzipThresholdBytes` (default 1024) are gzipped where gzip is available. Pending
envelopes are held in a durable offline JSONL queue capped by `maxQueueBytes` (default
5 MiB, oldest evicted FIFO) and replayed on the next launch.

### Flush / close

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

## Obfuscated release builds

A `flutter build --obfuscate` release needs **two** artifacts uploaded, and they
fix different halves of what you read:

| Artifact | Fixes | Without it |
|---|---|---|
| `--split-debug-info` symbols (`app.symbols`) | the **stack frames** | frames are bare `0x…` addresses |
| `--save-obfuscation-map` JSON | the **exception type** | the class name is `xY1` |

The symbols file alone is not enough for the type. The SDK reports an
exception's class as `error.runtimeType.toString()`, and under `--obfuscate`
that string is *already* the renamed identifier by the time it leaves the
device. DWARF maps addresses to functions and says nothing about type names, so
the obfuscation map is the only artifact that can reverse it.

Build with both:

```bash
flutter build apk --release \
  --obfuscate --split-debug-info=build/symbols \
  --extra-gen-snapshot-options=--save-obfuscation-map=build/obfuscation.json
```

Upload the symbols first — its response reports the `derived_debug_id`, read out
of the ELF's build-id note — then upload the map under **that same id**:

```bash
sauron-symcli upload-dart \
  --api https://<host> --token <dashboard-jwt> --app <app-uuid> \
  --platform android --arch arm64 \
  build/symbols/app.android-arm64.symbols

sauron-symcli upload-obfuscation-map \
  --api https://<host> --token <dashboard-jwt> --app <app-uuid> \
  --platform android --debug-id <derived_debug_id from above> \
  build/obfuscation.json
```

The map carries nothing identifying inside it — it is a flat JSON array of
`[original, obfuscated]` pairs — so that shared id is the *only* thing tying it
to the build it came from. Uploading it without `--debug-id` is refused rather
than accepted-and-silently-useless.

**Both are presentational.** Grouping runs on the raw values the device sent, so
uploading either one later never re-groups issues you already have; existing
rows are rewritten as they are read. And nothing is symbolicated on the device —
the SDK never ships the map or the symbols to end users.
