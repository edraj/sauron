# Sauron — Flutter SDK Demo

A small Material 3 Flutter app that showcases the **`sauron_flutter`** SDK
end-to-end: it pushes crashes and product events to a running Sauron ingest
gateway so you can watch them land in the dashboard.

It depends on the local SDK via a path dependency:

```yaml
dependencies:
  sauron_flutter:
    path: ../../sdks/flutter
```

## What it does

`main()` reads the persisted config and calls `Sauron.init(..., appRunner:)`,
which launches the app inside `runZonedGuarded` and binds all four uncaught
capture layers (`FlutterError.onError`, `PlatformDispatcher.onError`,
`Isolate.addErrorListener`, and the guarding zone).

The home screen has an editable **DSN / environment / release** section, a
`distinct_id` field, and buttons wired to the SDK:

| Button | What it exercises |
| --- | --- |
| **Throw uncaught (sync)** | `throw StateError(...)` in a gesture callback → `FlutterError.onError` layer |
| **Async gap error** | an unawaited future that throws → `PlatformDispatcher` / zone layer |
| **captureException (handled)** | `Sauron.captureException` with a synthetic error + stack trace |
| **addBreadcrumb then throw** | records a breadcrumb, then crashes (crash carries the trail) |
| **track: checkout_completed** | `Sauron.track` with a random cart value |
| **track: screen_viewed** | `Sauron.track` for the demo screen |
| **setScreen (navigate)** | v0.2.0 screen API — `Sauron.setScreen(...)` toggling `Home ⇄ Checkout` |
| **identify** | `Sauron.identify(distinctId, traits:)` + `setUser` |
| **Flush now** | `Sauron.flush()` — drains batched + queued envelopes |

Every action is appended to an in-app **activity log**, and a footer points you
to the dashboard.

### Screen tracking (v0.2.0)

`Sauron.setScreen(name)` sets the active screen: on a change it emits a
`$screen` view and tags every later `track()` / `captureException()` call with
that screen (read it back via `Sauron.screen`).

- **Automatic** — the exported `SauronNavigatorObserver` is wired into
  `MaterialApp.navigatorObservers` (`lib/main.dart`) and drives `setScreen`
  from **named** routes on every navigation (plus a `navigation` transaction
  timed by dwell).
- **Explicit** — this demo's home route is unnamed, so it calls
  `Sauron.setScreen('Home')` in `initState`, and the **setScreen (navigate)**
  button toggles `Home ⇄ Checkout` to show change detection.

## Showcase funnels, journeys & performance

The single-event buttons are great for Issues/Events, but one user tapping
buttons is a single `distinct_id` — a flat, single-path funnel. The **Run
showcase** card (top of the screen) fixes that: one tap drives the SDK through a
synthetic e-commerce cohort — **~120 users by default** (editable, capped at
500), each switched via `setUser` so their events keep their own `distinct_id`.

Each synthetic user walks the funnel
`product_viewed → product_added_to_cart → checkout_started → payment_info_entered → checkout_completed`
with realistic **drop-off**, branches into side events (`search_performed`,
`viewed_recommendations`, `applied_coupon`) for the journey graph, and emits a
spread of `trackTransaction()` calls (screen loads, `GET /api/products`,
`POST /api/checkout`, resource loads) with skewed latencies. The card renders the
resulting funnel inline when it finishes.

After a run, open the **dashboard → Flutter Demo app**:

- **Funnels** — prefilled with the first three steps; add the rest for the full
  5-step conversion.
- **Journeys** — the branching Sankey of paths through the cohort.
- **Performance** — p50/p95/p99 and latency badges over the transactions.

The simulator is pure logic + an injected sink (`lib/showcase.dart`), unit tested
in `test/showcase_test.dart` (`flutter test`).

## Run it

```bash
cd examples/flutter-app
flutter pub get
flutter run          # pick a device: android, ios, chrome, …
```

Then open the **Sauron dashboard → Flutter Demo app → Issues / Events** to see
the crashes and events arrive.

### Verify (no device needed)

```bash
flutter pub get
flutter analyze      # zero issues
flutter test         # widget smoke tests
```

## DSN

The DSN is pre-filled and editable in the app; it is persisted with
`shared_preferences`. The default points at the running **dev** ingest:

```
http://pk_2f587381b889049a0a21fd619a7ba41d@localhost:8091/b13ff85c-ccd1-450e-95a5-fcc52f7650a3
```

That resolves to the envelope endpoint:

```
POST http://localhost:8091/api/b13ff85c-ccd1-450e-95a5-fcc52f7650a3/envelope
X-Sauron-Key: pk_2f587381b889049a0a21fd619a7ba41d
```

Editing DSN / environment / release binds at **startup**, so the app shows a
"restart to apply" banner after you save those — restart to re-point every
capture layer. (The `distinct_id` field is read live by **identify**.)

### Ports

- **Local dev ingest:** `:8091` (the default DSN above).
- **Docker Compose:** the ingest is published on **`:8081`** instead
  (`docker compose up`). Swap the port in the DSN to `8081` when running the
  stack that way.

### Android emulator

On an Android emulator, `localhost` is the emulator itself — the host machine is
reachable at **`10.0.2.2`**. Edit the DSN host accordingly, e.g.:

```
http://pk_2f587381b889049a0a21fd619a7ba41d@10.0.2.2:8091/b13ff85c-ccd1-450e-95a5-fcc52f7650a3
```

(iOS simulators and desktop/web can use `localhost` directly.)

## Connectivity check

You can prove the DSN is wired to a live backend without launching the app by
POSTing a minimal envelope and confirming an HTTP `202`:

```bash
curl -i -X POST \
  "http://localhost:8091/api/b13ff85c-ccd1-450e-95a5-fcc52f7650a3/envelope" \
  -H "Content-Type: application/json" \
  -H "X-Sauron-Key: pk_2f587381b889049a0a21fd619a7ba41d" \
  -d '{
    "header": { "sdk": { "name": "sauron.flutter", "version": "0.1.0" },
      "environment": "development" },
    "items": [ { "type": "error", "exception": { "type": "ConnectivityProbe",
      "value": "hello from curl" } } ]
  }'
# → HTTP/1.1 202 Accepted   {"accepted":1}
```
