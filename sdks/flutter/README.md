# sauron_flutter

Client SDK for **Sauron** (Sentry-style crash reporting fused with PostHog-style
product analytics), for Flutter apps on Android, iOS, macOS, Windows and Linux.
It runs inside your app, on the user's device — if you are instrumenting a
server, use `@edraj/sauron-node`, `sauron-sdk` (Python) or the C# SDK instead;
for a browser page, use `@edraj/sauron-browser`.

- Captures **uncaught errors across all four Flutter/Dart layers**
  (`FlutterError.onError`, `PlatformDispatcher.onError`,
  `Isolate.addErrorListener`, `runZonedGuarded`).
- Records **breadcrumbs** (app lifecycle, navigation, custom) and stamps every
  signal with the current **screen** and a per-launch **session id**.
- `track()` / `identify()` / `trackTransaction()` for product analytics and
  latency percentiles.
- Auto-collects device / OS / runtime context plus a stable, per-install
  `device_id`. App version/build are supplied by you at init — no plugin needed.
- **Batches → gzips → persists** envelopes to an offline JSONL queue that
  survives app restarts, drains on the flush timer and on app resume, and honors
  the full ingest response policy.
- Three package dependencies (`http`, `device_info_plus`, `path_provider`); no
  native code of its own, and **no permissions added to your manifest**.

## Install

```bash
flutter pub add sauron_flutter
```

or, in `pubspec.yaml`:

```yaml
dependencies:
  sauron_flutter: ^1.4.0
```

Requires Dart SDK `>=3.4.0 <4.0.0` and Flutter `>=3.19.0`.

**Android builds** additionally need Android Gradle Plugin `>=8.12.1`, Gradle
wrapper `>=8.13`, and Kotlin `2.2.0`. This floor comes from the `device_info_plus`
plugin, not from Sauron itself.

## Quick start

```dart
import 'package:flutter/material.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

Future<void> main() async {
  await Sauron.init(
    SauronOptions(
      dsn: 'https://pk_test@localhost:8081/1',
      release: 'app@1.4.2+1402',
    ),
    appRunner: () => runApp(const MyApp()),
  );
}
```

`appRunner` runs `WidgetsFlutterBinding.ensureInitialized()`, installs the
capture layers, awaits `bootstrap()` and then launches your app — all inside a
single `runZonedGuarded` zone. Do not call `runApp` yourself when you pass
`appRunner`.

