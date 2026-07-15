# Sauron (.NET)

Server-side .NET SDK for [Sauron](https://sauron.dev) — dispatch product-analytics
events, captured exceptions, identify calls and performance transactions from your
.NET backends to the Sauron ingest gateway.

This is the **server-side** SDK (`System.*` only, no ASP.NET/DI coupling). Target
framework: `net8.0`.

## Usage

```csharp
using Sauron;

SauronSdk.Init(new SauronOptions
{
    Dsn = "https://<public_key>@<host>/<project_id>",
    Environment = "production",
    Release = "1.4.2",
    // Opt-in: capture uncaught exceptions (mechanism.handled = false), off by default.
    AutoCaptureUnhandled = false,
});

// Product analytics — distinctId is required.
SauronSdk.Track("order_completed", "user-123",
    new Dictionary<string, object?> { ["total"] = 42.5, ["currency"] = "USD" });

// Errors.
try { DoWork(); }
catch (Exception ex) { SauronSdk.CaptureException(ex); }

// Identify.
SauronSdk.Identify("user-123", new Dictionary<string, object?> { ["plan"] = "pro" });

// Flush + stop (e.g. at process shutdown).
SauronSdk.Close();
```

## Scope, breadcrumbs, tags & user

Per-request isolation uses `AsyncLocal`. Set global defaults on the ambient scope, or
push an isolated scope for the duration of a request with `using`:

```csharp
using (SauronSdk.PushScope())
{
    SauronSdk.SetUser(new SauronUser { Id = "user-123", Email = "a@b.co" });
    SauronSdk.SetTag("route", "/checkout");
    SauronSdk.AddBreadcrumb(new Breadcrumb { Type = "navigation", Message = "entered checkout" });

    // A captured error automatically carries the scope's user, tags and breadcrumb trail.
    try { Checkout(); }
    catch (Exception ex) { SauronSdk.CaptureException(ex); }
} // scope restored on dispose — no leak into the next request
```

An optional `fingerprint` override controls grouping (honored verbatim by the backend):

```csharp
SauronSdk.CaptureException(ex, fingerprint: new[] { "checkout", "timeout" });
```

## Transactions (performance)

```csharp
SauronSdk.TrackTransaction("GET /api/users", durationMs: 12.5, op: "http",
    status: "ok", httpMethod: "GET", httpStatus: 200, url: "/api/users");
```

`distinctId` falls back to the scoped user's id when omitted. `op` is one of
`navigation | http | resource | screen_load | custom` (defaults to `custom`).

## PII scrubbing / redaction

- `BeforeSend` runs on **every** outgoing item (event, error, identify, transaction);
  return the (possibly mutated) item to send it, or `null` to drop it.
- `BeforeBreadcrumb` runs on each breadcrumb before it is recorded.

```csharp
SauronSdk.Init(new SauronOptions
{
    Dsn = dsn,
    BeforeSend = item =>
    {
        if (item is EventItem) { /* redact properties */ }
        return item; // or null to drop
    },
});
```

## Transport

- **Gzip** — the request body is gzipped (with `Content-Encoding: gzip`) once it exceeds
  `GzipThresholdBytes` (default 1024).
- **Retry / backoff** — transient failures (408/413/429/5xx and network errors) are retried
  with exponential backoff + jitter (cap 30s), honoring `Retry-After` on 429; 400/401/403/404
  are dropped.
- **Bounded queue** — pending envelopes are held in a byte-capped in-memory ring
  (`MaxQueueBytes`, default 1 MiB, drop-oldest). Set `OfflineDir` to persist them to disk
  FIFO for at-least-once delivery across restarts (opt-in, off by default).

## No-op safety

Every dispatch API is a no-op before `Init` and when the DSN is missing/invalid — calls
never throw in that state.

## Version

`0.3.0` — see [CHANGELOG.md](CHANGELOG.md).
