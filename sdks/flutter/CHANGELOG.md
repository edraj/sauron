# Changelog

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
