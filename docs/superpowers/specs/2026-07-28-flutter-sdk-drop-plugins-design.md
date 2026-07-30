# Flutter SDK: drop `connectivity_plus`

**Date:** 2026-07-28
**Component:** `sdks/flutter` (`sauron_flutter`)
**Status:** design — approved in scope, awaiting implementation
**Scope:** SDK + docs only. **No backend changes. No API changes.**

## Decision

Remove `connectivity_plus`. **Keep `device_info_plus`** — see "Considered and
rejected" below, which is the more valuable half of this document.

Dependencies go **4 → 3**: `http`, `device_info_plus`, `path_provider`.

## Motivation

`connectivity_plus` injects
`<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE"/>`
into every consumer app's merged manifest. Consumers inherit a permission they
must explain in store listings and privacy reviews, in exchange for a latency
optimization the SDK already treats as advisory.

Both `lib/src/transport/connectivity.dart:9` and
`lib/src/transport/transport.dart:217` state that connectivity is only a hint and
that the authoritative signal is the HTTP response.

## What it actually buys today (measured, not assumed)

`ConnectivityMonitor` has two members. Only one is wired up:

| Member | Used? |
|---|---|
| `start(onOnline)` → `_drainQueue()` (`transport.dart:90`) | Yes — the only real use |
| `isOnline` getter | **No — dead code.** Nothing in the SDK calls it |

Because `isOnline` is never consulted, there is **no pre-flight send gate**, so
removing the package adds zero wasted HTTP attempts while offline. The offline
path is unchanged: persistent JSONL queue + `_Outcome.retry` backoff.

The single lost behaviour is therefore: *drain immediately on regaining
connectivity*, rather than at the next flush tick.

## Design

### 1. Replace the trigger with a lifecycle resume drain

`flush()` (`transport.dart:108`) is `_packBufferIntoQueue()` + `_drainQueue()` —
a strict **superset** of the connectivity callback, which called `_drainQueue()`
alone. So a resume-triggered `flush()` fully replaces it.

`SauronWidgetsBindingObserver`
(`lib/src/integrations/widgets_binding_observer.dart`) is already installed at
`client.dart:84` and already flushes on `paused` / `detached`. It does **not**
currently handle `resumed` — verified, nothing in `lib/` references
`AppLifecycleState.resumed`.

Add `resumed` to the flush condition. No new machinery, no new dependency, and
it closes a real gap: today, returning to a foregrounded app does not trigger a
drain at all.

### 2. Removals

- Delete `lib/src/transport/connectivity.dart`.
- `lib/src/transport/transport.dart`: drop the import (`:11`), constructor
  parameter (`:47`), field initialiser (`:55`), field (`:64`), the listener
  registration (`:90–92`), the `dispose()` call (`:127`), and correct the
  `start()` doc comment (`:80`).
- `lib/src/client.dart:107`: drop `connectivity: ConnectivityMonitor(),`.
- `pubspec.yaml`: drop `connectivity_plus: ^6.1.0`.

### 3. Documentation

- `README.md:21` — "Four package dependencies" → three, drop `connectivity_plus`.
- `README.md:534` — `close()` no longer "disposes the connectivity listener".
- `README.md:905–906` — rewrite the drain-trigger list: bootstrap, flush timer,
  batch-full, and **app resume**.
- `CHANGELOG.md` — non-breaking entry; call out the manifest permission removal
  as the user-visible win.

## Behavioural delta

| | Before | After |
|---|---|---|
| Drain triggers | bootstrap, flush timer, batch full, connectivity regained | bootstrap, flush timer, batch full, **app resume** |
| Offline sends | queued + backoff | unchanged |
| `ACCESS_NETWORK_STATE` in consumer manifest | yes | **no** |
| Public API | — | unchanged |

Worst case: the app is foregrounded and connectivity returns without a lifecycle
transition — the drain waits for the next flush tick, bounded by
`options.flushInterval`. Acceptable; that is already the behaviour for every
other queue-drain path.

## Testing

- Transport constructs and drains with no connectivity monitor.
- `didChangeAppLifecycleState(resumed)` triggers a flush. No existing test
  references connectivity, so this is net-new coverage.
- Build the example APK and confirm `ACCESS_NETWORK_STATE` is absent from the
  merged manifest (`aapt dump permissions`).

---

## Considered and rejected: removing `device_info_plus`

Investigated in depth and **rejected**. Recorded here because the evidence cost
a device build to obtain and the question will recur.

The intent was a plugin-free SDK. Measured on a Xiaomi Redmi Note 10
(`M2103K19C`, Android 13, Dart 3.12.2):

```
Platform.operatingSystemVersion   TP1A.220624.014      <-- ro.build.id
getprop ro.build.version.release  13                   <-- what you want
```

**`Platform.operatingSystemVersion` on Android returns the build fingerprint,
not the Android release.** There is no pure-Dart path to `13`, and mapping build
IDs to versions is an unmaintainable lookup table. Android is the only platform
with this defect — iOS/macOS (`NSProcessInfoOperatingSystemVersionString`),
Windows (`"<product>" <nt> (Build <n>)`) and Linux (`uname`) all return usable
values.

So dropping `device_info_plus` would have cost `device.family`, `device.model`
**and** Android `os.version` — with only `device.arch` recoverable for free
(`Platform.version` ends in `on "android_arm64"`).

Mitigations considered and their costs:

- **Developer-supplied descriptors at init** (the `appVersion`/`appBuild` pattern
  from 1.2.0): pushes a six-branch `defaultTargetPlatform` switch into every
  consumer app, and puts an `await` ahead of `Sauron.init` so failures during it
  are uncaptured.
- **An async `deviceProvider` callback**: better — `Sauron.init` stays first, the
  SDK owns caching and error-guarding — but needs a timeout so a hanging
  provider cannot stall bootstrap.
- **A companion `sauron_flutter_device_info` package**: satisfies "optional and
  not carried by the core SDK" cleanly, but adds a second pub.dev package to
  publish, version and maintain.

**Rejected** because the data loss is real and the mitigations each cost more
than the dependency does. `device_info_plus` collects only coarse, non-identifying
fields (manufacturer, model, ABI) and injects **no manifest permission**, so it
does not carry the privacy cost `connectivity_plus` does. The per-install
`device_id` comes from the first-party `DeviceIdStore`, not the plugin.

### Related findings worth keeping

- **`path_provider` must stay.** `Directory.systemTemp` on Android is
  `/data/user/0/<pkg>/code_cache`, wiped on every app update — persisting
  `device_id` there would regenerate it each release, making every updated
  install look new.
- **`README.md:40` is misleading.** It attributes the Android toolchain floor to
  `device_info_plus`. True only at current lock pins: `path_provider_android`
  2.2.18+ independently requires AGP 8.12.1–8.13.1. Worth correcting whenever
  that file is next touched.
- **If the toolchain floor ever becomes a priority**, pinning
  `device_info_plus: ^11.5.0` drops it from AGP 8.12.1 / Kotlin 2.2.0 to
  AGP 8.3.1 / Kotlin 1.7.22 at **zero data cost** — a far better trade than
  removing the package.
- **`EnvelopeContext.os/device/app/runtime` are `serde_json::Value`**
  (`backend/crates/sauron-core/src/envelope.rs:76`), deliberately free-form, so
  SDKs can add platform-specific keys with no backend or migration work.
