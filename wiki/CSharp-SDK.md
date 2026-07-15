# C# SDK — `Sauron`

Server-side .NET SDK (**v0.3.0**, `net8.0`, namespace `Sauron`). Dispatches
product-analytics events and captured exceptions over a buffered background HTTP
transport (`HttpClient` + a timer flush), with JSON via `System.Text.Json`. **No
auto-instrumentation** — a plain server-side dispatch API. Source:
[`sdks/csharp`](../sdks/csharp). SDK header name: `sauron-dotnet`.

See also: **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Examples](Examples.md)** · the runnable demo:
[`examples/csharp-server`](../examples/csharp-server).

## Install

Reference the project directly, or add the package once published:

```xml
<ProjectReference Include="path/to/sdks/csharp/Sauron/Sauron.csproj" />
```

```csharp
using Sauron;
```

## Init

Everything goes through the static `SauronSdk` facade over a single process-wide
client. Initialize once at startup:

```csharp
SauronSdk.Init("https://<public_key>@<host>/<project_id>");

// or with options:
SauronSdk.Init(new SauronOptions
{
    Dsn = "https://<public_key>@<host>/<project_id>",
    Environment = "production",
    Release = "1.4.2",
    Debug = true,
});
```

All dispatch calls are **no-ops until initialized**. `Init` replaces (and closes) any
previously-initialized client. `SauronSdk.Current` returns the current client (or
`null`).

### `SauronOptions`

| Property | Type | Default |
| --- | --- | --- |
| `Dsn` | `string` | `""` (required for dispatch) |
| `Environment` | `string` | `"production"` |
| `Release` | `string?` | `null` |
| `SampleRate` | `double` | `1.0` (errors) |
| `FlushInterval` | `TimeSpan` | `5 s` |
| `MaxBatch` | `int` | `30` |
| `Debug` | `bool` | `false` |
| `InAppInclude` | `IReadOnlyList<string>?` | `null` (everything outside `System.`/`Microsoft.` is in-app) |
| `MaxBreadcrumbs` | `int` | `100` |
| `BeforeBreadcrumb` | `Func<Breadcrumb, Breadcrumb?>?` | `null` (drop/mutate breadcrumbs) |
| `BeforeSend` | `Func<object, object?>?` | `null` (drop/mutate any outgoing item) |
| `GzipThresholdBytes` | `int` | `1024` (gzip the body over this size) |
| `MaxQueueBytes` | `int` | `1_048_576` (drop-oldest in-memory queue cap) |
| `OfflineDir` | `string?` | `null` (opt-in on-disk queue persistence) |
| `AutoCaptureUnhandled` | `bool` | `false` (opt-in uncaught-error capture) |

## API

| Method | Signature |
| --- | --- |
| `Init` | `Init(string dsn)` / `Init(SauronOptions options)` |
| `Track` | `Track(string @event, string distinctId, IReadOnlyDictionary<string, object?>? properties = null)` |
| `CaptureException` | `CaptureException(Exception exception, SauronUser? user = null, string level = "error", IReadOnlyDictionary<string, object?>? tags = null, IReadOnlyList<string>? fingerprint = null)` |
| `CaptureMessage` | `CaptureMessage(string message, string level = "info", IReadOnlyList<string>? fingerprint = null)` |
| `Identify` | `Identify(string distinctId, IReadOnlyDictionary<string, object?>? traits = null)` |
| `TrackTransaction` | `TrackTransaction(string name, double durationMs, string op = "custom", string? status = null, string? httpMethod = null, int? httpStatus = null, string? url = null, string? distinctId = null)` |
| `AddBreadcrumb` | `AddBreadcrumb(Breadcrumb breadcrumb)` |
| `SetUser` | `SetUser(SauronUser? user)` — pass `null` to clear |
| `SetTag` | `SetTag(string key, string value)` |
| `SetTags` | `SetTags(IReadOnlyDictionary<string, string> tags)` |
| `SetContext` | `SetContext(string key, object? value)` |
| `SetExtra` | `SetExtra(string key, object? value)` |
| `PushScope` | `IDisposable PushScope()` |
| `FlushAsync` | `FlushAsync(): Task` |
| `Flush` | `Flush()` (blocking) |
| `Close` | `Close()` — flush then stop |

`distinctId` is **required** on `Track`. `SauronUser` has `Id`, `Email`, `Username`.

### Track an event

```csharp
SauronSdk.Track("order_placed", "user-42", new Dictionary<string, object?>
{
    ["amount"] = 49.99,
    ["currency"] = "USD",
});
```

### Capture an exception

```csharp
try
{
    ProcessOrder("ord_1001");
}
catch (Exception ex)
{
    SauronSdk.CaptureException(
        ex,
        user: new SauronUser { Id = "user-42", Email = "ada@example.com" },
        level: "error",
        tags: new Dictionary<string, object?> { ["component"] = "checkout" },
        fingerprint: new[] { "checkout", "charge-failed" });
}

SauronSdk.CaptureMessage("nightly job finished", "info");
```

A supplied `fingerprint` is honored verbatim by the backend for grouping.

### Identify a user

```csharp
SauronSdk.Identify("user-42", new Dictionary<string, object?> { ["plan"] = "pro" });
```

