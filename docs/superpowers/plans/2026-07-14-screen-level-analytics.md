# Screen-Level Analytics — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the SDKs attach an optional `screen` to every event/exception (manual `setScreen` + opt-in auto-detection, auto-emitting a `$screen` view event on change), persist it, and add a dashboard **Screens** section showing per-screen views/events/users/exceptions and time-on-screen (dwell), with event/exception → screen click-through.

**Architecture:** `screen` threads exactly like the existing `session_id`: SDK builders stamp it → envelope item field → pipeline → nullable DB column on `analytics_events`/`error_events`. Screen metrics are computed **on-read** (like funnels/journeys): a new `routes/screens.rs` with two endpoints. Dwell = capped `LEAD`-gap over session-ordered `analytics_events`, attributed to the earlier row's screen. A new Screens list + detail page mirror `DevicesInventory`/`DeviceDetail`.

**Tech Stack:** TypeScript (tsup, vitest) for `@sauron/browser`; Dart/Flutter for `sauron_flutter`; Rust (axum, diesel-async, Postgres) backend; Svelte 5 + TS dashboard; Docker Compose.

## Global Constraints

- **`screen` is a nullable `TEXT`/`String?`/`Option<String>` everywhere** — mirror `session_id` field-for-field (envelope item, pipeline arg, DB column, read model). Reserved screen-view event name is exactly **`$screen`**.
- **Dwell definition (verbatim):** within a session, order `analytics_events` by `occurred_at`; `gap = LEAD(occurred_at) - occurred_at`; attribute each gap to the *current* row's `screen`; **cap each gap at 1,800,000 ms (30 min)**; ignore rows with null `session_id` or null `screen`; `avg_dwell_ms = total_dwell_ms / NULLIF(views, 0)` where `views` = count of `$screen` events.
- **No DB/handler integration-test harness** — unit-test extracted pure logic; verify SQL/handlers/UI end-to-end via docker compose (Task C6).
- **SDK versions:** bump `@sauron/browser` and `sauron_flutter` from `0.1.0` → `0.2.0`.
- **Wire contract is load-bearing** (`sdks/js/src/types.ts` header): add fields, never reorder/rename existing ones. Both SDKs and the Rust envelope must agree on `screen`.
- **Commits:** the maintainer disabled auto-commit — stage each `git commit` step and get their OK (or batch) at execution time.
- **RBAC:** the two new endpoints use `authorize_app(..., perm::EVENT_READ)`.

---

# PART C1 — JS SDK (`@sauron/browser`)

### Task C1.1: `screen` state module + wire-type fields

**Files:**
- Create: `sdks/js/src/screen.ts`
- Modify: `sdks/js/src/types.ts`

**Interfaces:**
- Produces: `getScreen(): string | null`, `setScreenState(name: string | null): boolean` (returns `true` if changed), `resetScreen(): void`; `screen?: string | null` on `EventItem` + `ErrorItem`; `screen?: string` + `screenTracking?: boolean` on `InitOptions`; `screenTracking: boolean` on `ResolvedOptions`.

- [ ] **Step 1: Write the failing test for the state module**

Create `sdks/js/test/screen.test.ts`:

```ts
import { describe, expect, it, beforeEach } from 'vitest';
import { getScreen, setScreenState, resetScreen } from '../src/screen.js';

describe('screen state', () => {
  beforeEach(() => resetScreen());

  it('starts null', () => {
    expect(getScreen()).toBeNull();
  });

  it('reports a change and stores the value', () => {
    expect(setScreenState('Home')).toBe(true);
    expect(getScreen()).toBe('Home');
  });

  it('reports no change for the same name', () => {
    setScreenState('Home');
    expect(setScreenState('Home')).toBe(false);
  });
});
```

- [ ] **Step 2: Run it — expect failure**

Run: `cd sdks/js && npx vitest run test/screen.test.ts`
Expected: FAIL — cannot resolve `../src/screen.js`.

- [ ] **Step 3: Create the state module**

`sdks/js/src/screen.ts`:

```ts
/**
 * Current-screen state. The SDK stamps this on every event/exception and
 * auto-emits a `$screen` view event when it changes (see api/product.ts).
 */
let currentScreen: string | null = null;

/** The current screen name, or null if none set. */
export function getScreen(): string | null {
  return currentScreen;
}

/** Set the current screen. Returns true iff the value actually changed. */
export function setScreenState(name: string | null): boolean {
  if (name === currentScreen) return false;
  currentScreen = name;
  return true;
}

/** Drop the in-memory value (tests + teardown). */
export function resetScreen(): void {
  currentScreen = null;
}
```

- [ ] **Step 4: Add wire-type fields**

In `sdks/js/src/types.ts`:
- On `ErrorItem` (after `session_id?: string | null;`): add `screen?: string | null;`
- On `EventItem` (after `session_id?: string | null;`): add `screen?: string | null;`
- On `InitOptions` (after `performance?: boolean;`): add
  ```ts
  /** Seed the initial screen name. */
  screen?: string;
  /**
   * Auto-track the current screen from History navigations (reuses the SPA
   * route hook). Opt-in — default `false`. `setScreen()` works regardless.
   */
  screenTracking?: boolean;
  ```
- On `ResolvedOptions` (after `performance: boolean;`): add `screenTracking: boolean;`

- [ ] **Step 5: Run test — expect pass**

Run: `cd sdks/js && npx vitest run test/screen.test.ts` → PASS. Then `npx tsc --noEmit` → clean.

- [ ] **Step 6: Commit**

```bash
git add sdks/js/src/screen.ts sdks/js/src/types.ts sdks/js/test/screen.test.ts
git commit -m "feat(js-sdk): screen state module + screen wire fields"
```

---

### Task C1.2: Attach `screen` to events/exceptions + `setScreen` API (emits `$screen`)

**Files:**
- Modify: `sdks/js/src/api/product.ts`
- Modify: `sdks/js/src/api/capture.ts`

**Interfaces:**
- Consumes: `getScreen`, `setScreenState` from `../screen.js`.
- Produces: `track(name, properties?, screen?)` attaches `screen`; `setScreen(name: string): void`; `buildErrorItem`/`captureMessage` attach `screen` (from `hint.screen ?? getScreen()`).

- [ ] **Step 1: Write the failing test**

Append to `sdks/js/test/screen.test.ts`:

```ts
import { init, getClient } from '../src/client.js';
import { track, setScreen } from '../src/api/product.js';

function capturedItems() {
  // The transport is exercised via client; assert through a beforeSend spy.
  return items;
}

let items: unknown[] = [];

describe('screen on items', () => {
  beforeEach(() => {
    resetScreen();
    items = [];
    init({ dsn: 'https://pk_test@localhost:9/1', beforeSend: (i) => { items.push(i); return null; } });
  });

  it('stamps the current screen on events', () => {
    setScreen('Home');
    track('clicked');
    // items[0] is the $screen view, items[1] is the clicked event
    const clicked = items.find((i: any) => i.name === 'clicked') as any;
    expect(clicked.screen).toBe('Home');
  });

  it('emits a $screen event only on change', () => {
    setScreen('Home');
    setScreen('Home');
    const views = items.filter((i: any) => i.name === '$screen');
    expect(views).toHaveLength(1);
    expect((views[0] as any).properties.screen).toBe('Home');
  });
});
```

