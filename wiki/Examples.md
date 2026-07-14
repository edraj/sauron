# Examples

Runnable, copy-pasteable apps that exercise each SDK end-to-end. All live under
[`examples/`](../examples). Each server example reads its DSN from the `SAURON_DSN`
environment variable; the client examples carry an editable/default DSN.

See also: **[Getting Started](Getting-Started.md)** ·
**[Ingest Wire Contract](Ingest-Wire-Contract.md)**.

| Example | SDK | Directory |
| --- | --- | --- |
| Svelte web | `@sauron/browser` | [`examples/svelte-web`](../examples/svelte-web) |
| Flutter app | `sauron_flutter` | [`examples/flutter-app`](../examples/flutter-app) |
| Node server | `@sauron/node` | [`examples/node-server`](../examples/node-server) |
| Python server | `sauron-sdk` | [`examples/python-server`](../examples/python-server) |
| C# server | `Sauron` | [`examples/csharp-server`](../examples/csharp-server) |

## Svelte web — `@sauron/browser`

A Vite + Svelte 5 single-page app that showcases the browser SDK: crashes, product
events, identify, screens, performance. See
[`examples/svelte-web/README.md`](../examples/svelte-web/README.md). Depends on the
local SDK via `"@sauron/browser": "file:../../sdks/js"`.

```bash
cd examples/svelte-web
npm install
npm run dev        # then open the printed local URL
```

## Flutter app — `sauron_flutter`

A Material 3 app that exercises all four uncaught-error layers, analytics, identify,
and a synthetic funnel/journey/performance showcase. See
[`examples/flutter-app/README.md`](../examples/flutter-app/README.md). Uses a path
dependency on [`sdks/flutter`](../sdks/flutter).

```bash
cd examples/flutter-app
flutter pub get
flutter run
```

## Node server — `@sauron/node`

A tiny backend exercising `init → identify → track → captureException →
flush/close`. See [`examples/node-server/README.md`](../examples/node-server/README.md).
Depends on the local SDK via `"@sauron/node": "file:../../sdks/node"`.

```bash
cd examples/node-server
npm install
SAURON_DSN="https://<public_key>@<host>/<project_id>" npm start
# typecheck only:
npm run typecheck
```

## Python server — `sauron-sdk`

Identifies a user, tracks an event, and captures a deliberate exception. See
[`examples/python-server/README.md`](../examples/python-server/README.md).

```bash
cd examples/python-server
pip install -e ../../sdks/python
SAURON_DSN="https://<public_key>@<host>/<project_id>" python main.py
```

## C# server — `Sauron`

A .NET 8 console app that initializes, identifies, tracks, and captures an exception.
See [`examples/csharp-server/README.md`](../examples/csharp-server/README.md).
References the shipped SDK via a project reference.

```bash
export SAURON_DSN="https://<public_key>@<host>/<project_id>"
cd examples/csharp-server
dotnet run
```

If `SAURON_DSN` is unset or invalid, the SDK runs in no-op mode and the program still
completes.

---

For per-SDK API details, jump to the matching page: **[Browser SDK](Browser-SDK.md)** ·
**[Flutter SDK](Flutter-SDK.md)** · **[Node SDK](Node-SDK.md)** ·
**[Python SDK](Python-SDK.md)** · **[C# SDK](CSharp-SDK.md)**.