## Scope, tags & context

A process-wide scope holds default user/tags/context/breadcrumbs; the facade setters
mutate the *active* scope (the global one outside a `PushScope`):

```csharp
SauronSdk.SetUser(new SauronUser { Id = "user-42", Email = "ada@example.com" }); // null clears
SauronSdk.SetTag("region", "eu-west-1");
SauronSdk.SetTags(new Dictionary<string, string> { ["tier"] = "pro", ["shard"] = "7" });
SauronSdk.SetContext("order", new { id = "ord_1001", items = 3 });
SauronSdk.SetExtra("cacheHit", false);
```

Scope tags/user/breadcrumbs are merged onto every captured error; per-call `tags`/`user`
win over scope values.

### Per-request isolation with `PushScope`

`ScopeManager` stores the active scope in an `AsyncLocal<Scope>`, so each request/task
gets its own layer over the global scope. `PushScope()` clones the current scope and
returns an `IDisposable` that restores the previous scope on `Dispose` — use it with a
`using` block:

```csharp
using (SauronSdk.PushScope())
{
    SauronSdk.SetUser(new SauronUser { Id = request.UserId });
    SauronSdk.SetTag("route", "POST /checkout");
    SauronSdk.AddBreadcrumb(new Breadcrumb { Category = "auth", Message = "token verified" });
    // any CaptureException in here inherits this scope
    Handle(request);
}
```

## Breadcrumbs

```csharp
SauronSdk.AddBreadcrumb(new Breadcrumb
{
    Type = "db", Category = "query", Message = "SELECT users", Level = "info",
    Data = new Dictionary<string, object?> { ["ms"] = 4 },
});
```

`Breadcrumb` defaults `Type` to `"default"` and `Timestamp` to `DateTimeOffset.UtcNow`.
The crumb lands on the active scope (ring-buffered at `MaxBreadcrumbs`, default 100) and
attaches to errors captured afterwards. A `BeforeBreadcrumb` hook runs first — return
`null` to drop it:

```csharp
new SauronOptions { BeforeBreadcrumb = c => c.Category == "noisy" ? null : c }
```

## `BeforeSend` (any item)

`BeforeSend` (`Func<object, object?>`) runs on **every** outgoing item (error, event,
identify, transaction) at the single enqueue chokepoint — return the (possibly replaced)
item to send it, or `null` to drop it. The item arrives as `object`: the concrete wire
DTOs (`ErrorItem`, `EventItem`, ...) are internal to the assembly, so match on the
runtime type name rather than casting:

```csharp
new SauronOptions
{
    BeforeSend = item =>
    {
        // Drop analytics events; send everything else.
        if (item.GetType().Name == "EventItem") return null;
        return item; // return null to drop, or return item to send
    },
}
```

## Performance transactions

```csharp
var sw = System.Diagnostics.Stopwatch.StartNew();
// ... handle request ...
SauronSdk.TrackTransaction(
    "GET /api/users", sw.Elapsed.TotalMilliseconds, op: "http",
    httpMethod: "GET", httpStatus: 200, url: "/api/users");
```

`op` defaults to `"custom"`; the JSON uses snake_case (`duration_ms`, `http_method`,
`http_status`, `distinct_id`). `distinctId` falls back to the scoped user's id.

## Gzip, retry & the offline queue

- **Gzip** — the request body is gzipped once it exceeds `GzipThresholdBytes` (default
  1024), with `Content-Encoding: gzip`; smaller bodies go out uncompressed (`GZipStream`).
- **Retry** — the transport retries transient failures (408/413/429/5xx and network
  errors) with backoff, honoring `Retry-After` on 429; non-retryable 4xx drop the batch
  and 401/403 disable the SDK.
- **Queue** — pending envelopes buffer in a byte-bounded queue (`MaxQueueBytes`, default
  1 MiB, drop-oldest). Set `OfflineDir` to persist them FIFO to disk (reloaded on the
  next start, deleted on delivery) for at-least-once delivery across restarts.

## Auto-capture & graceful shutdown

`AutoCaptureUnhandled = true` (opt-in, default `false`, enabled clients only) subscribes
to `AppDomain.CurrentDomain.UnhandledException` and
`TaskScheduler.UnobservedTaskException`, capturing each with `mechanism.handled = false`
and preserving the runtime's default crash/exit behavior:

```csharp
SauronSdk.Init(new SauronOptions { Dsn = dsn, AutoCaptureUnhandled = true });
```

`SauronClient` is `IDisposable`; `Close()`/`Dispose()` unsubscribes those handlers and
flushes the transport. Call `Close()` before the process exits:

```csharp
SauronSdk.Flush();   // blocking; FlushAsync() for the awaitable form
SauronSdk.Close();   // flush then stop — call before the process exits
```

## Example

See [`examples/csharp-server`](../examples/csharp-server). Run it with:

```bash
export SAURON_DSN="https://<public_key>@<host>/<project_id>"
cd examples/csharp-server
dotnet run
```

If `SAURON_DSN` is unset or invalid, the SDK runs in no-op mode and the program still
completes. Build the SDK with `cd sdks/csharp && dotnet build && dotnet test`. More in
**[Examples](Examples.md)**.