- [ ] **Step 2: Run it — expect failure**

Run: `cd sdks/js && npx vitest run test/screen.test.ts`
Expected: FAIL — `setScreen` not exported from `api/product.js`; `clicked.screen` undefined.

- [ ] **Step 3: Implement in `api/product.ts`**

Add the import and modify `track`, add `setScreen`:

```ts
import { getScreen, setScreenState } from '../screen.js';
```

Replace `track` with:

```ts
export function track(
  name: string,
  properties: Record<string, unknown> = {},
  screen?: string,
): void {
  const client = getClient();
  if (!client) return;
  const item: EventItem = {
    type: 'event',
    name,
    distinct_id: client.getDistinctId(),
    session_id: getSessionId(),
    screen: screen ?? getScreen(),
    timestamp: nowIso(),
    properties: properties ?? {},
  };
  client.captureItem(item);
}

/**
 * Set the current screen. On an actual change, emits a `$screen` view event
 * (carrying the new screen) so dwell can be computed server-side.
 */
export function setScreen(name: string): void {
  if (!setScreenState(name)) return;
  track('$screen', { screen: name });
}
```

- [ ] **Step 4: Implement in `api/capture.ts`**

Add `import { getScreen } from '../screen.js';`. In `buildErrorItem`, add `screen: (hint?.screen as string | undefined) ?? getScreen(),` after `fingerprint,`. In `captureMessage`'s item literal, add `screen: getScreen(),` after `fingerprint: …,`.

- [ ] **Step 5: Run test — expect pass**

Run: `cd sdks/js && npx vitest run test/screen.test.ts` → PASS.

- [ ] **Step 6: Commit**

```bash
git add sdks/js/src/api/product.ts sdks/js/src/api/capture.ts sdks/js/test/screen.test.ts
git commit -m "feat(js-sdk): stamp screen on events/errors + setScreen emits \$screen"
```

---

### Task C1.3: Public exports, init seeding, opt-in History auto-tracking

**Files:**
- Modify: `sdks/js/src/index.ts`
- Modify: `sdks/js/src/client.ts`
- Modify: `sdks/js/src/integrations/history.ts`
- Modify: `sdks/js/src/utils.ts` (SDK_VERSION) + `sdks/js/package.json` (version)

**Interfaces:**
- Consumes: `setScreen`/`getScreen` from `api/product.js` + `screen.js`; `onNavigation` from `integrations/history.js`.
- Produces: exported `setScreen`, `getScreen`; `resolveOptions` sets `screenTracking`; `install()` seeds `options.screen` and, when `screenTracking`, calls `setScreen` on navigation; teardown clears it.

- [ ] **Step 1: Add a navigation hook to `integrations/history.ts`**

Add a module-level listener and fire it inside `emit` after the breadcrumb:

```ts
let navHandler: ((path: string) => void) | null = null;

/** Register (or clear) a callback fired with the new path on each SPA navigation. */
export function onNavigation(cb: ((path: string) => void) | null): void {
  navHandler = cb;
}
```

In `emit`, after the `withInternal(() => addNavigationBreadcrumb(from, to));` line, add:

```ts
    if (to && navHandler) {
      try {
        navHandler(to);
      } catch {
        /* never let screen tracking break navigation */
      }
    }
```

- [ ] **Step 2: Wire options + install in `client.ts`**

In `resolveOptions`, add `screenTracking: options.screenTracking ?? false,` (after `performance: …`). Add imports at the top of `client.ts`:

```ts
import { setScreen } from './api/product.js';
import { setScreenState, resetScreen } from './screen.js';
import { onNavigation } from './integrations/history.js';
```

In `install()`, after `installHistory();`, add:

```ts
    // Screen tracking: seed the initial screen, then follow SPA navigations.
    if (this.options.screen) setScreenState(this.options.screen);
    if (this.options.screenTracking) {
      onNavigation((path) => setScreen(path));
    }
```

In `teardown()`, before `instrument.unpatchAll();`, add:

```ts
    onNavigation(null);
    resetScreen();
```

> Note: `this.options.screen` is on `InitOptions` but `ResolvedOptions` doesn't carry it — add `screen?: string;` to `ResolvedOptions` in `types.ts` and set `screen: options.screen,` in `resolveOptions`, OR read it before resolve. Simplest: add `screen: options.screen,` to the object returned by `resolveOptions` and `screen?: string;` to `ResolvedOptions`.

- [ ] **Step 3: Export from `index.ts`**

Add import: `import { setScreen as setScreenApi } from './api/product.js';` and `import { getScreen as getScreenApi } from './screen.js';`. Add:

```ts
/** Set the current screen (emits a `$screen` view on change). */
export function setScreen(name: string): void {
  setScreenApi(name);
}

/** The current screen name, or null. */
export function getScreen(): string | null {
  return getScreenApi();
}
```

Add `setScreen,` and `getScreen,` to the `Sauron` facade object.

- [ ] **Step 4: Bump version**

In `sdks/js/package.json`, `"version": "0.1.0"` → `"0.2.0"`. In `sdks/js/src/utils.ts`, update `SDK_VERSION` to `'0.2.0'` (grep to confirm the constant).

- [ ] **Step 5: Typecheck + full test + build**

Run: `cd sdks/js && npx tsc --noEmit && npx vitest run && npm run build`
Expected: types clean; all tests pass (including the 3 existing + new screen tests); tsup build succeeds.

- [ ] **Step 6: Commit**

```bash
git add sdks/js/src/index.ts sdks/js/src/client.ts sdks/js/src/integrations/history.ts sdks/js/src/types.ts sdks/js/src/utils.ts sdks/js/package.json
git commit -m "feat(js-sdk): export setScreen/getScreen + opt-in History screen tracking (v0.2.0)"
```

---

# PART C2 — Flutter SDK (`sauron_flutter`)

### Task C2.1: `screen` on envelope items

**Files:**
- Modify: `sdks/flutter/lib/src/envelope.dart`

**Interfaces:**
- Produces: `EventItem` + `ErrorItem` gain `final String? screen;` constructor param + `'screen': screen` in `toJson`.

- [ ] **Step 1: Add the field to both items**

In `envelope.dart`, in the `ErrorItem` class: add `this.screen,` to the constructor, `final String? screen;` beside `final String? sessionId;`, and `'screen': screen,` next to `'session_id': sessionId,` in its `toJson`. Do the same for `EventItem` (its constructor near line 147, field near 156, toJson `'session_id'` near 169).

