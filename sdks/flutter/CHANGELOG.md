# Changelog

## 1.8.0 - 2026-08-16

### Added

- **`tags` and `extra` on transactions.** `trackTransaction` now accepts two
  developer-supplied maps: `tags` (flat string→string, indexed) and `extra`
  (freeform JSON). `extra` is where a request body, a response body, an order
  id or a retry count goes — the span that times an HTTP call can now carry
  what the call actually sent and received.

  Both are visible on the new **Transactions** page and in the session
  timeline, and both are searchable: `@tag.tier:premium`,
  `extra.order_id:9001`, or `extra.response:~9001` to match a substring
  *inside* a stored response body.

  **They are per-call only.** Unlike `captureException` and `track`, a
  transaction does not inherit the scope — `setTag()` / `setExtra()` defaults
  are not merged in. Transactions are the highest-volume signal an app emits,
  one per navigation and per request, so inheriting a global blob would write
  it onto every row. This asymmetry is deliberate and is documented on the
  method.

  `extra` is serialized and capped at **16 KB**. Past that the whole map is
  replaced with a `{"_truncated": true, "_bytes": N}` marker, and the
  dashboard says so on the row rather than showing a short object that looks
  complete. The cap is not cosmetic: envelopes are batched, and one oversized
  body would push the whole envelope past the ingest limit and take every
  unrelated span sent with it — a silent loss of data nobody asked about.
  Size is measured in UTF-8 bytes, so non-ASCII payloads are counted at what
  they actually cost on the wire.

  Nothing in `extra` is scrubbed. `beforeSend` remains the redaction seam;
  think twice before attaching a body that can carry tokens or personal data.

  An app that sets neither field serializes byte-identically to before: both
  keys are omitted when empty, never sent as `null`.

  Signature: `trackTransaction({..., Map<String, String>? tags, Map<String, Object?>? extra})`. `startTransaction` accepts both too, and `ActiveTransaction` exposes them as **mutable fields** — the interesting facts about a call are known after it returns. `end(tags:, extra:)` replaces each map wholesale, matching how `status`/`httpStatus`/`url` already behave there; mutate the field to add to what is set.

## 1.7.0 - 2026-08-14

*Minor, not patch: `identify()`'s signature change below is source-breaking
(`void` → `Future<void>`), consistent with 1.2.0's precedent for other breaking
changes.*

