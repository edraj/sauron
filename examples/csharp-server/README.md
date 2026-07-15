# Sauron C# server example

A tiny .NET 8 console app that exercises the server-side Sauron SDK (v0.3.0):
initialize, identify a user, open a per-request scope, track an event, leave a
breadcrumb, capture an exception, and time the request with a transaction — then
flush and close.

It references the shipped SDK directly via a project reference to
[`../../sdks/csharp/Sauron/Sauron.csproj`](../../sdks/csharp/Sauron/Sauron.csproj),
so no NuGet package install is required.

## Run

The app reads its ingest DSN from the `SAURON_DSN` environment variable:

```bash
export SAURON_DSN="https://<public_key>@<host>/<project_id>"
cd examples/csharp-server
dotnet run
```

If `SAURON_DSN` is unset or invalid, the SDK runs in **no-op mode** — the program
still runs to completion, it just doesn't dispatch anything over the network. This
makes it safe to build and run without a live ingest gateway.

## Build

```bash
cd examples/csharp-server
dotnet build
```

## What it does

The static `SauronSdk` facade (namespace `Sauron`) wraps a single process-wide client:

- `SauronSdk.Init(new SauronOptions { Dsn = ..., Environment = ..., Release = ... })` — initialize once at startup.
- `SauronSdk.SetTag(key, value)` / `SauronSdk.SetUser(user)` — set defaults on the active scope (the global scope when none is pushed).
- `SauronSdk.Identify(distinctId, traits)` — attach traits to a user.
- `using (SauronSdk.PushScope()) { ... }` — open an isolated per-request scope. User and tags set inside ride along on anything captured in the block, and are torn down on dispose so they never leak into other requests.
- `SauronSdk.Track(eventName, distinctId, properties)` — record a product-analytics event.
- `SauronSdk.AddBreadcrumb(new Breadcrumb { Category, Message, Level })` — leave a trail entry; breadcrumbs attach to errors captured afterwards.
- `SauronSdk.CaptureException(ex, level: ...)` — capture a native exception with an in-app stack trace. The scoped user, tags and breadcrumbs merge on automatically.
- `SauronSdk.TrackTransaction(name, durationMs, op, status, httpMethod, httpStatus, url)` — emit a performance transaction. `distinctId` (omitted here) falls back to the scoped user id.
- `SauronSdk.CaptureMessage(message, level)` — capture a plain message (not shown here).
- `SauronSdk.Flush()` / `SauronSdk.Close()` — drain the buffer and shut down before exit.

Every dispatch call is a no-op before `Init` and when the DSN is missing or disabled,
so the whole flow is safe to run without a live ingest gateway.

See [`Program.cs`](./Program.cs) for the full flow.