- [ ] **Step 2: Analyze**

Run: `cd sdks/flutter && dart analyze lib/src/envelope.dart`
Expected: no errors (new optional named param).

- [ ] **Step 3: Commit**

```bash
git add sdks/flutter/lib/src/envelope.dart
git commit -m "feat(flutter-sdk): screen field on Event/Error envelope items"
```

---

### Task C2.2: Client screen state + `setScreen`, attach to events/errors

**Files:**
- Modify: `sdks/flutter/lib/src/client.dart`

**Interfaces:**
- Produces: `String? get screen`; `void setScreen(String name)` (emits `$screen` on change); `captureException(..., {String? screen})` + `track(..., {String? screen})` attach the screen.

- [ ] **Step 1: Write the failing test**

Create `sdks/flutter/test/screen_test.dart` (mirror an existing client test's setup — a client with a capturing transport/beforeSend):

```dart
import 'package:flutter_test/flutter_test.dart';
import 'package:sauron_flutter/sauron_flutter.dart';

void main() {
  test('setScreen stamps screen and emits \$screen once on change', () {
    final captured = <EnvelopeItem>[];
    final client = SauronClient(
      SauronOptions(dsn: 'https://pk_test@localhost:9/1', beforeSend: (i) { captured.add(i); return null; }),
    );
    client.setScreen('Home');
    client.setScreen('Home'); // no-op
    client.track('tapped');

    final views = captured.whereType<EventItem>().where((e) => e.name == r'$screen').toList();
    expect(views, hasLength(1));
    final tapped = captured.whereType<EventItem>().firstWhere((e) => e.name == 'tapped');
    expect(tapped.screen, 'Home');
  });
}
```

> Adjust the `SauronClient`/`SauronOptions` construction to match the exact constructor signature used by the existing Flutter tests (check `sdks/flutter/test/` for the pattern — `beforeSend` returning null to capture-and-drop mirrors the JS approach).

- [ ] **Step 2: Run it — expect failure**

Run: `cd sdks/flutter && flutter test test/screen_test.dart`
Expected: FAIL — `setScreen` undefined; `EventItem.screen` may be fine (added in C2.1) but no value flows.

- [ ] **Step 3: Implement in `client.dart`**

Add a field near `sessionId`: `String? _currentScreen;` and `String? get screen => _currentScreen;`.

Add the method:

```dart
  /// Sets the current screen. On an actual change, emits a `$screen` view event
  /// carrying the new screen (so dwell can be computed server-side).
  void setScreen(String name) {
    if (name == _currentScreen) {
      return;
    }
    _currentScreen = name;
    track(r'$screen', properties: <String, Object?>{'screen': name});
  }
```

Add a `String? screen` optional param to `captureException` and `track`, and attach it:
- `track(String name, {Map<String, Object?>? properties, String? screen})` → set `screen: screen ?? _currentScreen,` on the `EventItem`.
- `captureException(Object error, {StackTrace? stackTrace, Mechanism? mechanism, SauronLevel level = SauronLevel.error, String? screen})` → set `screen: screen ?? _currentScreen,` on the `ErrorItem`.

- [ ] **Step 4: Run test — expect pass**

Run: `cd sdks/flutter && flutter test test/screen_test.dart` → PASS.

- [ ] **Step 5: Commit**

```bash
git add sdks/flutter/lib/src/client.dart sdks/flutter/test/screen_test.dart
git commit -m "feat(flutter-sdk): client screen state + setScreen + attach to events/errors"
```

---

### Task C2.3: Public API, NavigatorObserver auto-tracking, version bump

**Files:**
- Modify: `sdks/flutter/lib/src/sauron.dart`
- Modify: `sdks/flutter/lib/src/sauron_options.dart`
- Modify: `sdks/flutter/lib/src/integrations/widgets_binding_observer.dart`
- Modify: `sdks/flutter/pubspec.yaml` + `lib/src/envelope.dart` (`kSauronSdkVersion`)

**Interfaces:**
- Produces: `Sauron.setScreen(String)`, `Sauron.screen`; `SauronOptions.screen`; `SauronNavigatorObserver` calls `setScreen` on route change.

- [ ] **Step 1: Public facade in `sauron.dart`**

Add (mirroring `track` at line 68):

```dart
  /// Sets the current screen (emits a `$screen` view on change).
  static void setScreen(String name) => _client?.setScreen(name);

  /// The current screen name, or null.
  static String? get screen => _client?.screen;
```

- [ ] **Step 2: Init seeding in `sauron_options.dart` + client**

Add `final String? screen;` to `SauronOptions` (with `this.screen,` in the constructor). In `client.dart`, where options are consumed at construction, seed `_currentScreen = options.screen;`.

- [ ] **Step 3: NavigatorObserver drives setScreen**

In `widgets_binding_observer.dart`'s `SauronNavigatorObserver`, add a `trackScreens` flag (default `true`) to the constructor, and in `_enterRoute` (after setting `_currentRouteName`), add:

```dart
      if (trackScreens && route?.settings.name != null) {
        _client.setScreen(route!.settings.name!);
      }
```

Constructor becomes: `SauronNavigatorObserver(this._client, {this.recordTransactions = true, this.trackScreens = true});` with `final bool trackScreens;`.

- [ ] **Step 4: Version bump**

`pubspec.yaml`: `version: 0.1.0` → `0.2.0`. Update `kSauronSdkVersion` in `envelope.dart` to `'0.2.0'` (grep to confirm the constant name/location).

- [ ] **Step 5: Analyze + full test**

Run: `cd sdks/flutter && dart analyze && flutter test`
Expected: no analyzer errors; all tests (existing 32 + new) pass.

- [ ] **Step 6: Commit**

```bash
git add sdks/flutter/lib/src/sauron.dart sdks/flutter/lib/src/sauron_options.dart sdks/flutter/lib/src/integrations/widgets_binding_observer.dart sdks/flutter/pubspec.yaml sdks/flutter/lib/src/envelope.dart
git commit -m "feat(flutter-sdk): Sauron.setScreen + NavigatorObserver screen tracking (v0.2.0)"
```

---

# PART C3 — Backend data plumbing

### Task C3.1: Migration + diesel schema

**Files:**
- Create: `backend/migrations/2026-07-14-000007_events_screen/up.sql`
- Create: `backend/migrations/2026-07-14-000007_events_screen/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs`

- [ ] **Step 1: up.sql**

```sql
-- 0007: screen attribution. Optional screen/route name stamped by the SDKs on
-- every analytics event and error, mirroring session_id/device_key. Enables the
-- dashboard Screens section (views/events/users/exceptions + on-read dwell).
ALTER TABLE analytics_events ADD COLUMN screen TEXT;
ALTER TABLE error_events     ADD COLUMN screen TEXT;
CREATE INDEX analytics_events_app_screen_idx ON analytics_events (app_id, screen);
CREATE INDEX error_events_app_screen_idx     ON error_events (app_id, screen);
```

- [ ] **Step 2: down.sql**

```sql
DROP INDEX IF EXISTS analytics_events_app_screen_idx;
DROP INDEX IF EXISTS error_events_app_screen_idx;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS screen;
ALTER TABLE error_events     DROP COLUMN IF EXISTS screen;
```

- [ ] **Step 3: Schema**

In `schema.rs`, add `screen -> Nullable<Text>,` to both the `analytics_events` and `error_events` `table!` blocks (place after `device_key -> Nullable<Text>,` / `session_id`).

- [ ] **Step 4: Apply + build**

Run: `docker compose up --build -d` then confirm `2026-07-14-000007_events_screen` applied (`docker compose logs sauron-api | grep -i migrat`); `cd backend && cargo build -p sauron-db` clean.

- [ ] **Step 5: Commit**

```bash
git add backend/migrations/2026-07-14-000007_events_screen backend/crates/sauron-db/src/schema.rs
git commit -m "feat(db): screen column on analytics_events + error_events"
```

---

### Task C3.2: Envelope + DB models + pipeline threading

**Files:**
- Modify: `backend/crates/sauron-core/src/envelope.rs`
- Modify: `backend/crates/sauron-db/src/models.rs`
- Modify: `backend/crates/sauron-pipeline/src/process.rs`

**Interfaces:**
- Produces: `screen: Option<String>` on `AnalyticsItem`, `ErrorItem`, `NewAnalyticsEvent`, `NewErrorEvent`; pipeline sets `screen: ev.screen.clone()` (events) / `screen: e.screen.clone()` (errors).

- [ ] **Step 1: Envelope**

In `envelope.rs`, add to `AnalyticsItem` (after `pub session_id: Option<String>,`, line ~193):

```rust
    #[serde(default)]
    pub screen: Option<String>,
```

Add the same to `ErrorItem` (after its `session_id` at line ~123).

- [ ] **Step 2: DB models**

In `models.rs`, add `pub screen: Option<String>,` to `NewAnalyticsEvent` (after `device_key`) and `NewErrorEvent` (after `device_key`).

- [ ] **Step 3: Pipeline**

In `process.rs` `process_event`, in the `NewAnalyticsEvent { … }` literal add `screen: ev.screen.clone(),` (near `device_key: info.device_key.clone(),`). In `process_error`, in the `NewErrorEvent { … }` literal add `screen: e.screen.clone(),` (match the error item binding name used there — likely `e` or `err`; grep the function to confirm).

- [ ] **Step 4: Build**

Run: `cd backend && cargo build`
Expected: clean (all `Serialize`/`Insertable` derives pick up the new fields).

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-core/src/envelope.rs backend/crates/sauron-db/src/models.rs backend/crates/sauron-pipeline/src/process.rs
git commit -m "feat(ingest): thread screen through envelope + pipeline into rows"
```

---

### Task C3.3: Surface `screen` on read models + hide `$screen` from Events explorer

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (analytics-event + error-event read structs; `list_analytics_events`)
- Modify: `dashboard/src/lib/models/index.ts` (`AnalyticsEvent`, `ErrorEvent`)

**Interfaces:**
- Produces: `screen: Option<String>` on the `AnalyticsEvent` + `ErrorEvent` read structs (so event/issue detail can show + link it); `list_analytics_events` excludes `name = '$screen'`.

- [ ] **Step 1: Add `screen` to the read structs**

In `repo.rs`, find the `AnalyticsEvent` and `ErrorEvent` `#[derive(Queryable, …)]` structs (the ones returned by `list_analytics_events` / the error-event list). Add `pub screen: Option<String>,` positioned to match the `screen -> Nullable<Text>` column order in `schema.rs` (Queryable maps by column order — place it in the same relative position, i.e. last, matching the `ALTER TABLE … ADD COLUMN` which appends at the end). Verify field/column order alignment after editing.

- [ ] **Step 2: Exclude `$screen` from the Events explorer**

In `list_analytics_events`, after the base `query` is built (after `.filter(analytics_events::app_id.eq(app_id))`), add:

```rust
    // Synthetic screen-view events belong to the Screens section, not the stream.
    let mut query = query.filter(analytics_events::name.ne("$screen"));
```

(Adjust binding: fold into the existing `let mut query = …into_boxed();` chain, or reassign as shown.)

- [ ] **Step 3: Frontend models**

In `dashboard/src/lib/models/index.ts`, add `screen?: string | null;` to the `AnalyticsEvent` interface and the `ErrorEvent` interface.

- [ ] **Step 4: Build + typecheck**

Run: `cd backend && cargo build` (clean) then `cd ../dashboard && npx svelte-check --tsconfig ./tsconfig.json` (0 errors).

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-db/src/repo.rs dashboard/src/lib/models/index.ts
git commit -m "feat: surface screen on event/error read models; hide \$screen from Events list"
```

---

# PART C4 — Backend screens endpoints

### Task C4.1: `screen_list` + `screen_stats` repo queries (+ pure `avg_dwell`)

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs`

**Interfaces:**
- Produces: `ScreenRow { screen, views, events, exceptions, users: i64, avg_dwell_ms: f64 }`; `ScreenStats { screen, views, events, exceptions, users: i64, total_dwell_ms, avg_dwell_ms: f64 }`; `screen_list(conn, app_id, since, q_pattern, limit, offset)`; `screen_stats(conn, app_id, since, name)`; pure `avg_dwell(total_ms: f64, views: i64) -> f64`.

- [ ] **Step 1: Write the failing test for `avg_dwell`**

Append to `repo.rs`:

```rust
#[cfg(test)]
mod avg_dwell_tests {
    use super::avg_dwell;

    #[test]
    fn divides_total_by_views() {
        assert!((avg_dwell(9000.0, 3) - 3000.0).abs() < 1e-9);
    }

    #[test]
    fn zero_views_is_zero() {
        assert_eq!(avg_dwell(9000.0, 0), 0.0);
    }
}
```

- [ ] **Step 2: Run — expect failure**

Run: `cd backend && cargo test -p sauron-db avg_dwell_tests`
Expected: FAIL — `avg_dwell` not found.

- [ ] **Step 3: Implement structs, helper, and the two queries**

```rust
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct ScreenRow {
    #[diesel(sql_type = Text)]
    pub screen: String,
    #[diesel(sql_type = BigInt)]
    pub views: i64,
    #[diesel(sql_type = BigInt)]
    pub events: i64,
    #[diesel(sql_type = BigInt)]
    pub exceptions: i64,
    #[diesel(sql_type = BigInt)]
    pub users: i64,
    #[diesel(sql_type = Double)]
    pub avg_dwell_ms: f64,
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct ScreenStats {
    #[diesel(sql_type = Text)]
    pub screen: String,
    #[diesel(sql_type = BigInt)]
    pub views: i64,
    #[diesel(sql_type = BigInt)]
    pub events: i64,
    #[diesel(sql_type = BigInt)]
    pub exceptions: i64,
    #[diesel(sql_type = BigInt)]
    pub users: i64,
    #[diesel(sql_type = Double)]
    pub total_dwell_ms: f64,
    #[diesel(sql_type = Double)]
    pub avg_dwell_ms: f64,
}

/// total dwell / views, guarding views=0. Pure.
pub fn avg_dwell(total_ms: f64, views: i64) -> f64 {
    if views > 0 {
        total_ms / views as f64
    } else {
        0.0
    }
}

// Shared CTE fragment: per-screen views/events/users/exceptions/dwell. $1 app, $2 since.
const SCREEN_CTES: &str = "\
  WITH ev AS ( \
    SELECT screen, \
      count(*) FILTER (WHERE name='$screen')::bigint AS views, \
      count(*) FILTER (WHERE name<>'$screen')::bigint AS events \
    FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL GROUP BY screen), \
  ex AS ( \
    SELECT screen, count(*)::bigint AS exceptions \
    FROM error_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL GROUP BY screen), \
  us AS ( \
    SELECT screen, count(DISTINCT distinct_id)::bigint AS users FROM ( \
      SELECT screen, distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND distinct_id IS NOT NULL AND distinct_id<>'' \
      UNION ALL \
      SELECT screen, distinct_id FROM error_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND distinct_id IS NOT NULL AND distinct_id<>'' \
    ) u GROUP BY screen), \
  dw AS ( \
    SELECT screen, sum(gap_ms)::double precision AS total_dwell_ms FROM ( \
      SELECT screen, LEAST(EXTRACT(EPOCH FROM ( \
        LEAD(occurred_at) OVER (PARTITION BY session_id ORDER BY occurred_at) - occurred_at)) * 1000, 1800000) AS gap_ms \
      FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND session_id IS NOT NULL AND screen IS NOT NULL) g \
    WHERE gap_ms > 0 GROUP BY screen), \
  keys AS (SELECT screen FROM ev UNION SELECT screen FROM ex) ";

pub async fn screen_list(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
    q_pattern: &str, // '%' for no filter, else like_contains(term)
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<ScreenRow>> {
    diesel::sql_query(format!(
        "{SCREEN_CTES} \
         SELECT k.screen, \
           COALESCE(ev.views,0)::bigint AS views, \
           COALESCE(ev.events,0)::bigint AS events, \
           COALESCE(ex.exceptions,0)::bigint AS exceptions, \
           COALESCE(us.users,0)::bigint AS users, \
           (COALESCE(dw.total_dwell_ms,0) / NULLIF(COALESCE(ev.views,0),0))::double precision AS avg_dwell_ms \
         FROM keys k \
         LEFT JOIN ev ON ev.screen=k.screen LEFT JOIN ex ON ex.screen=k.screen \
         LEFT JOIN us ON us.screen=k.screen LEFT JOIN dw ON dw.screen=k.screen \
         WHERE k.screen ILIKE $3 \
         ORDER BY views DESC, k.screen ASC LIMIT $4 OFFSET $5"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .bind::<Text, _>(q_pattern)
    .bind::<BigInt, _>(limit)
    .bind::<BigInt, _>(offset)
    .get_results(conn)
    .await
}

pub async fn screen_stats(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
    name: &str,
) -> QueryResult<ScreenStats> {
    diesel::sql_query(format!(
        "{SCREEN_CTES} \
         SELECT k.screen, \
           COALESCE(ev.views,0)::bigint AS views, \
           COALESCE(ev.events,0)::bigint AS events, \
           COALESCE(ex.exceptions,0)::bigint AS exceptions, \
           COALESCE(us.users,0)::bigint AS users, \
           COALESCE(dw.total_dwell_ms,0)::double precision AS total_dwell_ms, \
           (COALESCE(dw.total_dwell_ms,0) / NULLIF(COALESCE(ev.views,0),0))::double precision AS avg_dwell_ms \
         FROM keys k \
         LEFT JOIN ev ON ev.screen=k.screen LEFT JOIN ex ON ex.screen=k.screen \
         LEFT JOIN us ON us.screen=k.screen LEFT JOIN dw ON dw.screen=k.screen \
         WHERE k.screen = $3"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .bind::<Text, _>(name)
    .get_result(conn)
    .await
}
```

> `avg_dwell_ms` computed in SQL returns NULL when views=0 (NULLIF). Map the column as `Double` — but a NULL would fail to deserialize into `f64`. Wrap with `COALESCE(… , 0)`: change the avg expression to `COALESCE(COALESCE(dw.total_dwell_ms,0) / NULLIF(COALESCE(ev.views,0),0), 0)::double precision`. Apply in both queries. (The pure `avg_dwell` helper is still used by the detail handler as the source of truth for the tile, keeping the tested path authoritative.)

- [ ] **Step 4: Run tests + build**

Run: `cd backend && cargo test -p sauron-db avg_dwell_tests` → PASS; `cargo build -p sauron-db` → clean.

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-db/src/repo.rs
git commit -m "feat(db): screen_list + screen_stats on-read queries with capped dwell"
```

---

### Task C4.2: `recent_events_for_screen` + `recent_exceptions_for_screen`

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs`

**Interfaces:**
- Produces: `recent_events_for_screen(conn, app_id, screen, since, limit) -> QueryResult<Vec<AnalyticsEvent>>`; `recent_exceptions_for_screen(conn, app_id, screen, since, limit) -> QueryResult<Vec<ErrorEvent>>` (reusing the existing read structs).

- [ ] **Step 1: Implement using the diesel query builder (mirror `list_analytics_events`)**

```rust
pub async fn recent_events_for_screen(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    screen: &str,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    analytics_events::table
        .filter(analytics_events::app_id.eq(app_id))
        .filter(analytics_events::screen.eq(screen))
        .filter(analytics_events::occurred_at.ge(since))
        .filter(analytics_events::name.ne("$screen"))
        .order(analytics_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

pub async fn recent_exceptions_for_screen(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    screen: &str,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    error_events::table
        .filter(error_events::app_id.eq(app_id))
        .filter(error_events::screen.eq(screen))
        .filter(error_events::occurred_at.ge(since))
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}
```

> If `AnalyticsEvent`/`ErrorEvent` load via `.select(X::as_select())` elsewhere, use the same `.select(...)` here so column/field order matches. Confirm against the existing list functions.

- [ ] **Step 2: Build + commit**

Run: `cd backend && cargo build -p sauron-db` → clean.

```bash
git add backend/crates/sauron-db/src/repo.rs
git commit -m "feat(db): recent events/exceptions for a screen"
```

---

### Task C4.3: `routes/screens.rs` — list + detail endpoints

**Files:**
- Create: `backend/bins/sauron-api/src/routes/screens.rs`
- Modify: `backend/bins/sauron-api/src/routes/mod.rs` (register module)
- Modify: `backend/bins/sauron-api/src/main.rs` (routes)

**Interfaces:**
- Produces: `GET /v1/apps/{app_id}/screens` and `GET /v1/apps/{app_id}/screens/detail`.

- [ ] **Step 1: Create the handler module**

```rust
//! Screen analytics: per-screen views/events/users/exceptions + on-read dwell.
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::repo;

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct ScreenListQuery {
    #[serde(default = "days30")]
    pub since_days: i64,
    pub q: Option<String>,
    #[serde(default = "lim50")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}
fn days30() -> i64 { 30 }
fn lim50() -> i64 { 50 }

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenListQuery>,
) -> Result<Json<Vec<repo::ScreenRow>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let pattern = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(term) => repo::like_contains(term),
        None => "%".to_string(),
    };
    let rows = repo::screen_list(
        &mut conn, app_id, since, &pattern,
        q.limit.clamp(1, 200), q.offset.max(0),
    ).await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct ScreenDetailQuery {
    pub name: String,
    #[serde(default = "days30")]
    pub since_days: i64,
}

#[derive(Serialize)]
pub struct ScreenDetail {
    pub stats: repo::ScreenStats,
    pub recent_events: Vec<repo::AnalyticsEvent>,
    pub recent_exceptions: Vec<repo::ErrorEvent>,
}

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenDetailQuery>,
) -> Result<Json<ScreenDetail>, ApiError> {
    if q.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let stats = repo::screen_stats(&mut conn, app_id, since, &q.name).await?;
    let recent_events = repo::recent_events_for_screen(&mut conn, app_id, &q.name, since, 20).await?;
    let recent_exceptions = repo::recent_exceptions_for_screen(&mut conn, app_id, &q.name, since, 20).await?;
    Ok(Json(ScreenDetail { stats, recent_events, recent_exceptions }))
}
```

> `repo::like_contains` is currently private (used inside `repo.rs`). Make it `pub` (it's already unit-tested). `repo::AnalyticsEvent`/`repo::ErrorEvent` must be public read structs — confirm they're exported.

- [ ] **Step 2: Register the module + routes**

In `routes/mod.rs`, add `pub mod screens;`. In `main.rs`, add:

```rust
.route("/v1/apps/{app_id}/screens", get(routes::screens::list))
.route("/v1/apps/{app_id}/screens/detail", get(routes::screens::detail))
```

- [ ] **Step 3: Build**

Run: `cd backend && cargo build`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add backend/bins/sauron-api/src/routes/screens.rs backend/bins/sauron-api/src/routes/mod.rs backend/bins/sauron-api/src/main.rs backend/crates/sauron-db/src/repo.rs
git commit -m "feat(api): GET /screens list + /screens/detail endpoints"
```

---

# PART C5 — Dashboard

### Task C5.1: Models + API client + nav + routes

**Files:**
- Modify: `dashboard/src/lib/models/index.ts`
- Create: `dashboard/src/lib/api/screens.ts`
- Modify: `dashboard/src/lib/components/layout/Sidebar.svelte`
- Modify: `dashboard/src/routes.ts`

- [ ] **Step 1: Models**

Append to `models/index.ts`:

```ts
// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

export interface ScreenRow {
  screen: string;
  views: number;
  events: number;
  exceptions: number;
  users: number;
  avg_dwell_ms: number;
}

export interface ScreenStats extends ScreenRow {
  total_dwell_ms: number;
}

export interface ScreenDetail {
  stats: ScreenStats;
  recent_events: AnalyticsEvent[];
  recent_exceptions: ErrorEvent[];
}
```

- [ ] **Step 2: API client `api/screens.ts`**

```ts
import { api } from './client';
import type { ScreenRow, ScreenDetail } from '../models';

export interface ListScreensParams {
  q?: string;
  sinceDays?: number;
  limit?: number;
  offset?: number;
}

export async function listScreens(appId: string, opts: ListScreensParams = {}): Promise<ScreenRow[]> {
  const p = new URLSearchParams();
  if (opts.q) p.set('q', opts.q);
  if (opts.sinceDays != null) p.set('since_days', String(opts.sinceDays));
  if (opts.limit != null) p.set('limit', String(opts.limit));
  if (opts.offset != null) p.set('offset', String(opts.offset));
  const { data } = await api.get<ScreenRow[]>(`/v1/apps/${appId}/screens?${p.toString()}`);
  return data;
}

export async function getScreenDetail(appId: string, name: string, sinceDays = 30): Promise<ScreenDetail> {
  const { data } = await api.get<ScreenDetail>(`/v1/apps/${appId}/screens/detail`, {
    params: { name, since_days: sinceDays },
  });
  return data;
}
```

- [ ] **Step 3: Nav item**

In `Sidebar.svelte`, in the **Explore** group's `items` array (after the `devices` entry, line ~35), add:

```ts
        { href: '#/screens', label: 'Screens', icon: 'layout-panel-top', match: (p) => p.startsWith('/screens') },
```

(Confirm `layout-panel-top` exists in the Icon set; else use `panels-top-left` or `app-window`.)

- [ ] **Step 4: Routes**

In `routes.ts`, add imports `import ScreensList from './pages/ScreensList.svelte';` and `import ScreenDetail from './pages/ScreenDetail.svelte';`, and in the Explore section of `routes`:

```ts
  '/screens': guarded(ScreensList as Component<never>),
  '/screens/:name': guarded(ScreenDetail as Component<never>),
```

- [ ] **Step 5: Typecheck**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json`
Expected: FAILS only on the two missing page imports (created next task). If you prefer green here, create empty placeholder pages first; otherwise proceed to C5.2 and typecheck at its end.

- [ ] **Step 6: Commit**

```bash
git add dashboard/src/lib/models/index.ts dashboard/src/lib/api/screens.ts dashboard/src/lib/components/layout/Sidebar.svelte dashboard/src/routes.ts
git commit -m "feat(dashboard): screens models, API client, nav + routes"
```

---

### Task C5.2: `ScreensList` page

**Files:**
- Create: `dashboard/src/pages/ScreensList.svelte`

- [ ] **Step 1: Create the page (mirrors `DevicesInventory.svelte`)**

```svelte
<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import DataTable from '../lib/components/DataTable.svelte';
  import SearchInput from '../lib/components/SearchInput.svelte';
  import Pagination from '../lib/components/Pagination.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { listScreens } from '../lib/api/screens';
  import { errorMessage } from '../lib/api/client';
  import { compactNumber, formatDuration } from '../lib/utils/format';
  import type { ScreenRow } from '../lib/models';

  const LIMIT = 50;
  let searchTerm = $state('');
  let query = $state('');
  let sinceDays = $state(30);
  let offset = $state(0);
  let rows = $state<ScreenRow[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let debounce: ReturnType<typeof setTimeout> | undefined;

  function onSearch(v: string) {
    clearTimeout(debounce);
    debounce = setTimeout(() => { query = v.trim(); offset = 0; }, 250);
  }

  async function load(appId: string, q: string, days: number, off: number) {
    loading = true;
    error = null;
    try {
      rows = await listScreens(appId, { q: q || undefined, sinceDays: days, limit: LIMIT, offset: off });
    } catch (err) {
      error = errorMessage(err);
      rows = [];
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const q = query;
    const days = sinceDays;
    const off = offset;
    if (aid) void load(aid, q, days, off);
  });
</script>

<AppShell requireApp>
  <div class="head">
    <div>
      <h1 class="page-title">Screens</h1>
      <p class="muted sub">Views, engagement and errors per screen.</p>
    </div>
    <DateRange value={sinceDays} onchange={(d) => { sinceDays = d; offset = 0; }} />
  </div>

  <div class="toolbar">
    <SearchInput bind:value={searchTerm} placeholder="Search screens…" oninput={(e) => onSearch((e.target as HTMLInputElement).value)} />
  </div>

  {#if error && rows.length === 0}
    <Card><EmptyState title="Couldn't load screens" description={error} icon="triangle-alert" /></Card>
  {:else if loading && rows.length === 0}
    <Card><div class="center"><Spinner size={22} /></div></Card>
  {:else if rows.length === 0}
    <Card><EmptyState title="No screens yet" description={query ? `No screens match “${query}”.` : 'Call setScreen() in your SDK to attribute events to screens.'} icon="layout-panel-top" /></Card>
  {:else}
    <DataTable>
      <thead>
        <tr>
          <th>Screen</th>
          <th class="num">Views</th>
          <th class="num">Events</th>
          <th class="num">Exceptions</th>
          <th class="num">Users</th>
          <th class="num">Avg dwell</th>
        </tr>
      </thead>
      <tbody>
        {#each rows as r (r.screen)}
          <tr class="clickable" onclick={() => push('/screens/' + encodeURIComponent(r.screen))}>
            <td><span class="cell-mono truncate">{r.screen}</span></td>
            <td class="num">{compactNumber(r.views)}</td>
            <td class="num">{compactNumber(r.events)}</td>
            <td class="num" class:danger={r.exceptions > 0}>{compactNumber(r.exceptions)}</td>
            <td class="num">{compactNumber(r.users)}</td>
            <td class="num">{formatDuration(r.avg_dwell_ms)}</td>
          </tr>
        {/each}
      </tbody>
    </DataTable>
    <Pagination {offset} limit={LIMIT} count={rows.length} onchange={(o) => (offset = o)} />
  {/if}
</AppShell>

<style>
  .head { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin-bottom: 16px; flex-wrap: wrap; }
  .sub { font-size: 13.5px; margin-top: 3px; }
  .toolbar { margin-bottom: 14px; }
  .center { display: grid; place-items: center; min-height: 200px; }
  .num { text-align: right; }
  .danger { color: var(--error); }
  .clickable { cursor: pointer; }
  .clickable:hover { background: var(--surface-2); }
</style>
```

> Confirm `SearchInput`'s prop API against `DevicesInventory.svelte` (it uses `bind:value` + an input handler — match its exact props; the `oninput` wiring above may need to be `oninput={onSearch}` depending on the component). Confirm `DataTable` expects `<thead>/<tbody>` (as used here) vs bare rows — match `DevicesInventory.svelte`'s usage exactly.

- [ ] **Step 2: Typecheck**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json`
Expected: 0 errors (ScreenDetail import in routes still pending → create in next task, or it errors until C5.3; acceptable to batch).

- [ ] **Step 3: Commit**

```bash
git add dashboard/src/pages/ScreensList.svelte
git commit -m "feat(dashboard): Screens list page"
```

---

### Task C5.3: `ScreenDetail` page

**Files:**
- Create: `dashboard/src/pages/ScreenDetail.svelte`

- [ ] **Step 1: Create the page (mirrors `DeviceDetail.svelte`)**

```svelte
<script lang="ts">
  import { push } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { getScreenDetail } from '../lib/api/screens';
  import { errorMessage } from '../lib/api/client';
  import { compactNumber, formatDuration, formatDateTime, relativeTime } from '../lib/utils/format';
  import type { ScreenDetail } from '../lib/models';

  interface Props { params?: { name?: string }; }
  let { params }: Props = $props();
  const screenName = $derived(decodeURIComponent(params?.name ?? ''));

  let detail = $state<ScreenDetail | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load(appId: string, name: string) {
    loading = true;
    error = null;
    try {
      detail = await getScreenDetail(appId, name);
    } catch (err) {
      error = errorMessage(err);
      detail = null;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const name = screenName;
    if (aid && name) void load(aid, name);
  });
</script>

<AppShell requireApp>
  <button class="back" onclick={() => push('/screens')}><Icon name="arrow-left" size={14} /> Screens</button>

  {#if loading && !detail}
    <Card><div class="center"><Spinner size={22} /></div></Card>
  {:else if error}
    <Card><EmptyState title="Couldn't load screen" description={error} icon="triangle-alert">
      {#snippet action()}<Button variant="secondary" onclick={() => push('/screens')}>Back to screens</Button>{/snippet}
    </EmptyState></Card>
  {:else if detail}
    <h1 class="page-title mono">{screenName}</h1>

    <StatTiles min={150}>
      <StatTile label="Views" value={compactNumber(detail.stats.views)} tone="primary" />
      <StatTile label="Users" value={compactNumber(detail.stats.users)} />
      <StatTile label="Events" value={compactNumber(detail.stats.events)} />
      <StatTile label="Exceptions" value={compactNumber(detail.stats.exceptions)} tone={detail.stats.exceptions > 0 ? 'error' : 'neutral'} />
      <StatTile label="Avg dwell" value={formatDuration(detail.stats.avg_dwell_ms)} />
      <StatTile label="Total dwell" value={formatDuration(detail.stats.total_dwell_ms)} />
    </StatTiles>

    <div class="lists">
      <Card title="Recent events">
        {#if detail.recent_events.length === 0}
          <p class="muted">No events on this screen.</p>
        {:else}
          <ul class="rows">
            {#each detail.recent_events as e (e.id)}
              <li><span class="mono truncate">{e.name}</span><span class="faint" title={formatDateTime(e.occurred_at)}>{relativeTime(e.occurred_at)}</span></li>
            {/each}
          </ul>
        {/if}
      </Card>
      <Card title="Recent exceptions">
        {#if detail.recent_exceptions.length === 0}
          <p class="muted">No exceptions on this screen.</p>
        {:else}
          <ul class="rows">
            {#each detail.recent_exceptions as x (x.id)}
              <li>
                <button class="link mono truncate" onclick={() => push('/issues/' + x.issue_id)}>{x.exception_type ?? x.message}</button>
                <span class="faint" title={formatDateTime(x.occurred_at)}>{relativeTime(x.occurred_at)}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </Card>
    </div>
  {:else}
    <Card><EmptyState title="Screen not found" description="No data for this screen in the selected range." icon="layout-panel-top" /></Card>
  {/if}
</AppShell>

<style>
  .back { display: inline-flex; align-items: center; gap: 6px; background: none; border: none; color: var(--text-muted); font-size: 13px; padding: 4px 0; margin-bottom: 10px; cursor: pointer; }
  .center { display: grid; place-items: center; min-height: 200px; }
  .lists { display: grid; grid-template-columns: 1fr 1fr; gap: 18px; margin-top: 16px; align-items: start; }
  @media (max-width: 900px) { .lists { grid-template-columns: 1fr; } }
  .rows { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 8px; }
  .rows li { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .link { background: none; border: none; color: var(--primary); cursor: pointer; padding: 0; text-align: left; }
  .faint { font-size: 12px; color: var(--text-faint); white-space: nowrap; }
</style>
```

> Confirm the `ErrorEvent` fields (`id`, `issue_id`, `exception_type`, `message`, `occurred_at`) and `AnalyticsEvent` fields (`id`, `name`, `occurred_at`) against `models/index.ts`; adjust property names to the real ones. Confirm `Card`, `StatTile`, `EmptyState` action-snippet APIs against `DeviceDetail.svelte`.

- [ ] **Step 2: Typecheck + build**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json && npx vite build`
Expected: 0 errors; build succeeds.

- [ ] **Step 3: Commit**

```bash
git add dashboard/src/pages/ScreenDetail.svelte
git commit -m "feat(dashboard): Screen detail page"
```

---

### Task C5.4: Screen click-through on event + issue detail

**Files:**
- Modify: `dashboard/src/pages/IssueDetail.svelte`
- Modify: the Events explorer event view (`dashboard/src/pages/Events.svelte` or its event drawer/row component)
- Modify: `dashboard/src/pages/SessionDetail.svelte` (optional — where events/errors are listed with context)

- [ ] **Step 1: Add a reusable screen link where a signal's screen is shown**

Wherever an error's or event's `screen` is available (Issue detail's latest error event; the Events explorer's event detail), render, when `screen` is non-null:

```svelte
{#if item.screen}
  <a class="meta-link mono" href={`#/screens/${encodeURIComponent(item.screen)}`}>
    <Icon name="layout-panel-top" size={14} /> {item.screen}
  </a>
{/if}
```

Mirror the exact markup `SessionDetail.svelte` uses for its `device_key` link (lines ~104–106) so styling (`.meta-link`) is consistent. Place it in the same metadata row where session/device/user are shown.

- [ ] **Step 2: Typecheck + build**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json && npx vite build`
Expected: 0 errors; build succeeds.

- [ ] **Step 3: Commit**

```bash
git add dashboard/src/pages/IssueDetail.svelte dashboard/src/pages/Events.svelte dashboard/src/pages/SessionDetail.svelte
git commit -m "feat(dashboard): clickable screen link on event/exception detail"
```

---

# PART C6 — End-to-end verification

### Task C6.1: Full-stack e2e

**Files:** none (verification only).

- [ ] **Step 1: Build + bring up the stack**

Run: `docker compose up --build -d`. Confirm migration `2026-07-14-000007_events_screen` applied and the API is healthy.

- [ ] **Step 2: Emit screen-tagged signals**

Use the `examples/svelte-web` demo (or a curl envelope) with the updated SDK: call `Sauron.setScreen('Home')`, fire a couple of events + an exception, then `setScreen('Checkout')` and fire more. Confirm envelopes carry `screen` (check `docker compose logs sauron-ingest` or the DB: `SELECT name, screen FROM analytics_events ORDER BY occurred_at DESC LIMIT 10;`).

- [ ] **Step 3: Curl the endpoints**

```bash
curl -s "localhost:10000/v1/apps/$APP/screens?since_days=30" -H "authorization: Bearer $TOKEN" | jq
curl -s "localhost:10000/v1/apps/$APP/screens/detail?name=Home&since_days=30" -H "authorization: Bearer $TOKEN" | jq
```

Expected: list has one row per screen with `views ≥ 1`, correct `events`/`exceptions`/`users`, and a plausible `avg_dwell_ms` (≤ 30 min cap); detail returns `stats` + `recent_events` (no `$screen` rows) + `recent_exceptions`. A screen with a single event has `avg_dwell_ms` = 0 or small.

- [ ] **Step 4: UI**

Via preview: open `#/screens` — the Screens nav item appears under Explore; the table lists screens with dwell; a row → `#/screens/Home` detail with tiles + recent lists. Open an exception on that screen (Issue detail) and confirm the **screen link** navigates back to the screen detail. Confirm the Events explorer does **not** show `$screen` rows. Screenshot for the maintainer.

- [ ] **Step 5: Record result** with the actual JSON + screenshots; fix any failing query/handler/UI and re-run.

---

## Self-Review (completed by plan author)

**Spec coverage:** SDK optional screen on event+exception (C1.2, C2.2) ✓; `setScreen`/init option/per-call override (C1.2–C1.3, C2.2–C2.3) ✓; auto `$screen` on change (C1.2, C2.2) ✓; opt-in auto-detect web+Flutter (C1.3, C2.3) ✓; migration + envelope + pipeline (C3.1–C3.2) ✓; views/events/exceptions/users/dwell on-read with 30-min cap (C4.1) ✓; list + detail endpoints (C4.3) ✓; new Screens section list+detail (C5.2–C5.3) ✓; clickable screen on event/exception (C5.4) ✓; `$screen` hidden from Events explorer (C3.3) ✓; SDK version bumps (C1.3, C2.3) ✓; compose e2e (C6.1) ✓.

**Placeholder scan:** no TBD/TODO. The several `> Confirm …` notes are verification instructions targeting components I did not read line-for-line (DataTable/SearchInput markup, exact ErrorEvent field names, Icon names, Flutter test constructor, error-item binding name in `process_error`); each names the concrete file to mirror and a fallback. All code steps carry full code.

**Type consistency:** `screen` is `Option<String>`/`string | null`/`String?` uniformly across envelope↔models↔pipeline↔read-structs↔TS. `ScreenRow`/`ScreenStats`/`ScreenDetail` field names match between `repo.rs`, `routes/screens.rs`, and `models/index.ts` (`avg_dwell_ms`, `total_dwell_ms`, `views`, `events`, `exceptions`, `users`). `avg_dwell`/`setScreenState`/`setScreen` names match their tests and callers. Reserved name `$screen` identical in SDK emit, `list_analytics_events` exclusion, and `SCREEN_CTES`.
