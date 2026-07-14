# Sauron C# server example

A tiny .NET 8 console app that exercises the server-side Sauron SDK: initialize,
identify a user, track an event, and capture an exception, then flush and close.

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
- `SauronSdk.Identify(distinctId, traits)` — attach traits to a user.
- `SauronSdk.Track(eventName, distinctId, properties)` — record a product-analytics event.
- `SauronSdk.CaptureException(ex, user, level, tags)` — capture a native exception with an in-app stack trace.
- `SauronSdk.CaptureMessage(message, level)` — capture a plain message (not shown here).
- `SauronSdk.Flush()` / `SauronSdk.Close()` — drain the buffer and shut down before exit.

See [`Program.cs`](./Program.cs) for the full flow.