- **Breaking: `Sauron.identify()` / `SauronClient.identify()` now return
  `Future<void>` instead of `void`.**

  **If you do nothing, you lose nothing.** `Sauron.identify('u_123');` with no
  `await` still compiles — Dart doesn't require awaiting a `Future`, and `void`
  is a top type in return position, so `void Function(String) cb =
  Sauron.identify;` also compiles with no diagnostic. Detecting an identity
  switch (below) needs to read persisted on-device state, so the identify item
  is now queued after an asynchronous gap instead of synchronously.

  An earlier draft of this entry claimed an un-awaited call gets "*exactly*
  the durability an un-migrated caller already had". That is not true, and the
  comparison it makes is not available: before this change `identify()`
  persisted **nothing** — there was no last-identified record to write — so
  there is no prior durability to be unchanged from. What is actually true is
  narrower and worth stating plainly: an un-awaited `identify()` returns
  before the switch-detection record has been written to disk, so a process
  death in that window loses the record, and the NEXT launch cannot detect a
  user switch. Nothing else is at risk: the identify item is queued either
  way, and within a single process run the in-process copy of the digest
  (`_lastIdentifiedDigest`) makes detection work even if the file write never
  lands at all. `await` closes the window. Async is otherwise simply the
  better shape for *new* code — most
  prominently, code that wants `flush()` immediately after `identify()` to
  actually include it — so it is being adopted now rather than left to a
  future breaking release.

  This is nonetheless a real break for two narrower cases:
  - A hand-written fake or `implements SauronClient` (`void identify(...)`) is
    a **hard compile error** (`invalid_override`) — not a silent behavior
    change, `flutter analyze`/`dart analyze` will stop your build.
  - Consumers on a strict lint set (e.g. `very_good_analysis`, or any config
    enabling `discarded_futures`/`unawaited_futures`) will see new
    diagnostics on existing `identify(...)` call sites, which fails CI under
    `--fatal-infos` or `--fatal-warnings` until those sites are updated
    (`await` it, or wrap in `unawaited(...)` if it's genuinely fire-and-forget).

- **Auto-reset on identity switch.** `identify()` now detects a login by a
  DIFFERENT user than last time on this device — the common case of a
  forgotten `reset()` on logout — and mints a fresh anonymous id (and rotates
  the session id) before sending, so `anonymous_id` is `null` instead of an
  alias to the previous person. This can't undo an alias already sent under
  the old id — still call `reset()` on logout — but it bounds a missed
  `reset()` to one corrupted guest window instead of every one after it. To
  detect the switch, `identify()` persists a short one-way digest (never the
  raw id; see `hashIdentity` in `lib/src/context/last_identified_store.dart`)
  of the last identified user in `<app-support>/sauron/sauron_prefs.json`
  under `sauron.last_identified` — the same key name and digest algorithm the
  browser SDK uses in `localStorage`. Like the anonymous id, this is a durable
  first-party value stored on the user's device — a retention and consent
  consequence, not just an implementation detail.

  The stored value carries a format tag: `v1:<digest>`, byte-identical to what
  the browser SDK writes under the same key. A value with no tag or an
  unrecognised one reads as "nobody has identified on this device yet" and is
  rewritten in the current format on the next `identify()`. That matters
  because the digest's shape is not frozen — if it ever widens again, an
  untagged store could not tell "a digest I no longer produce" from "a
  different person", so every returning user's next `identify()` would be read
  as a switch and would rotate their anonymous id and session, once, silently.
  The tag turns that into one missed switch per device instead.

  On a detected switch, `identify()` also **clears the scope user's `email`
  and `traits`** instead of carrying the previous person's forward. The scope
  user is attached to every envelope, so carrying them over stamped person
  A's email onto every event, error and session recorded under person B's
  `distinct_id` — a cross-user leak at exactly the boundary this detection
  exists to police, and one that lasts for the whole process rather than one
  guest window. A *same-user* re-`identify()` (adding traits, refreshing after
  a token renewal) still carries them forward, unchanged. This matches the
  browser SDK, whose `Scope.setUser` has always rebuilt the user from its
  input alone.

  `SauronClient.prepareIdentify` accordingly returns `IdentifyPreparation`
  (`aliasOf` + `switched`) rather than a bare `String?`. Only relevant if you
  call that method directly — `identify()` is unaffected. `aliasOf` alone
  could not express the switch: it is `null` both for a switch and for the
  ordinary "the anonymous id was never used" case.
- `reset()` now also clears the last-identified record and rotates the
  session id (`sessionId`). Clearing last-identified matters so the same
  person logging back in later isn't affected by stale state; rotating the
  session id matters because the server's `bump_session` is last-write-wins
  on `distinct_id`, so without it a single `sessions` row could otherwise end
  up serially representing two different people and recording only whichever
  wrote last. Unlike the browser SDK's `session_id`, Flutter's `sessionId` was
  never persisted to disk (it's minted fresh in memory per launch), so
  rotating it needs no storage I/O.
- Added `SauronClient.prepareIdentify(String id)` — the primitive `identify()`
  calls internally to decide the alias/switch behaviour above. Public because
  it is independently useful to call directly (e.g. to pre-warm the check
  without sending an item yet) and to test.

## 1.6.0

*Follows 1.4.0 on pub.dev directly: 1.5.0 was built but never published, and its
two fixes are folded in here. A minor bump, not a patch — 1.4.0 could not
attribute a pre-login analytics event to anyone, and 1.6.0 counts it under an
anonymous id. Without a version change the builds are indistinguishable in
`header.sdk.version`, and the one-time step in Active Users described below
could not be attributed to an SDK upgrade after the fact.*

- **Fixed: `track()` before `identify()` destroyed the whole envelope.** The
  wire's `AnalyticsItem.distinct_id` is a non-optional `String`, so the `null`
  that 1.4.0 sent was not "one item with a null field" — the gateway failed to
  deserialize the ENTIRE envelope (`400 invalid_envelope`), and because every
  SDK treats a 400 as non-retryable, every unrelated error, transaction and
  identify batched alongside it was dropped too, without a retry and without a
  trace. `setScreen()` and the `$workflow_*` lifecycle events are affected the
  same way, since they emit through `track()`.

  Such an item is now attributed to an anonymous id (below). In the one window
  where neither kind of identity exists yet — tracking before
  `await Sauron.init(...)` has finished — it is dropped rather than sent, and
  the first drop prints regardless of `SauronOptions.debug`, so it cannot stay
  invisible the way this bug did.

- **Anonymous id — `track()` no longer requires `identify()` first.** When no
  user has been identified, `distinct_id` is now a persisted `anon_<uuidv4>`
  stored in `<app-support>/sauron/sauron_prefs.json` under `sauron.anon_id` —
  the same format and key name the browser SDK uses in `localStorage`. This is
  what lets an unidentified person be counted as a person: Active Users is a
  distinct count over `distinct_id` per UTC day, and until now no Flutter event
  before login reached the server attributable to anyone, so those people were
  invisible rather than anonymous.

  **Every Flutter app's reported active-user count rises on the day this is
  adopted.** That is the previously-dropped population arriving, not a
  regression. It is a one-time step, not a trend.

  The id is minted once and **adopted verbatim on every later launch** — never
  re-minted, never reformatted, and an install upgrading from 1.4.0 keeps the
  device id already in that prefs file untouched. An id that changes shape on
  upgrade would read as a crowd of new users who never arrived.

  The anonymous id is a durable first-party identifier stored on the user's
  device. That is a retention and consent consequence, not just an
  implementation detail — see the README.

- **Added: `Sauron.reset()`. Call it on logout.** It clears the user and mints a
  fresh anonymous id. Without it the next person to use the device inherits the
  persisted id, and their first `identify()` aliases the previous person's
  anonymous activity onto the new account, permanently, server-side. Unlike the
  browser SDK, `setUser(null)` does *not* do this for you: persisting the new id
  is asynchronous and `setUser` is not.

- **Added: `Sauron.anonymousId`** — the current anonymous id, or `null` before
  `init` completes.

- **`identify()` now sends `anonymous_id`**, but only when the anonymous id was
  actually used as a `distinct_id` first. A first-ever launch that identifies
  immediately still sends `null`: the server writes a permanent alias row for
  any non-empty value, and a speculative one mis-merges two people forever.

- **Fixed: the prefs file no longer overwrites itself.** `sauron_prefs.json` was
  rewritten wholesale by whichever store touched it last, so the second key
  added to it would have silently deleted the first on the next launch — a
  churning `device_id` splits the device dimension exactly the way a churning
  anonymous id splits the user one. Writes now merge, and keys written by a
  newer SDK version survive a downgrade.

- **Fixed: obfuscated Dart stack traces could not be symbolicated.** Real Dart
  AOT writes both DSO base keys on a single line —
  `isolate_dso_base: 7b9c2b7000, vm_dso_base: 7b9c2b7000` — and the parser took
  the entire remainder of that line, reporting an `isolate_dso_base` of
  `"7b9c2b7000, vm_dso_base: 7b9c2b7000"`, which the backend's hex parse
  rejected outright. Confirmed across all 14 events captured from a real device
  on 2026-08-08. Only the leading token is kept now. Release-build stack traces
  stayed unreadable in the dashboard until this fix; events ingested before it
  carry the malformed value and are not retroactively repaired.

## 1.4.0 - 2026-07-30

- **Workflows** — bound a named span of activity with start / end / cancel, and
  read the active one back. Every event, error and transaction captured while a
  workflow is active is stamped with its `workflow_id` / `workflow_name`, so the
  dashboard can group a whole flow (`checkout`, `password_reset`, …) as one unit.
  Entirely optional: an app that never starts a workflow behaves exactly as before.
- **`beforeSend` can no longer throw into your app.** A hook that raises is logged
  and the item is sent unmodified, rather than the exception escaping through the
  capture call. Returning `null` still drops the item as before.

## 1.3.0 - 2026-07-28

- **`connectivity_plus` dropped — the SDK no longer adds a permission to your
  manifest.** `connectivity_plus` merges
  `<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE"/>`
  into every consuming app, so apps inherited a permission they had to justify
  in store listings and privacy reviews in exchange for a queue-drain latency
  optimization. Dependencies drop from four to three (`http`,
  `device_info_plus`, `path_provider`).

  **No action required**, and no API change. The offline queue, retry/backoff
  and response policy are untouched — connectivity was only ever a hint, and the
  HTTP response has always been the authoritative signal. The `isOnline`
  pre-flight check was never wired into the send path, so this adds no wasted
  requests while offline.

  **Behaviour change:** a backlog queued while offline now drains on the flush
  timer (`flushInterval`, default 5 s) or on app resume, instead of the instant a
  network interface comes back. If you need tighter delivery, lower
  `flushInterval`.

- **Added: the queue now drains when the app returns to the foreground.**
  `SauronWidgetsBindingObserver` already flushed on `paused`/`detached`; it now
  also flushes on `resumed`. This closes a real gap — previously, foregrounding
  an app triggered no drain at all and a backlog waited for the next timer tick.

## 1.2.0 - 2026-07-28

> Contains breaking API changes despite the minor bump — see the three
> **Breaking** entries below. `Sauron.init` now takes a `SauronOptions` object,
> so an upgrade from 1.x needs a one-line edit at your `init` call site.

- **Breaking: the `environment` option has been removed.** An environment is now
  identified by the ingest key it belongs to, not by a string the client sends.
  Create environments in the dashboard under app settings; each one has its own
  DSN. Delete `environment` from your `SauronOptions`/`init` call and swap in the
  DSN of the environment you want to report to.
- **Fixed: `Sauron.init` no longer triggers Flutter's `Zone mismatch.` assertion.**
  When an app called `WidgetsFlutterBinding.ensureInitialized()` before `init` —
  as it must to read config, initialize Firebase or lock orientation — the
  binding was pinned to that zone while `appRunner` launched inside a new
  `runZonedGuarded` zone, so `runApp` reported:

  > Zone mismatch. The Flutter bindings were initialized in a different zone
  > than is now being used.

  The SDK now detects an already-initialized binding and runs the app in the
  current zone instead. Uncaught errors are still captured: outside a guarded
  zone `PlatformDispatcher.onError` is Flutter's catch-all and covers what
  `runZonedGuarded` would have caught. Apps that let Sauron initialize the
  binding are unaffected and keep the zone layer. With `debug: true` the SDK
  logs which path it took. See "Startup ordering" in the README.

- **`debug: true` now logs every item delivered to the server** — errors,
  events, identifies, transactions and breadcrumb batches — one line each, at
  the point the server accepted them:

  ```
  [Sauron] delivered 2 item(s) to http://localhost:8081/api/<project>/envelope:
  [Sauron]   event $screen (distinct_id=u_123, screen=Home, properties={"screen":"Home"})
  [Sauron]   error StateError: Bad state: card declined (level=error, screen=Checkout)
  ```

- **Breaking: `Sauron.init` takes a `SauronOptions` object instead of a configure
  callback,** and `SauronOptions` gained a named constructor covering every field.
  Dart has no overloads, so the callback form is gone rather than deprecated.

  ```dart
  // before
  await Sauron.init((o) {
    o.dsn = dsn;
    o.release = 'app@1.0.0';
  }, appRunner: appRunner);

  // after
  await Sauron.init(
    SauronOptions(dsn: dsn, release: 'app@1.0.0'),
    appRunner: appRunner,
  );
  ```

  **Action required:** rewrite the `init` call. Fields stay mutable, so
  `SauronOptions(dsn: dsn)..debug = true` still works when you set values
  conditionally. `tags`, `contexts` and `extra` passed to the constructor are
  now copied, so mutating your own map afterwards no longer leaks into the SDK.

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
