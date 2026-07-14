# Getting Started

This walks you from nothing to your first signal landing in the dashboard.

See also: **[Home](Home.md)** · **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Examples](Examples.md)**

## 1. Create an app and get its DSN

In the dashboard, an **App** is the ingest unit — it belongs to a **Project**, which
belongs to an **Organization** (see **[Home](Home.md)** for the tenancy model).

1. Open the dashboard and go to **Projects** (the Manage section of the sidebar).
2. Create (or open) a project, then create an **App**. Pick an **app type** — see
   step 2.
3. Open the app's **settings** to read its **DSN**.

The DSN looks like:

```
https://<public_key>@<host>/<project_id>
```

- `<public_key>` is a **non-secret write key** — safe to ship in client code. There is
  no password component.
- `<host>` is the ingest host (may include a port, e.g. `localhost:8081`).
- `<project_id>` is the project id path segment (an id/UUID).

## 2. Pick an app type

The app type tells the dashboard which SDK and integration guide to show. Supported
types:

| App type | SDK | Wiki page |
| --- | --- | --- |
| `web` | `@sauron/browser` | [Browser SDK](Browser-SDK.md) |
| `flutter` | `sauron_flutter` | [Flutter SDK](Flutter-SDK.md) |
| `node` | `@sauron/node` | [Node SDK](Node-SDK.md) |
| `python` | `sauron-sdk` | [Python SDK](Python-SDK.md) |
| `csharp` | `Sauron` (`sauron-dotnet`) | [C# SDK](CSharp-SDK.md) |
| `ios`, `android`, `react_native` | — | (native mobile app types) |

## 3. Send your first event

Each SDK exposes the same core surface: **init**, **track**, **captureException**,
**identify**, **flush/close**. Below is the shortest path per SDK; all read the DSN
however is idiomatic for that platform (client SDKs take it in `init`, the example
servers read it from a `SAURON_DSN` env var).

### Browser (`@sauron/browser`)

```ts
import { Sauron } from '@sauron/browser';

Sauron.init({ dsn: 'https://<public_key>@<host>/<project_id>', release: 'web@1.0.0' });
Sauron.identify('u_123', { plan: 'pro' });
Sauron.track('checkout_completed', { cart_value: 42.5 });
```

### Flutter (`sauron_flutter`)

```dart
import 'package:sauron_flutter/sauron_flutter.dart';

await Sauron.init((o) {
  o.dsn = 'https://<public_key>@<host>/<project_id>';
  o.release = 'app@1.0.0+1';
}, appRunner: () => runApp(const MyApp()));

Sauron.track('checkout_completed', properties: {'cart_value': 42.5});
```

### Node (`@sauron/node`)

```ts
import { init, track } from '@sauron/node';

init({ dsn: process.env.SAURON_DSN!, environment: 'production' });
track('order_completed', 'user-123', { total: 42.5, currency: 'USD' });
```

### Python (`sauron-sdk`)

```python
import sauron

sauron.init(dsn="https://<public_key>@<host>/<project_id>")
sauron.track("checkout_completed", distinct_id="u_123", properties={"cart_value": 42.5})
```

### C# (`Sauron`)

```csharp
using Sauron;

SauronSdk.Init("https://<public_key>@<host>/<project_id>");
SauronSdk.Track("order_completed", "user-42", new Dictionary<string, object?> { ["total"] = 42.5 });
```

## 4. Flush before exit (server SDKs)

Client SDKs (browser, Flutter) flush in the background. Short-lived server processes
should flush and close so the buffer drains before the process exits:

- Node: `await close();`
- Python: `sauron.flush(); sauron.close()`
- C#: `SauronSdk.Flush(); SauronSdk.Close();`

## 5. Watch it land

Open the **[Dashboard](Dashboard.md)** — events appear under **Events**, grouped
errors under **Exceptions**, and identified people under **Users**.

## Next steps

- Full wire details: **[Ingest Wire Contract](Ingest-Wire-Contract.md)**.
- Runnable end-to-end apps: **[Examples](Examples.md)**.
