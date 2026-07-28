# Changelog

## Unreleased

- **Breaking: `package_info_plus` dropped; app version is now developer-supplied.**
  The SDK no longer reads `context.app` off the platform. Set the new
  `appVersion` / `appBuild` options at init — e.g. from `--dart-define`, a
  generated constants file, or your own `package_info_plus` call if you already
  depend on it. When both are unset the `app` block is omitted; nothing else
  changes.

  **Action required:** apps relying on automatic app version/build must set
  these two options, or `context.app` goes null. `release` is unaffected.

  This removes two transitive dependencies (`package_info_plus` and
  `package_info_plus_platform_interface`). It does **not** lower the Android
  toolchain floor (AGP `>=8.12.1`, Gradle `>=8.13`, Kotlin `2.2.0`), which
  `device_info_plus ^12.0.0` imposes independently.

- **Fixed: `trackScreens` did nothing unless `recordTransactions` was also on.**
  `SauronNavigatorObserver` documented the two flags as independent, but the
  `setScreen` call sat behind the `recordTransactions` early-return, so
  `SauronNavigatorObserver(client, recordTransactions: false)` silently stopped
  attributing events to screens. The two switches are now genuinely independent.

- **Fixed: unbounded memory growth after `close()`.** `close()` dropped the
  transport but left the client accepting work, so every subsequent capture
  appended to the pre-bootstrap replay buffer, which was never drained again —
  in a long-lived process that closed the SDK, this grew without limit. `close()`
  is now terminal and idempotent: `isEnabled` flips to `false`, later captures
  are dropped, the buffer is cleared, and the four capture layers are
  uninstalled so the handlers they replaced are restored.

## 1.0.0 - 2026-07-27

First public release. Prior `0.x` versions were internal-only and were never
published to pub.dev.

- **Fixed: oversized envelopes.** `maxBatchItems` only *triggers* a flush; it does not bound
  the request, so a producer outpacing delivery (offline, or mid-retry) could build an
  envelope past the server's 1000-item limit and have it dropped as a non-retryable `400`.
  The buffer is now packed into chunks of at most `maxItemsPerEnvelope` (default 1000).

## 0.3.0

- **Breaking / behavioral change — `beforeSend` now runs on every item.**
  Previously `beforeSend` was invoked for errors only; analytics events,
  identifies, and transactions bypassed it. It now runs on **every** outgoing
  item just before it is enqueued for delivery, so you can redact, mutate, or
  drop any item type (return the item to send it, `null` to drop it).
  - `BeforeSendCallback` widened from `ErrorItem? Function(ErrorItem)` to
    `Object? Function(Object item)`. Update your hook's signature to accept
    `Object` and guard on the runtime type if you only want to act on errors,
    e.g. `if (item is! ErrorItem) return item;`. Existing error-only logic keeps
    working — an error is still passed through as an item.

## 0.1.0

Initial release.

- Four-layer uncaught error capture: `FlutterError.onError`,
  `PlatformDispatcher.onError`, `Isolate.addErrorListener`, and
  `runZonedGuarded`.
- Breadcrumbs with a bounded ring buffer; app-lifecycle and navigation
  breadcrumb integrations.
- Product analytics: `track()` and `identify()`.
- `captureException`, `setUser`, `addBreadcrumb`, `flush`, `close`.
- Batching transport with gzip compression (`dart:io`, skipped on web / under
  ~1&nbsp;KB) and the full ingest response policy
  (202/400/401/403/413/429/408/5xx).
- Durable offline JSONL queue in the app-support directory with a byte cap +
  FIFO eviction, drained on init and on connectivity changes.
- Device/OS/app/runtime context via `device_info_plus` + `package_info_plus`.
- JIT and AOT/obfuscated Dart stack-trace parser.
- Golden-shape envelope test guarding parity with the backend and JS SDK.