Keep `ensureInitialized()` out of `main()`. Flutter pins `runApp` to the zone
the binding was built in, so initializing it before `Sauron.init` makes the zone
layer unavailable — see [Startup ordering](#startup-ordering).

That is enough for uncaught errors. Add analytics and manual capture anywhere:

```dart
Sauron.identify('u_123', traits: <String, Object?>{'plan': 'pro'});
Sauron.setScreen('Checkout');
Sauron.track('checkout_completed',
    properties: <String, Object?>{'cart_value': 42.5});

try {
  await placeOrder();
} on Exception catch (error, stack) {
  Sauron.captureException(error, stackTrace: stack);
}
```

## Configuration

`Sauron.init` takes a `SauronOptions`. Every parameter is named and optional;
fields stay mutable afterwards, so `SauronOptions(dsn: dsn)..debug = true` also
works. Every field, in constructor order:

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `dsn` | `String?` | `null` | **Required to send anything.** `https://<public_key>@<host>/<environment_id>`. Null, empty or malformed leaves the SDK disabled and every call a no-op. |
| `release` | `String?` | `null` | Release identifier, e.g. `app@1.4.2+1402`. Stamped on the envelope header. |
| `appVersion` | `String?` | `null` | App version for `context.app`, e.g. `1.4.2`. Developer-supplied — the SDK does not read it from the platform. |
| `appBuild` | `String?` | `null` | App build number for `context.app`, e.g. `1402`. When this and `appVersion` are both null the `app` block is omitted. |
| `screen` | `String?` | `null` | Seeds the initial screen name, stamped on events/errors until `setScreen` (or `SauronNavigatorObserver`) changes it. |
| `sampleRate` | `double` | `1.0` | Fraction of **errors** sent, clamped to `[0.0, 1.0]`. Analytics events, identifies and transactions are never sampled. |
| `maxBreadcrumbs` | `int` | `100` | Breadcrumb ring-buffer size; oldest evicted first. `<= 0` disables breadcrumbs entirely. |
| `tags` | `Map<String, String>` | `{}` | Default tags seeded into the global scope at init. |
| `contexts` | `Map<String, Map<String, Object?>>` | `{}` | Default structured context blocks seeded into the global scope. |
| `extra` | `Map<String, Object?>` | `{}` | Default freeform extra seeded into the global scope. |
| `beforeSend` | `BeforeSendCallback?` | `null` | Runs on **every** outgoing item just before it is enqueued. Return the item to send, `null` to drop. |
| `flushInterval` | `Duration` | `Duration(seconds: 5)` | Transport auto-flush cadence. |
| `maxBatchItems` | `int` | `30` | Buffered-item count that *triggers* an eager flush. |
| `maxItemsPerEnvelope` | `int` | `1000` | Hard ceiling on items per envelope, matching the server limit. The buffer is packed into chunks of this size. `<= 0` means "one envelope, whatever the size". |
| `maxQueueBytes` | `int` | `5 * 1024 * 1024` | On-disk offline-queue cap. Oldest envelopes evicted FIFO; the newest is always kept. |
| `gzipThresholdBytes` | `int` | `1024` | Bodies **at or above** this size are gzipped, where gzip is available. |
| `debug` | `bool` | `false` | Emit `[Sauron] …` diagnostics via `debugPrint`, including every item delivered to the server — see [Seeing what is sent](#seeing-what-is-sent). |
| `attachStacktrace` | `bool` | `true` | Attach `StackTrace.current` to captured errors that arrive without one. |
| `httpClient` | `http.Client?` | `null` | Injected HTTP client (tests). Defaults to a fresh `http.Client()`. |

Two derived getters are also public: `normalizedSampleRate` (`sampleRate`
clamped to `[0.0, 1.0]`) and `isConfigured` (`dsn` is non-null and non-empty).

### Seeing what is sent

`debug: true` prints every item the server accepted, so you can confirm what
actually left the device instead of inferring it from the absence of errors.
The line is emitted on delivery, not on capture — items are queued, may be
split or retried, and can survive a restart before they land:

```
[Sauron] delivered 3 item(s) to https://ingest.example.com/api/42/envelope:
[Sauron]   identify u_123 (traits={"plan":"pro"})
[Sauron]   event checkout_completed (distinct_id=u_123, screen=Checkout, properties={"cart_value":42.5})
[Sauron]   error StateError: Bad state: card declined (level=error, screen=Checkout)
```

Transactions and breadcrumb batches are logged the same way
(`transaction GET /orders op=http 120.0ms status=null`,
`breadcrumb_batch 12 crumb(s)`). Long values are truncated to keep one item on
one line. Keep `debug` off in release builds — the payload summaries include
user-supplied properties and traits.

Everything set at once:

```dart
await Sauron.init(
  SauronOptions(
    dsn: 'https://pk_test@ingest.example.com/42',
    release: 'app@1.4.2+1402',
    appVersion: '1.4.2',
    appBuild: '1402',
    screen: 'Splash',
    sampleRate: 0.25,
    maxBreadcrumbs: 50,
    tags: <String, String>{'tier': 'free'},
    contexts: <String, Map<String, Object?>>{
      'build': <String, Object?>{'flavor': 'beta'},
    },
    extra: <String, Object?>{'boot_ms': 412},
    beforeSend: (Object item) {
      if (item is EventItem && item.name == 'secret') return null;
      return item;
    },
    flushInterval: const Duration(seconds: 10),
    maxBatchItems: 50,
    maxItemsPerEnvelope: 500,
    maxQueueBytes: 2 * 1024 * 1024,
    gzipThresholdBytes: 2048,
    debug: true,
    attachStacktrace: true,
    httpClient: null, // leave null outside tests
  ),
  appRunner: () => runApp(const MyApp()),
);
```

### Supplying app version

The SDK does not read your app's version off the platform — that would pull in a
plugin (and its Android toolchain requirements) for two strings you already know
at build time. Supply them yourself via `appVersion` / `appBuild`.

The dependency-free option is `--dart-define`, which keeps the values in your
build command and out of the source tree:

```bash
flutter build apk \
  --dart-define=APP_VERSION=1.4.2 \
  --dart-define=APP_BUILD=1402
```

```dart
SauronOptions(
  dsn: dsn,
  appVersion: const String.fromEnvironment('APP_VERSION'),
  appBuild: const String.fromEnvironment('APP_BUILD'),
);
```

If you already depend on `package_info_plus` for other reasons, read it from
there instead — the SDK is happy either way:

```dart
final PackageInfo info = await PackageInfo.fromPlatform();
SauronOptions(dsn: dsn, appVersion: info.version, appBuild: info.buildNumber);
```

Leave both unset and the `app` context block is omitted; nothing else is
affected. Note `release` is separate — it identifies the build on the envelope
header and is what the dashboard groups by, so set it regardless.

### `BeforeSendCallback`

```dart
typedef BeforeSendCallback = Object? Function(Object item);
```

The argument is the outgoing `EnvelopeItem` — an `ErrorItem`, `EventItem`,
`IdentifyItem`, `TransactionItem` or `BreadcrumbBatchItem`. Return it (possibly
mutated), return a replacement item, or return `null` to drop it. It runs on
every item type, so guard on the runtime type if you only care about a subset:

```dart
SauronOptions(
  dsn: dsn,
  beforeSend: (Object item) {
    if (item is! ErrorItem) return item;
    if (item.exception.value.contains('@')) return null; // drop PII
    return item;
  },
);
```

## API reference

Convention below: named parameters are written with a trailing colon
(`stackTrace:`); everything else is positional. Required parameters say so in
the Default column.

All `Sauron.*` members are static and delegate to `Sauron.client`. Before
`init` (or after `close`) the client is `null` and every call is a silent
no-op — except the three workflow methods, which instead return a
`WorkflowResult(status: WorkflowStatus.disabled)` (see
[Workflows](#sauronstartworkflow--sauronendworkflow--sauroncancelworkflow--sauronworkflow)).

### `Sauron.init`

```dart
static Future<void> init(
  SauronOptions options, {
  FutureOr<void> Function()? appRunner,
})
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `options` | `SauronOptions` | required | The configuration object — see [Configuration](#configuration). |
| `appRunner:` | `FutureOr<void> Function()?` | `null` | When supplied, binding init + integrations + `bootstrap()` + your app all run inside one `runZonedGuarded`. If the binding is already initialized the zone is skipped — see [Startup ordering](#startup-ordering). |

Returns `Future<void>`. Without `appRunner`, `init` calls
`WidgetsFlutterBinding.ensureInitialized()`, installs the integrations and
awaits `bootstrap()` itself — you then call `runApp` yourself and forgo the
`runZonedGuarded` layer.

```dart
// With the zone (recommended):
await Sauron.init(SauronOptions(dsn: dsn),
    appRunner: () => runApp(const MyApp()));

// Without it:
await Sauron.init(SauronOptions(dsn: dsn));
runApp(const MyApp());
```

### `Sauron.captureException`

```dart
static void captureException(
  Object error, {
  StackTrace? stackTrace,
  Mechanism? mechanism,
  SauronLevel level = SauronLevel.error,
  String? screen,
  Map<String, String>? tags,
  Map<String, Map<String, Object?>>? contexts,
  Map<String, Object?>? extra,
})
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `error` | `Object` | required | Any thrown value. `runtimeType` becomes the exception type, `toString()` the value. |
| `stackTrace:` | `StackTrace?` | `null` | Falls back to `StackTrace.current` when `attachStacktrace` is `true`, otherwise no frames. |
| `mechanism:` | `Mechanism?` | `Mechanism(type: 'manual', handled: true)` | How the error reached the SDK. |
| `level:` | `SauronLevel` | `SauronLevel.error` | Severity. |
| `screen:` | `String?` | current screen | Per-call screen override. |
| `tags:` | `Map<String, String>?` | `null` | Per-call tags, merged over scope tags by key. |
| `contexts:` | `Map<String, Map<String, Object?>>?` | `null` | Per-call context blocks, replacing same-named scope blocks. |
| `extra:` | `Map<String, Object?>?` | `null` | Per-call extra, merged over scope extra by key. |

Returns `void`. Subject to `sampleRate`; attaches the current breadcrumbs; then
triggers an eager (unawaited) flush, because errors are worth sending now.

```dart
try {
  throw const FormatException('bad payload');
} on FormatException catch (error, stack) {
  Sauron.captureException(
    error,
    stackTrace: stack,
    level: SauronLevel.warning,
    tags: <String, String>{'endpoint': '/orders'},
    extra: <String, Object?>{'retries': 3},
  );
}
```

### `Sauron.track`

```dart
static void track(
  String name, {
  Map<String, Object?>? properties,
  Map<String, String>? tags,
  Map<String, Map<String, Object?>>? contexts,
  Map<String, Object?>? extra,
})
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `String` | required | Event name, e.g. `checkout_completed`. |
| `properties:` | `Map<String, Object?>?` | `null` | Event properties (JSON-encodable). |
| `tags:` | `Map<String, String>?` | `null` | Per-call tags. |
| `contexts:` | `Map<String, Map<String, Object?>>?` | `null` | Per-call context blocks. |
| `extra:` | `Map<String, Object?>?` | `null` | Per-call extra. |

Returns `void`. The event carries the current distinct id, session id and
screen. Never sampled. The static facade has no `screen:` parameter — use
`Sauron.client!.track(name, screen: 'Checkout')` for a per-call screen override.

```dart
Sauron.track(
  'checkout_completed',
  properties: <String, Object?>{'cart_value': 42.5, 'currency': 'USD'},
  tags: <String, String>{'plan': 'pro'},
);
```

### `Sauron.trackTransaction`

```dart
static void trackTransaction({
  required String name,
  required Duration duration,
  String op = 'custom',
  String? status,
  String? httpMethod,
  int? httpStatus,
  String? url,
})
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name:` | `String` | required | Route / operation label — the grouping key on the dashboard. |
| `duration:` | `Duration` | required | Serialized as fractional milliseconds (`inMicroseconds / 1000.0`). |
| `op:` | `String` | `'custom'` | One of `navigation`, `http`, `resource`, `screen_load`, `custom`. |
| `status:` | `String?` | `null` | Free-form outcome, e.g. `ok`, `error`. |
| `httpMethod:` | `String?` | `null` | HTTP verb for `http` transactions. |
| `httpStatus:` | `int?` | `null` | HTTP response status for `http` transactions. |
| `url:` | `String?` | `null` | Request URL for `http` / `resource` transactions. |

Returns `void`. The current distinct id and session id are attached
automatically.

```dart
final Stopwatch sw = Stopwatch()..start();
final int statusCode = await fetchUsers();
sw.stop();
Sauron.trackTransaction(
  name: 'GET /users',
  op: 'http',
  duration: sw.elapsed,
  httpMethod: 'GET',
  httpStatus: statusCode,
  url: 'https://api.example.com/users',
  status: statusCode < 400 ? 'ok' : 'error',
);
```

### `Sauron.setScreen` / `Sauron.screen`

```dart
static void setScreen(String name)
static String? get screen
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `String` | required | The new screen/route name. |

Returns `void`. A no-op when `name` equals the current screen. On an actual
change it emits one `$screen` analytics event with
`properties: {'screen': name}` so dwell time can be computed server-side, and
every later event/error is stamped with the new screen. `Sauron.screen` reads
the current value (`null` until set or seeded via `options.screen`).

```dart
Sauron.setScreen('Checkout');
Sauron.setScreen('Checkout'); // no-op, no second $screen event
assert(Sauron.screen == 'Checkout');
```

### `Sauron.startWorkflow` / `Sauron.endWorkflow` / `Sauron.cancelWorkflow` / `Sauron.workflow`

A **workflow** is a named, explicitly-bounded span of activity you declare
around a multi-step flow (checkout, onboarding, a background sync) so the
dashboard can group the errors/events/transactions captured inside it. Entirely
optional: if you never call `startWorkflow`, nothing changes — no field is
added to any item, and `workflow_id`/`workflow_name` are omitted from the wire
rather than sent as `null`.

```dart
static WorkflowResult startWorkflow(String name, {bool force = false})
static WorkflowResult endWorkflow([String? name])
static WorkflowResult cancelWorkflow([String? name, String? reason])
static ActiveWorkflow? get workflow
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` (start) | `String` | required | The workflow's name. Trimmed; must be non-empty and at most 120 chars after trimming, or the call is rejected. |
| `force:` | `bool` | `false` | When `true` and another workflow is already active, that workflow is closed first (`$workflow_cancel`, `reason: 'superseded'`) and the new one starts. When `false`, an already-active workflow makes the call a no-op. |
| `name` (end/cancel) | `String?` | `null` | Optional guard: when given, the call only takes effect if it equals the active workflow's name. `null` closes whichever workflow is active. |
| `reason` (cancel only) | `String?` | `null` | Free-form cancellation reason. Trimmed and capped at 120 chars; defaults to `'user'` when omitted or blank. |

While a workflow is active, its `workflow_id` (a fresh, client-generated
UUIDv4 — never derived from the session id, device id, or the name itself) and
`workflow_name` are stamped onto every captured error, `track()` event, and
`trackTransaction()`, in addition to riding along in the lifecycle event's own
`properties`. `identify()` is never stamped — the server has no workflow
columns for it.

Starting, ending, and cancelling a workflow each emit one reserved analytics
event through `track()` — `$workflow_start`, `$workflow_end`, or
`$workflow_cancel` — so they show up in Events like anything else. `endWorkflow`
adds `duration_ms`; `cancelWorkflow` adds both `duration_ms` and `reason`.

All three mutators return a `WorkflowResult { status, workflowId }`. Exactly six
`WorkflowStatus` values, never a seventh:

| Status | From | Meaning |
| --- | --- | --- |
| `ok` | any | The call took effect. `workflowId` is the id of the workflow it affected. |
| `alreadyActive` | `startWorkflow` | Another workflow is active and `force` was not set. No-op. |
| `invalidName` | `startWorkflow` | `name` is empty after trimming, or over 120 chars. No-op. |
| `notActive` | `endWorkflow`/`cancelWorkflow` | No workflow is active. No-op. |
| `nameMismatch` | `endWorkflow`/`cancelWorkflow` | The given `name` does not equal the active workflow's name — **including when the given `name` itself fails normalization** (blank, or over 120 chars). `invalidName` is only ever returned by `startWorkflow`; a malformed name on end/cancel is `nameMismatch`. |
| `disabled` | any | The SDK did not perform the call: before `init`, after `close()`, after the transport auto-disabled itself (401/403), **or an unexpected internal error**. Never a claim about workflow state or your input — just "the SDK did not do this." |

`Sauron.workflow` reads the active `ActiveWorkflow { workflowId, name,
startedAt }`, or `null` when none is active (including before `init` / after
`close()`).

**Abandonment.** A workflow with no further stamped activity for 30 minutes
reads as `abandoned` when you view it on the dashboard. This is computed
**on read** from the last stamped event's timestamp — it is never stored, and
no client action is needed. If a later event or error is stamped with that
same `workflow_id` after the 30-minute mark, the workflow simply reads as
active again; nothing needs to be "resumed."

```dart
// A genuine bounded span: start, do work (events + a captured error land
// inside it), then end.
final WorkflowResult started = Sauron.startWorkflow('checkout');
if (started.status == WorkflowStatus.ok) {
  Sauron.track('checkout_started');
  try {
    await placeOrder();
    Sauron.track('checkout_completed', properties: <String, Object?>{'cart_value': 42.5});
  } on Exception catch (error, stack) {
    Sauron.captureException(error, stackTrace: stack); // stamped with this workflow
  }
  Sauron.endWorkflow(); // emits $workflow_end with duration_ms
}

// Cancelling instead of ending — e.g. the user backs out of the flow.
Sauron.startWorkflow('checkout');
Sauron.cancelWorkflow(); // reason defaults to 'user'

Sauron.startWorkflow('checkout');
Sauron.cancelWorkflow('checkout', 'payment declined 3x');
```

### `Sauron.identify`

```dart
static void identify(String distinctId, {Map<String, Object?>? traits})
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `distinctId` | `String` | required | Stable user id; becomes the scope user's `id` and the `distinct_id` on later events. |
| `traits:` | `Map<String, Object?>?` | `null` | User traits. When `null`, the existing user's traits are preserved. |

Returns `void`. Emits an `identify` item and updates the scope user, keeping the
existing `email`. Never sampled.

```dart
Sauron.identify('u_123', traits: <String, Object?>{'plan': 'pro'});
```

### `Sauron.addBreadcrumb`

```dart
static void addBreadcrumb(Breadcrumb crumb)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `crumb` | `Breadcrumb` | required | The breadcrumb to append to the ring buffer. |

Returns `void`. Breadcrumbs are not sent on their own; a snapshot of the buffer
rides along with each captured error.

```dart
Sauron.addBreadcrumb(Breadcrumb.ui('Tapped: checkout'));
Sauron.addBreadcrumb(Breadcrumb.navigation('/settings'));
Sauron.addBreadcrumb(
  Breadcrumb.log('cache miss', level: SauronLevel.warning),
);
```

### `Sauron.setUser`

```dart
static void setUser(SauronUser? user)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `user` | `SauronUser?` | required | Replaces the scope user wholesale. `null` clears it. |

Returns `void`. The user is serialized into `context.user` of every envelope.

```dart
Sauron.setUser(const SauronUser(id: 'u_123', email: 'dev@example.com'));
Sauron.setUser(null); // on sign-out
```

### `Sauron.setTag` / `Sauron.setTags`

```dart
static void setTag(String key, String value)
static void setTags(Map<String, String> values)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `String` | required | Tag key. |
| `value` | `String` | required | Tag value. |
| `values` | `Map<String, String>` | required | Tags merged into the scope, last-write-wins by key. |

Both return `void`.

```dart
Sauron.setTag('feature', 'checkout');
Sauron.setTags(<String, String>{'tier': 'free', 'ab_bucket': 'b'});
```

### `Sauron.setContext`

```dart
static void setContext(String name, Map<String, Object?> block)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `String` | required | Block name. |
| `block` | `Map<String, Object?>` | required | Structured block; **replaces** any existing block with the same name. |

Returns `void`.

```dart
Sauron.setContext('order', <String, Object?>{'id': 7, 'total': 42.5});
```

### `Sauron.setExtra`

```dart
static void setExtra(String key, Object? value)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `String` | required | Extra key. |
| `value` | `Object?` | required | Any JSON-encodable value. |

Returns `void`.

```dart
Sauron.setExtra('cart_size', 3);
```

### `Sauron.flush`

```dart
static Future<void> flush()
```

Zero-argument. Packs the buffer into an envelope, persists it, and drains the
on-disk queue. Awaiting it awaits the drain attempt (not delivery of retried
envelopes).

```dart
await Sauron.flush();
```

### `Sauron.close`

```dart
static Future<void> close()
```

Zero-argument. Flushes, cancels the timers, closes the HTTP client, uninstalls
the four capture layers (restoring the handlers they replaced), and clears
`Sauron.client`.

Terminal and idempotent: `isEnabled` flips to `false`, and anything captured
afterwards is dropped rather than buffered — a long-lived process that closes
the SDK does not accumulate events. Re-`init` in the same process is not
supported.

```dart
await Sauron.close();
```

### `Sauron.addIsolateErrorListener`

```dart
static void addIsolateErrorListener(Isolate isolate)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `isolate` | `Isolate` | required | An isolate you spawned. |

Returns `void`. The SDK auto-listens on `Isolate.current` only; isolates you
spawn must be registered explicitly. No-op on web.

```dart
final Isolate isolate =
    await Isolate.spawn<String>(_entry, 'payload', paused: true);
Sauron.addIsolateErrorListener(isolate);
isolate.resume(isolate.pauseCapability!);
```

### `Sauron.client` / `Sauron.isEnabled`

```dart
static SauronClient? get client
static bool get isEnabled
```

`client` is `null` before `init` and after `close`. `isEnabled` is `true` only
when a client exists, its DSN parsed successfully, **and** the transport has
not disabled itself. The transport disables itself permanently for the process
when the gateway rejects the ingest key with a `401`/`403` (see
[Transport & delivery](#transport--delivery)) — so `isEnabled` going `false`
mid-session means the key was revoked or rotated, and nothing more will be
delivered until the app restarts with a valid key.

### `SauronClient`

The engine behind the facade. `Sauron.client` hands you the live instance; it is
also constructible directly (the tests do this) when you want two clients or
full control of the lifecycle.

```dart
SauronClient(SauronOptions options)
```

It exposes every capture/scope method the facade does, with two differences and
four additions:

| Member | Signature | Notes |
| --- | --- | --- |
| `options` | `final SauronOptions` | The options this client was built with. |
| `sessionId` | `final String` | UUIDv4 generated at construction; stamped on errors, events and transactions. |
| `screen` | `String? get` | Current screen. |
| `workflow` | `ActiveWorkflow? get` | Current workflow, or `null`. Same as `Sauron.workflow`; `startWorkflow`/`endWorkflow`/`cancelWorkflow` also exist here with identical signatures to the facade. |
| `isEnabled` | `bool get` | Whether the DSN parsed, the client is not closed, and the transport has not auto-disabled on a `401`/`403`. |
| `installIntegrations()` | `void installIntegrations()` | Installs the error layers + lifecycle observer. Must run after `WidgetsFlutterBinding.ensureInitialized()`. Called for you by `Sauron.init`. |
| `bootstrap({Directory? queueDirectory})` | `Future<void>` | Resolves the queue directory (defaults to `<app-support>/sauron`), loads device context, starts the transport and replays anything captured before it was ready. Idempotent. |
| `track(...)` | adds `screen:` | `client.track(name, properties:, screen:, tags:, contexts:, extra:)` — the facade omits `screen:`. |

```dart
final SauronClient client = SauronClient(
  SauronOptions()..dsn = 'https://pk_test@localhost:8081/1',
);
await client.bootstrap(queueDirectory: Directory.systemTemp);
client.track('viewed', screen: 'Home');
await client.close();
```

### Types

Everything below is exported from `package:sauron_flutter/sauron_flutter.dart`.

| Type | Constructor / signature | Notes |
| --- | --- | --- |
| `SauronLevel` | enum `debug, info, warning, error, fatal` | Wire value is `.name`. |
| `Breadcrumb` | `Breadcrumb({required String type, required String category, String? message, SauronLevel level = SauronLevel.info, DateTime? timestamp, Map<String, Object?>? data})` | All named. `timestamp` defaults to `DateTime.now().toUtc()`, `data` to `{}`. |
| `Breadcrumb.navigation` | `(String route, {Map<String, Object?>? data})` | type `navigation`, category `route`. |
| `Breadcrumb.ui` | `(String message, {Map<String, Object?>? data})` | type `ui`, category `click`. |
| `Breadcrumb.log` | `(String message, {SauronLevel level = SauronLevel.info, Map<String, Object?>? data})` | type `log`, category `console`. |
| `SauronUser` | `const SauronUser({String? id, String? email, Map<String, Object?> traits = const {}})` | Has `copyWith({id, email, traits})`. |
| `Mechanism` | `const Mechanism({required String type, bool handled = false})` | `type` is the capture layer. |
| `SauronException` | `const SauronException({required String type, required String value, required Mechanism mechanism, List<StackFrame> stacktrace = const []})` | Built for you by `captureException`. |
| `StackFrame` | `const StackFrame({String? function, String? filename, int? lineno, int? colno, bool inApp = false})` | `inApp` is `true` for `package:`/`file:` frames that are not `dart:`, `package:flutter/`, `package:flutter_test/` or `package:sauron_flutter/`. |
| `DebugMeta` | `const DebugMeta({String? buildId, String? isolateDsoBase, String? arch, String? os})` and `DebugMeta.fromTrace(String raw, {String? os})` | Parses `build_id:` and `isolate_dso_base:` out of an AOT trace header. |
| `DeviceDescriptor` | `const DeviceDescriptor({String? family, String? model, String? arch, String? deviceId})` | Has `copyWith`. |
| `OsDescriptor` | `const OsDescriptor({String? name, String? version})` | |
| `AppDescriptor` | `const AppDescriptor({String? version, String? build})` | |
| `RuntimeDescriptor` | `const RuntimeDescriptor({String? name, String? version})` | |
| `SauronContext` | `const SauronContext({DeviceDescriptor? device, OsDescriptor? os, AppDescriptor? app, RuntimeDescriptor? runtime, SauronUser? user})` | Has `copyWith`. Built by the SDK each send. |
| `EnvelopeHeader` | `const EnvelopeHeader({required String dsn, required DateTime sentAt, String? release, String sdkName = kSauronSdkName, String sdkVersion = kSauronSdkVersion})` | |
| `Envelope` | `const Envelope({required EnvelopeHeader header, required SauronContext context, required List<EnvelopeItem> items})` | `encode()` returns the compact wire JSON. |
| `EnvelopeItem` | abstract; `String get type`, `Map<String, Object?> toJson()`, `int get approximateBytes` | Base of all items below. |
| `ErrorItem` | `ErrorItem({required SauronException exception, required DateTime timestamp, SauronLevel level = SauronLevel.error, List<Breadcrumb> breadcrumbs = const [], List<String>? fingerprint, String? sessionId, String? workflowId, String? workflowName, String? screen, String? rawStacktrace, DebugMeta? debugMeta, Map<String, String> tags = const {}, Map<String, Map<String, Object?>> contexts = const {}, Map<String, Object?> extra = const {}})` | `fingerprint` is never set by the SDK — `null` lets the server group. `workflowId`/`workflowName` are stamped by `captureException` from the active workflow, if any — omitted from the wire (never `null`) when there isn't one. |
| `EventItem` | `EventItem({required String name, required DateTime timestamp, String? distinctId, String? sessionId, String? workflowId, String? workflowName, String? screen, Map<String, Object?>? properties, Map<String, String>? tags, Map<String, Map<String, Object?>>? contexts, Map<String, Object?>? extra})` | `workflowId`/`workflowName` — see `ErrorItem`. |
| `IdentifyItem` | `IdentifyItem({required String distinctId, String? anonymousId, Map<String, Object?>? traits})` | Never carries workflow fields — the server has no workflow columns for `identify`. |
| `TransactionItem` | `TransactionItem({required String name, required double durationMs, String op = 'custom', String? status, String? httpMethod, int? httpStatus, String? url, String? distinctId, String? sessionId, String? workflowId, String? workflowName, DateTime? timestamp})` | `workflowId`/`workflowName` — see `ErrorItem`. |
| `BreadcrumbBatchItem` | `BreadcrumbBatchItem({required List<Breadcrumb> breadcrumbs, DateTime? timestamp})` | Part of the wire contract; the Flutter SDK never emits one on its own. Construct and pass it through a `SauronClient` only if you need standalone breadcrumbs. |
| `Dsn` | `Dsn({required String scheme, required String publicKey, required String host, required int port, required String projectId, List<String> pathPrefix = const []})` | `projectId` is the DSN's path segment — despite the name, this is the **environment** id since the ingest key now lives on the environment, not the app. `Dsn.parse(String input)` throws `FormatException`. `envelopeEndpoint` → `Uri` of `.../api/{environment_id}/envelope`; `toString()` round-trips the canonical DSN. |
| `DartStackTraceParser` | `const DartStackTraceParser()`, `List<StackFrame> parse(Object? stackTrace)`, `static bool isNoise(String line)` | Parses JIT and AOT traces; unrecognized lines are dropped. |
| `isObfuscatedDartTrace` | `bool isObfuscatedDartTrace(String raw)` | `true` when the trace contains `isolate_dso_base` or `build_id:`. |
| `sauronIso` | `String sauronIso(DateTime dateTime)` | ISO-8601 UTC with a trailing `Z`. |
| `kSauronSdkName` | `const String = 'sauron.flutter'` | Sent in `header.sdk.name`. |
| `kSauronSdkVersion` | `const String = '1.4.0'` | Sent in `header.sdk.version`. |
| `WorkflowStatus` | enum `ok, alreadyActive, notActive, nameMismatch, invalidName, disabled` | Wire values (in lifecycle-adjacent server logs) are snake_case: `already_active`, `not_active`, `name_mismatch`, `invalid_name`. See [Workflows](#sauronstartworkflow--sauronendworkflow--sauroncancelworkflow--sauronworkflow). |
| `WorkflowResult` | `WorkflowResult(WorkflowStatus status, [String? workflowId])` | `workflowId` is set when `status == ok`. |
| `ActiveWorkflow` | `ActiveWorkflow({required String workflowId, required String name, required DateTime startedAt})` | What `Sauron.workflow`/`client.workflow` returns; `null` when none is active. |
| `SauronNavigatorObserver` | see [Flutter integration](#flutter-integration) | |
| `SauronWidgetsBindingObserver` | see [Flutter integration](#flutter-integration) | |

## Scope & metadata

There is exactly one scope per client. It holds the user, a bounded breadcrumb
buffer, and three developer-owned maps: `tags` (flat `String -> String`),
`contexts` (named structured blocks) and `extra` (freeform JSON). The
machine-collected `context` (device / os / app / runtime / user) is separate and
is never touched by these setters.

Precedence, lowest to highest:

1. **Init defaults** — `options.tags` / `options.contexts` / `options.extra`
   seed the scope when the client is constructed.
2. **Runtime setters** — `setTag`, `setTags`, `setContext`, `setExtra` mutate
   that same scope; they overwrite seeded values by key.
3. **Per-call arguments** — `tags:` / `contexts:` / `extra:` on
   `captureException` and `track` apply to that one item only.

Merge semantics differ by kind:

- `tags` and `extra` merge **shallowly, per key**; the later write wins.
- `contexts` merges **by block name** — a per-call block replaces the
  same-named scope block wholesale rather than deep-merging into it.
- Empty `tags` / `contexts` / `extra` maps are omitted from the wire entirely.

```dart
// init:      tags {env_tag: seed}, contexts {order: {id: 1}}, extra {boot: true}
Sauron.setTag('env_tag', 'runtime');          // overrides the seed
Sauron.setContext('cart', <String, Object?>{'items': 2});
Sauron.client!.track(
  'checkout',
  tags: <String, String>{'env_tag': 'call'},  // wins over scope
  contexts: <String, Map<String, Object?>>{
    'order': <String, Object?>{'id': 99},     // replaces the seeded block
  },
);
// on the wire: tags {env_tag: call}
//              contexts {order: {id: 99}, cart: {items: 2}}
//              extra {boot: true}
```

**User.** `setUser` replaces the whole user. `identify(id, traits:)` sets the
user's `id` to `id`, preserves the existing `email`, and preserves existing
traits when `traits` is `null`. The resulting user is serialized into
`context.user` on every envelope; `distinct_id` on events/transactions comes
from `user.id`.

**Breadcrumbs.** A FIFO ring buffer capped at `maxBreadcrumbs`. A snapshot is
attached to each `ErrorItem` at capture time; the buffer is not cleared
afterwards, so a later error still carries the same history.

**Screen.** `options.screen` seeds it, `setScreen` changes it (emitting one
`$screen` event per change), `SauronNavigatorObserver` can drive it from named
routes, and `screen:` on `captureException` / `client.track` overrides it for a
single item.

## Flutter integration

### Bootstrap

```dart
Future<void> main() async {
  await Sauron.init(
    SauronOptions(dsn: 'https://pk_test@localhost:8081/1'),
    appRunner: () => runApp(const MyApp()),
  );
}
```

`appRunner` is the supported path: it calls
`WidgetsFlutterBinding.ensureInitialized()`, `installIntegrations()` and
`bootstrap()` **inside** `runZonedGuarded`, so binding-owned callbacks and any
failure during startup are captured too. If you must control `runApp` yourself,
omit `appRunner` — you keep layers 1-3 and lose the zone catch-all.

#### Startup ordering

Flutter records the zone the binding was created in and asserts `runApp` still
runs in it. So the zone layer is only available when Sauron initializes the
binding — that is, when nothing touched it first:

```dart
Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();   // ← binding pinned to this zone
  final config = await loadConfig();
  await Sauron.init(
    SauronOptions(dsn: config.dsn),
    appRunner: () => runApp(MyApp(config: config)),   // would be a second zone
  );
}
```

Sauron detects this and runs your app in the current zone instead, so you never
see Flutter's `Zone mismatch.` assertion — but layer 4 is skipped (with `debug:
true` the SDK logs that it did). Layers 1-3 still catch everything:
`PlatformDispatcher.onError` is Flutter's supported catch-all for async errors
outside a guarded zone.

To keep all four layers, move the pre-`runApp` work into `appRunner`, which
already runs after `ensureInitialized()` inside the zone:

```dart
Future<void> main() async {
  await Sauron.init(
    SauronOptions(dsn: const String.fromEnvironment('SAURON_DSN')),
    appRunner: () async {
      final config = await loadConfig();   // binding is up, same zone as runApp
      runApp(MyApp(config: config));
    },
  );
}
```

If your DSN itself comes from that async work, keep the pre-init version — the
zone layer is the only thing you give up.

### Error capture layers

| # | Layer | Installed by | Mechanism `type` | `handled` | Level |
| --- | --- | --- | --- | --- | --- |
| 1 | `FlutterError.onError` | `installIntegrations()` | `FlutterError.onError` | `false` | `error` |
| 2 | `PlatformDispatcher.instance.onError` | `installIntegrations()` | `PlatformDispatcher.onError` | `false` | `error` |
| 3 | `Isolate.current.addErrorListener` | `installIntegrations()`, skipped on web | `Isolate.addErrorListener` | `false` | `fatal` |
| 4 | `runZonedGuarded` | passing `appRunner:` to `init` | `runZonedGuarded` | `false` | `error` |

Layer 1 chains the previous `FlutterError.onError` (so the debug red screen and
any existing console reporting survive), falling back to
`FlutterError.presentError` if there was none. Layer 2 chains the previous
handler and returns `true`, marking the error handled. Layer 3 covers the
current isolate only — register isolates you spawn with
`Sauron.addIsolateErrorListener(isolate)`. Manual `captureException` calls
default to `Mechanism(type: 'manual', handled: true)`.

Each layer installs at most once per process. `close()` uninstalls all of them,
restoring the handlers they replaced.

### `SauronNavigatorObserver`

```dart
SauronNavigatorObserver(
  SauronClient client, {
  bool recordTransactions = true,
  bool trackScreens = true,
})
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `client` | `SauronClient` | required | Usually `Sauron.client!`. |
| `recordTransactions:` | `bool` | `true` | Emit a `navigation` transaction for the route being left, timed by its dwell duration. |
| `trackScreens:` | `bool` | `true` | Drive `setScreen` from `route.settings.name` on each change. |

Records a `navigation`/`route` breadcrumb on every push, pop, replace and remove
(with `data: {'operation': …}`), naming unnamed routes `<unnamed>`. Transactions
and screen tracking both require `route.settings.name`, so unnamed routes
contribute neither. Instrumentation failures are swallowed — it never breaks
navigation.

```dart
MaterialApp(
  navigatorObservers: <NavigatorObserver>[
    if (Sauron.client != null) SauronNavigatorObserver(Sauron.client!),
  ],
  home: const HomePage(),
);
```

The two switches are independent. Set `recordTransactions: false` to attribute
events to screens without emitting navigation timings, or `trackScreens: false`
for the reverse.

```dart
// Screen attribution only — no navigation transactions.
SauronNavigatorObserver(Sauron.client!, recordTransactions: false);
```

### `SauronWidgetsBindingObserver`

```dart
SauronWidgetsBindingObserver(SauronClient client)
static void install(SauronClient client)
static void uninstall()
```

Installed automatically by `installIntegrations()` as a process-wide singleton.
It records a `navigation`/`app.lifecycle` breadcrumb on every lifecycle change
and flushes the transport on `paused` and `detached`, so buffered data survives
backgrounding. You only need the class directly if you manage the binding
yourself.

### Device, app and privacy

`bootstrap()` collects, once, and caches:

- **device** — Android: manufacturer / model / first supported ABI; iOS:
  `Apple` / `utsname.machine`; macOS: `Apple` / model / arch; Windows: `PC` /
  product name; Linux: name / pretty name.
- **os** — name and version per platform.
- **app** — `version` and `build`, taken verbatim from the `appVersion` /
  `appBuild` options. Not read from the platform; omitted entirely when neither
  is set. See [Supplying app version](#supplying-app-version).
- **runtime** — `Dart` plus the major.minor from `Platform.version`.
- **device_id** — a UUIDv4 minted on first run and persisted to
  `<app-support>/sauron/sauron_prefs.json` under the key `sauron.device_id`.
  The backend treats it as the stable device identity. It is per-install, not
  per-user: uninstalling the app resets it, and nothing links it to a hardware
  identifier. Every plugin read is guarded — a failure yields `null` fields
  rather than a lost error report.

The stable device id and the per-launch `sessionId` are the only identifiers the
SDK creates on its own. Everything else about a user comes from your
`identify` / `setUser` calls, and `beforeSend` is the escape hatch for redacting
any of it before it leaves the device.

## Stack traces & symbolication

Debug/JIT traces are parsed on-device into normalized `StackFrame`s
(`function`, `filename`, `lineno`, `colno`, `in_app`).

Release AOT builds are different: the trace is program-counter offsets, not
names. When the raw trace contains `isolate_dso_base` or `build_id:`
(`isObfuscatedDartTrace`), the SDK additionally ships:

- `raw_stacktrace` — the verbatim trace string, and
- `debug_meta` — `build_id` and `isolate_dso_base` parsed out of its header,

so the server can resolve the addresses against the matching
`--split-debug-info` ELF via DWARF. Frames are never symbolicated on-device.
Build with `--split-debug-info=<dir>` (the flag that produces the symbol file;
usually paired with `--obfuscate`) and upload the result before you need to read
a trace:

```bash
sauron-symcli upload-dart --api <url> --token <jwt> --app <uuid> \
    --platform android --arch arm64 --debug-id <build-id> app.symbols
```

Without an uploaded symbol file the dashboard falls back to showing the raw
trace.

## Transport & delivery

- **Batching.** Items are buffered in memory and packed into an envelope on a
  `flushInterval` timer (default 5s), when the buffer reaches `maxBatchItems`
  (30), when an error is captured, on app `paused`/`detached`, and on `flush()`
  / `close()`.
- **Envelope bounds.** The buffer is packed into chunks of at most
  `maxItemsPerEnvelope` (1000) so a backlog never goes out as a single
  oversized request.
- **Compression.** Bodies ≥ `gzipThresholdBytes` (1024) are gzipped with
  `dart:io`'s `GZipCodec` and sent with `Content-Encoding: gzip`. A compile-time
  conditional import supplies a no-op stub where `dart:io` is absent.
- **Offline queue.** Every envelope is appended to a JSONL file at
  `<app-support>/sauron/queue.jsonl` **before** the send is attempted, so it
  survives an app kill. Total size is capped at `maxQueueBytes` (5 MiB) with
  FIFO eviction of the oldest entries; the newest entry is always retained even
  if it alone exceeds the cap. A corrupt or unreadable queue file is discarded
  rather than crashing the app.
- **Draining.** The queue is drained on `bootstrap()` (picking up a previous
  session's envelopes), on the flush timer, when a batch fills, and when the app
  returns to the foreground (`AppLifecycleState.resumed`). The SDK ships no
  connectivity plugin — the HTTP response is the only reachability signal it
  trusts, so a backlog accumulated while offline moves on the next flush tick or
  the next resume, whichever comes first.
- **Retry.** Exponential backoff with full jitter — `min(30, 2^attempt)`
  seconds plus 0-999 ms, capped at 30 s. The attempt counter resets on the first
  success.

Request shape:

```
POST /api/{environment_id}/envelope
Content-Type: application/json
Content-Encoding: gzip            # when compressed
X-Sauron-Key: <public_key>

{ "header": {…}, "context": {…}, "items": [ … ] }
```

Response policy:

| Status | Action |
| --- | --- |
| 200 / 202 | Success — remove from the queue. |
| 400 | Drop, no retry. |
| 401 / 403 | Drop **and disable** the transport for this process. |
| 413 | Split the envelope in half and re-queue both. A single-item envelope is dropped. |
| 429 | Retry, honoring `Retry-After` — an integer number of seconds, or any date `DateTime.tryParse` accepts (ISO-8601). |
| 408 / 5xx / network error | Retry with backoff + jitter. |
| any other 4xx | Drop, no retry. |

## Platform support

| Platform | Supported | Notes |
| --- | --- | --- |
| Android | yes | All four layers, offline queue, gzip. |
| iOS | yes | All four layers, offline queue, gzip. |
| macOS / Windows / Linux | yes | Same as mobile. |
| Web | no | `lib/src/client.dart`, `queue.dart`, `device_id_store.dart` and `device_context.dart` import `dart:io` unconditionally, and `path_provider` has no web implementation, so a web build does not compile. The `kIsWeb` guards and the gzip stub are groundwork, not a shipped target — use `@edraj/sauron-browser` for the web. |

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| Nothing arrives, no logs | `dsn` unset/empty, so the SDK is disabled | Pass `dsn:` to `SauronOptions`; check `Sauron.isEnabled`. |
| Nothing arrives, `[Sauron] invalid DSN, SDK disabled` | DSN failed to parse | Use `https://<public_key>@<host>[:port]/<environment_id>`; the environment id is the last path segment. |
| Requests leave but nothing lands | Your proxy does not expose ingest at `/api/{environment_id}/envelope` on the DSN's host (plus any DSN path prefix) | Route that exact path to the gateway — events otherwise drop silently and look delivered. |
| Delivery stops permanently mid-session | A `401`/`403` disabled the transport (`Sauron.isEnabled` flips to `false`) | Verify the public key belongs to the project; restart the app after fixing. |
| Events arrive, errors do not | `sampleRate < 1.0`, or `beforeSend` returned `null` | Pass `sampleRate: 1.0`; log inside `beforeSend`. |
| No breadcrumbs on errors | `maxBreadcrumbs <= 0` | Set a positive `maxBreadcrumbs`. |
| Stack traces are hex addresses | Obfuscated AOT build with no symbols uploaded | Upload the `--split-debug-info` output (see above). |
| Errors from a spawned isolate are missing | Only `Isolate.current` is auto-listened | Call `Sauron.addIsolateErrorListener(isolate)`. |
| Nothing captured after `close()` | `close()` is terminal: it disables the client and uninstalls the capture layers | Treat `close()` as end-of-process; do not re-`init`. |
| Web build fails on `dart:io` | Web is not a supported target | Use `@edraj/sauron-browser`. |
| Need to see what the SDK is doing | — | `debug: true` prints `[Sauron] …` lines (sampling drops, `beforeSend` drops, invalid DSN, network errors, retry schedule, non-retryable drops). |

## Development

```bash
flutter pub get
flutter analyze     # flutter_lints + strict-casts + strict-raw-types
flutter test
```

Run the sample app (all four layers, `track`, `identify`, `flush` wired to
buttons):

```bash
cd example
flutter pub get
flutter run
```

`test/envelope_test.dart` holds the locked golden envelope shape that guards
byte-for-byte parity with the Rust backend and the other SDKs — update it only
alongside the wire contract.

## License

AGPL-3.0-only — GNU Affero General Public License v3.0.

Repo: https://github.com/edraj/sauron — wiki:
https://github.com/edraj/sauron/wiki
