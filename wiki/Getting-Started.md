# Getting Started

This walks you from nothing to your first signal landing in the dashboard.

See also: **[Home](Home.md)** · **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Capabilities](Capabilities.md)** · **[Examples](Examples.md)**

## 1. Create an app and get a DSN

In the dashboard, an **App** belongs to a **Project**, which belongs to an
**Organization** (see **[Home](Home.md)** for the tenancy model). Each app holds one
or more **Environments**, and it's the environment that owns the DSN — the ingest
unit.

1. Open the dashboard and go to **Projects** (the Manage section of the sidebar).
2. Create (or open) a project, then create an **App**. Pick an **app type** — see
   step 2. The app is created with one environment, `dev`.
3. Open the app's **Settings → Environments** to read the `dev` environment's **DSN**.

The DSN looks like:

```
https://<public_key>@<host>/<environment_id>
```

- `<public_key>` is a **non-secret write key** — safe to ship in client code. There is
  no password component.
- `<host>` is the ingest host (may include a port, e.g. `localhost:8081`).
- `<environment_id>` is the environment's id path segment (an id/UUID).
  Informational only: the gateway authenticates on the key alone and does not
  read this segment.

### Environments

Every app is created with one environment, `dev`, and each environment has its
own DSN. Add more (`staging`, `production`, …) under **Settings → app →
Environments**, then point each deployment at the matching DSN. The environment a
signal belongs to is determined by the key it arrived with, so it cannot be
spoofed by a client and typos cannot create phantom environments.

## 2. Pick an app type

The app type tells the dashboard which SDK and integration guide to show. Supported
types:

| App type | SDK | Wiki page |
| --- | --- | --- |
| `web` | `@edraj/sauron-browser` | [Browser SDK](Browser-SDK.md) |
| `flutter` | `sauron_flutter` | [Flutter SDK](Flutter-SDK.md) |
| `node` | `@edraj/sauron-node` | [Node SDK](Node-SDK.md) |
| `python` | `sauron-sdk` | [Python SDK](Python-SDK.md) |
| `csharp` | `Sauron` (`sauron-dotnet`) | [C# SDK](CSharp-SDK.md) |
| `ios`, `android`, `react_native` | — | (native mobile app types) |

## 3. Send your first event

Each SDK exposes the same core surface: **init**, **track**, **captureException**,
**identify**, **flush/close** — plus **scope** (user/tags/context), **breadcrumbs**,
**transactions** (`trackTransaction`), and a **`beforeSend`** hook, all reconciled across
the five SDKs in **v0.3.0**. See **[Capabilities](Capabilities.md)** for the full parity
matrix. Below is the shortest path per SDK; all read the DSN however is idiomatic for that
platform (client SDKs take it in `init`, the example servers read it from a `SAURON_DSN`
env var).

### Browser (`@edraj/sauron-browser`)

```ts
import { Sauron } from '@edraj/sauron-browser';

Sauron.init({ dsn: 'https://<public_key>@<host>/<environment_id>', release: 'web@1.0.0' });
Sauron.identify('u_123', { plan: 'pro' });
Sauron.track('checkout_completed', { cart_value: 42.5 });
```

### Flutter (`sauron_flutter`)

```dart
import 'package:sauron_flutter/sauron_flutter.dart';

await Sauron.init(
  SauronOptions(
    dsn: 'https://<public_key>@<host>/<environment_id>',
    release: 'app@1.0.0+1',
  ),
  appRunner: () => runApp(const MyApp()),
);

Sauron.track('checkout_completed', properties: {'cart_value': 42.5});
```

### Node (`@edraj/sauron-node`)

```ts
import { init, track } from '@edraj/sauron-node';

init({ dsn: process.env.SAURON_DSN! });
track('order_completed', 'user-123', { total: 42.5, currency: 'USD' });
```

### Python (`sauron-sdk`)

```python
import sauron

sauron.init(dsn="https://<public_key>@<host>/<environment_id>")
sauron.track("checkout_completed", distinct_id="u_123", properties={"cart_value": 42.5})
```

### C# (`Sauron`)

```csharp
using Sauron;

SauronSdk.Init("https://<public_key>@<host>/<environment_id>");
SauronSdk.Track("order_completed", "user-42", new Dictionary<string, object?> { ["total"] = 42.5 });
```

## 4. Flush before exit (server SDKs)

Client SDKs (browser, Flutter) flush in the background. Short-lived server processes
should flush and close so the buffer drains before the process exits:

- Node: `await close();` — or pass `autoShutdown: true` to `init` to wire
  `beforeExit`/`SIGTERM`/`SIGINT` to `close()` automatically.
- Python: `sauron.flush(); sauron.close()` — `init` also registers an `atexit` flush, so a
  short-lived script drains on exit even without an explicit `close()`.
- C#: `SauronSdk.Flush(); SauronSdk.Close();`

For long-running services, prefer per-request scope isolation over global state — see
**[Best Practices](Best-Practices.md)** and **[Framework Integrations](Framework-Integrations.md)**.

## 5. Watch it land

Open the **[Dashboard](Dashboard.md)** — events appear under **Events**, grouped
errors under **Exceptions**, and identified people under **Users**.

## Next steps

- Full wire details: **[Ingest Wire Contract](Ingest-Wire-Contract.md)**.
- SDK feature-parity matrix: **[Capabilities](Capabilities.md)**.
- Copy-paste framework recipes: **[Framework Integrations](Framework-Integrations.md)**.
- Naming, PII scrubbing, sampling, flush/shutdown: **[Best Practices](Best-Practices.md)**.
- When nothing shows up: **[Troubleshooting](Troubleshooting.md)**.
- Runnable end-to-end apps: **[Examples](Examples.md)**.
