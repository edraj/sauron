# Sauron (.NET)

Server-side .NET SDK for [Sauron](https://github.com/edraj/sauron) — dispatch
product-analytics events, captured exceptions, identify calls and performance
transactions from your .NET backends to the Sauron ingest gateway.

This is the **server-side** SDK: `System.*` only, no ASP.NET/DI package
dependency and no auto-instrumentation. If you are instrumenting a browser
front-end, use `@edraj/sauron-browser` (`sdks/js`) instead; for Flutter apps use
`sauron_flutter` (`sdks/flutter`).

- Explicit dispatch API — `Track`, `CaptureException`, `CaptureMessage`,
  `Identify`, `TrackTransaction`. Nothing is captured behind your back.
- Per-request attribution via an `AsyncLocal` scope stack: user, tags, contexts,
  extra and a bounded breadcrumb ring.
- Opt-in global uncaught-error capture (`AppDomain.UnhandledException` +
  `TaskScheduler.UnobservedTaskException`), off by default.
- Buffered background transport: batching, gzip, bounded FIFO queue with
  optional on-disk persistence, retry/backoff honoring `Retry-After`.
- Zero third-party dependencies. `net8.0`, `Nullable` enabled, assembly and
  namespace `Sauron`.

## Install

**The package is not published to NuGet yet.** `sdks/PUBLISHING.md` covers npm,
PyPI and pub.dev only and states explicitly that `sdks/csharp` "targets NuGet and
is **not** covered here". Until a package exists, reference the project directly:

```bash
dotnet add <your-project>.csproj reference path/to/sdks/csharp/Sauron/Sauron.csproj
```

which writes:

```xml
<ItemGroup>
  <ProjectReference Include="path/to/sdks/csharp/Sauron/Sauron.csproj" />
</ItemGroup>
```

Or build a local package and consume it from a local feed:

```bash
cd sdks/csharp
dotnet pack Sauron/Sauron.csproj -c Release -o ./nupkg
dotnet nuget add source "$(pwd)/nupkg" --name sauron-local
dotnet add <your-project>.csproj package Sauron --version 1.5.0
```

Once published, the install command will be:

```bash
dotnet add package Sauron
```

Target framework: **`net8.0`**. It runs on newer runtimes; consuming projects on
net9/net10 without the net8 runtime installed should set
`<RollForward>LatestMajor</RollForward>` (see `examples/csharp-server`).

## Quick start

```csharp
using Sauron;

SauronSdk.Init(new SauronOptions
{
    Dsn = Environment.GetEnvironmentVariable("SAURON_DSN")!,
    Release = "1.4.2",
});

// Product analytics — distinctId is required.
SauronSdk.Track("order_completed", "user-123", new Dictionary<string, object?>
{
    ["total"] = 42.5,
    ["currency"] = "USD",
});

// Errors.
try
{
    throw new InvalidOperationException("checkout failed");
}
catch (Exception ex)
{
    SauronSdk.CaptureException(ex);
}

// Flush and stop — the buffer is in memory, so do this before the process exits.
SauronSdk.Close();
```

DSN format: `https://<public_key>@<host>/<environment_id>`. The SDK POSTs a canonical
JSON envelope (`header` + `context` + `items[]`, identical across all Sauron
SDKs) to `POST /api/{environment_id}/envelope`, authenticated with the
`X-Sauron-Key: <public_key>` header. Bodies over the gzip threshold are sent with
`Content-Encoding: gzip`.

If the DSN is missing or unparseable the client enters **no-op mode**: `Init`
does not throw, `Enabled` is `false`, and every dispatch call silently returns.

## Configuration

`SauronOptions` is a plain settable class; all properties are read at
`SauronClient` construction time. Mutating it afterwards has no effect.

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `Dsn` | `string` | `""` (**required**) | Ingest DSN, `https://<public_key>@<host>/<environment_id>`. Empty or invalid puts the client in no-op mode. |
| `Release` | `string?` | `null` | Release identifier, sent in `header.release`. |
| `Tags` | `IReadOnlyDictionary<string, string>?` | `null` | Default tags seeded into the process-wide global scope at construction. |
| `Contexts` | `IReadOnlyDictionary<string, object?>?` | `null` | Default context blocks (name → block) seeded into the global scope. Distinct from the machine `context` in the envelope. |
| `Extra` | `IReadOnlyDictionary<string, object?>?` | `null` | Default extra values (key → any) seeded into the global scope. |
| `SampleRate` | `double` | `1.0` | Sample rate in [0, 1] applied to `CaptureException` **only**. See the note below. |
| `FlushInterval` | `TimeSpan` | `TimeSpan.FromSeconds(5)` | Background flush cadence. A value `<= TimeSpan.Zero` disables the timer entirely (flush only on `MaxBatch`, `Flush`/`FlushAsync`, or `Close`). |
| `MaxBatch` | `int` | `30` | Flush automatically once this many items are buffered. Clamped to a minimum of 1. |
| `MaxItemsPerEnvelope` | `int` | `1000` | Hard ceiling on items per envelope, matching the server limit. A flush carrying more is split into several envelopes. Clamped to a minimum of 1. |
| `Debug` | `bool` | `false` | Write diagnostics to `stderr`, prefixed `[sauron]`. |
| `InAppInclude` | `IReadOnlyList<string>?` | `null` | Module prefixes treated as in-app for stack frames. When `null`, every module outside `System.` and `Microsoft.` is in-app. |
| `MaxBreadcrumbs` | `int` | `100` | Ring-buffer size for breadcrumbs on a scope; the oldest are dropped. Negative is treated as 0. |
| `BeforeBreadcrumb` | `Func<Breadcrumb, Breadcrumb?>?` | `null` | Runs on each breadcrumb before it is recorded. Return the crumb (possibly mutated) to keep it, `null` to drop it. If it throws, the crumb is dropped. |
| `BeforeSend` | `Func<object, object?>?` | `null` | Runs on every outgoing item just before buffering. Return the item to send it, `null` to drop it. If it throws, the item is dropped. |
| `GzipThresholdBytes` | `int` | `1024` | Gzip the serialized body when it is strictly larger than this many bytes. `int.MaxValue` (or a negative value) disables compression. |
| `MaxQueueBytes` | `int` | `1_048_576` (1 MiB) | Byte cap on the pending-envelope queue. Over the cap, the oldest envelopes are dropped. Negative is treated as 0. |
| `OfflineDir` | `string?` | `null` | Opt-in directory for FIFO on-disk queue persistence (at-least-once delivery across restarts). An unusable directory falls back to memory-only. |
| `AutoCaptureUnhandled` | `bool` | `false` | Opt-in: subscribe to `AppDomain.UnhandledException` and `TaskScheduler.UnobservedTaskException`. Never installed for a no-op client. |
| `HttpMessageHandler` | `HttpMessageHandler?` | `null` | Inject a custom handler (test fake, proxy, custom TLS). When `null` a single process-wide static `HttpClient` is used. |

Note on `SampleRate`: it is applied inside the exception-capture path only, and
only when `< 1.0`. `CaptureMessage`, `Track`, `Identify`, `TrackTransaction` and
auto-captured uncaught exceptions are never sampled.

Fully-populated example:

```csharp
var options = new SauronOptions
{
    Dsn = "https://pub_abc123@sauron.example.com/proj_42",
    Release = "api@1.4.2",
    Tags = new Dictionary<string, string> { ["service"] = "checkout" },
    Contexts = new Dictionary<string, object?>
    {
        ["deploy"] = new Dictionary<string, object?> { ["region"] = "eu-west-1" },
    },
    Extra = new Dictionary<string, object?> { ["build"] = "9f31c2a" },
    SampleRate = 0.5,
    FlushInterval = TimeSpan.FromSeconds(2),
    MaxBatch = 50,
    MaxItemsPerEnvelope = 500,
    Debug = true,
    InAppInclude = new[] { "Acme.Api.", "Acme.Domain." },
    MaxBreadcrumbs = 50,
    BeforeBreadcrumb = crumb =>
    {
        if (crumb.Category == "sql") return null;   // drop
        crumb.Data?.Remove("password");             // mutate
        return crumb;
    },
    BeforeSend = item => item,                      // see the BeforeSend section
    GzipThresholdBytes = 2048,
    MaxQueueBytes = 4 * 1024 * 1024,
    OfflineDir = "/var/lib/myapp/sauron-queue",
    AutoCaptureUnhandled = true,
    HttpMessageHandler = null,
};

SauronSdk.Init(options);
```

## API reference

### Facade or client?

Two equivalent entry points:

- **`SauronSdk`** — a static facade over one process-wide `SauronClient`. Use it
  for apps: one `Init` at startup, then static calls anywhere. Every dispatch
  method is a no-op before `Init`.
- **`SauronClient`** — the client itself, constructible and injectable. Use it
  when you want DI ownership, deterministic disposal, or more than one client
  (e.g. dispatching to two projects). Register it as a singleton.

The dispatch and lifecycle methods — `Track`, `CaptureException`,
`CaptureMessage`, `Identify`, `TrackTransaction`, `AddBreadcrumb`, `StartWorkflow`,
`EndWorkflow`, `CancelWorkflow`, `GetWorkflow`, `FlushAsync`, `Flush`, `Close` — exist
in **both** forms with identical parameters: `static` on `SauronSdk`, instance on
`SauronClient`. The signatures below are shown in the instance form; prefix them
with `SauronSdk.` for the static one.

Unlike the rest of the scope API, `StartWorkflow`/`EndWorkflow`/`CancelWorkflow` are
**not** ambient-only: they need an initialized, enabled client to emit the lifecycle
event and return `Disabled` otherwise. `GetWorkflow` is a pure read of the active
scope, so — like `SetUser`/`SetTag`/etc. — it works even before `Init`.

The scope API (`SetUser`, `SetTag`, `SetTags`, `SetContext`, `SetExtra`,
`PushScope`) is **static-only and process-ambient**, not per-client: an injected
`SauronClient` reads the same `AsyncLocal` scope stack the static facade writes
to. Those methods therefore work even before `Init`.

---

### `SauronSdk.Init`

```csharp
public static void Init(string dsn)
public static void Init(SauronOptions options)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `dsn` | `string` | — | DSN string. Shorthand for `Init(new SauronOptions { Dsn = dsn })` — every other option keeps its default. |
| `options` | `SauronOptions` | — | Full options object. Required, non-null. |

Returns `void`. Constructs a new `SauronClient`, installs it as the process-wide
one, then **closes the previous client** (which performs a final blocking flush).
Calling `Init` again is therefore a safe hot-swap, not a leak.

```csharp
SauronSdk.Init("https://pub_abc123@sauron.example.com/proj_42");

// or
SauronSdk.Init(new SauronOptions
{
    Dsn = builder.Configuration["Sauron:Dsn"]!,
    Release = ThisAssembly.InformationalVersion,
});
```

### `SauronSdk.Current`

```csharp
public static SauronClient? Current { get; }
```

No parameters. Returns the process-wide client, or `null` before `Init` (and
after `Close`). Useful to check `Enabled` or to hand the client to code that
prefers an instance.

```csharp
if (SauronSdk.Current is { Enabled: false })
    logger.LogWarning("Sauron is running in no-op mode — check SAURON_DSN");
```

### `new SauronClient(...)`

```csharp
public SauronClient(SauronOptions options)
public bool Enabled { get; }
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `options` | `SauronOptions` | — | Required. `ArgumentNullException` when `null`. |

The constructor never throws on a bad DSN — it logs (when `Debug`) and produces a
no-op client. `Enabled` is `false` for a no-op client, and also flips to `false`
permanently after the ingest answers `401`/`403` (a bad key is never retried).

```csharp
using var client = new SauronClient(new SauronOptions { Dsn = dsn });
if (!client.Enabled) Console.Error.WriteLine("sauron disabled");
client.Track("job_finished", "worker-1");
```

### `CaptureException`

```csharp
public void CaptureException(
    Exception exception,
    SauronUser? user = null,
    string level = "error",
    IReadOnlyDictionary<string, object?>? tags = null,
    IReadOnlyList<string>? fingerprint = null,
    IReadOnlyDictionary<string, object?>? contexts = null,
    IReadOnlyDictionary<string, object?>? extra = null)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `exception` | `Exception` | — | Required. `ArgumentNullException` when `null` (on an enabled client). |
| `user` | `SauronUser?` | `null` | Per-call user. Wins over the scope user. |
| `level` | `string` | `"error"` | `debug\|info\|warning\|error\|fatal`. An empty string falls back to `"error"`. |
| `tags` | `IReadOnlyDictionary<string, object?>?` | `null` | Per-call tags, merged over scope tags by key. |
| `fingerprint` | `IReadOnlyList<string>?` | `null` | Grouping override, honored verbatim by the backend. |
| `contexts` | `IReadOnlyDictionary<string, object?>?` | `null` | Per-call context blocks (name → block), merged over scope contexts by block name. |
| `extra` | `IReadOnlyDictionary<string, object?>?` | `null` | Per-call extra values, merged over scope extra by key. |

Returns `void`. Emits an `error` item with `mechanism.type = "generic"` and
`mechanism.handled = true`, a parsed stack trace (frames ordered call-site →
crash, crash frame **last**), and the active scope's user/tags/contexts/extra and
breadcrumb trail. Subject to `SampleRate`.

```csharp
try
{
    await _payments.ChargeAsync(orderId);
}
catch (PaymentDeclinedException ex)
{
    SauronSdk.CaptureException(ex,
        user: new SauronUser { Id = "user-123", Email = "a@b.co" },
        level: "warning",
        tags: new Dictionary<string, object?> { ["gateway"] = "stripe" },
        fingerprint: new[] { "payment", "declined" },
        contexts: new Dictionary<string, object?>
        {
            ["order"] = new Dictionary<string, object?> { ["id"] = orderId },
        },
        extra: new Dictionary<string, object?> { ["attempt"] = 2 });
}
```

### `CaptureMessage`

```csharp
public void CaptureMessage(
    string message,
    string level = "info",
    IReadOnlyList<string>? fingerprint = null,
    IReadOnlyDictionary<string, object?>? tags = null,
    IReadOnlyDictionary<string, object?>? contexts = null,
    IReadOnlyDictionary<string, object?>? extra = null)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `message` | `string` | — | Required. `ArgumentNullException` when `null` (on an enabled client). |
| `level` | `string` | `"info"` | An empty string falls back to `"info"`. |
| `fingerprint` | `IReadOnlyList<string>?` | `null` | Grouping override. |
| `tags` | `IReadOnlyDictionary<string, object?>?` | `null` | Per-call tags. |
| `contexts` | `IReadOnlyDictionary<string, object?>?` | `null` | Per-call context blocks. |
| `extra` | `IReadOnlyDictionary<string, object?>?` | `null` | Per-call extra values. |

Returns `void`. Emits an `error` item with `exception = null` and the `message`
field set. Not sampled — `SampleRate` does not apply here.

```csharp
SauronSdk.CaptureMessage("cache rebuild took unusually long",
    level: "warning",
    tags: new Dictionary<string, object?> { ["cache"] = "catalog" },
    extra: new Dictionary<string, object?> { ["seconds"] = 41.2 });
```

### `Track`

```csharp
public void Track(
    string @event,
    string distinctId,
    IReadOnlyDictionary<string, object?>? properties = null,
    IReadOnlyDictionary<string, object?>? tags = null,
    IReadOnlyDictionary<string, object?>? contexts = null,
    IReadOnlyDictionary<string, object?>? extra = null)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `@event` | `string` | — | Required event name. `ArgumentException` when null/empty (on an enabled client). |
| `distinctId` | `string` | — | Required user identifier — the wire contract has no anonymous fallback. `ArgumentException` when null/empty. |
| `properties` | `IReadOnlyDictionary<string, object?>?` | `null` | Event properties. `null` serializes as `{}`. |
| `tags` | `IReadOnlyDictionary<string, object?>?` | `null` | Per-call tags, merged over scope tags. |
| `contexts` | `IReadOnlyDictionary<string, object?>?` | `null` | Per-call context blocks. |
| `extra` | `IReadOnlyDictionary<string, object?>?` | `null` | Per-call extra values. |

Returns `void`. Emits an `event` item. Scope tags/contexts/extra are merged in;
the scope **user and breadcrumbs are not** attached to analytics events. When
neither the call nor the scope supplies tags/contexts/extra, those fields are
omitted from the wire rather than sent empty.

```csharp
SauronSdk.Track("order_completed", "user-123",
    properties: new Dictionary<string, object?> { ["total"] = 42.5, ["currency"] = "USD" },
    tags: new Dictionary<string, object?> { ["plan"] = "pro" },
    extra: new Dictionary<string, object?> { ["experiment"] = "checkout-v2" });
```

### `Identify`

```csharp
public void Identify(string distinctId, IReadOnlyDictionary<string, object?>? traits = null)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `distinctId` | `string` | — | Required. `ArgumentException` when null/empty (on an enabled client). |
| `traits` | `IReadOnlyDictionary<string, object?>?` | `null` | User traits. `null` serializes as `{}`. |

Returns `void`. Emits an `identify` item. Scope metadata is not merged into
identify items.

```csharp
SauronSdk.Identify("user-123", new Dictionary<string, object?>
{
    ["email"] = "a@b.co",
    ["plan"] = "pro",
    ["signup_date"] = "2026-01-14",
});
```

### `TrackTransaction`

```csharp
public void TrackTransaction(
    string name,
    double durationMs,
    string op = "custom",
    string? status = null,
    string? httpMethod = null,
    int? httpStatus = null,
    string? url = null,
    string? distinctId = null,
    IReadOnlyDictionary<string, object?>? tags = null,
    IReadOnlyDictionary<string, object?>? extra = null)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string` | — | Required route/screen/operation label — the grouping key. `ArgumentException` when null/empty (on an enabled client). |
| `durationMs` | `double` | — | Required duration in milliseconds. |
| `op` | `string` | `"custom"` | One of `navigation\|http\|resource\|screen_load\|custom`. An empty string falls back to `"custom"`. |
| `status` | `string?` | `null` | Outcome label, e.g. `"ok"`, `"cancelled"`. |
| `httpMethod` | `string?` | `null` | HTTP method for `op: "http"`. |
| `httpStatus` | `int?` | `null` | Response status for `op: "http"`. |
| `url` | `string?` | `null` | Request URL/path. |
| `distinctId` | `string?` | `null` | User id. When omitted, falls back to the active scope's user id, then to `null`. |
| `tags` | `IReadOnlyDictionary<string, object?>?` | `null` | Indexed string→string labels. Filter with `@tag.key:value` on the Transactions page. |
| `extra` | `IReadOnlyDictionary<string, object?>?` | `null` | Freeform JSON — request body, response body, SQL text, row counts. Searchable with `extra.key:value`. |

Returns `void`. Emits a `transaction` item. Scope tags/contexts/extra are **not**
merged into transactions — only the user-id fallback applies. Transactions are
the highest-volume signal a service emits (one per request and per query), so
inheriting a global blob would write it onto every row; pass what this call site
knows and nothing more.

`extra` is serialized and capped at **16 KB** (`TransactionExtra.MaxBytes`).
Past that the whole map is replaced with `{ "_truncated": true, "_bytes": N }`
and the dashboard says so on the row. The cap is not cosmetic: envelopes are
batched, and one oversized body would push the whole envelope past the ingest
limit and drop every unrelated span sent with it. A value the serializer cannot
handle becomes the same marker with `"_bytes": -1` rather than throwing.

Nothing in `extra` is scrubbed. Use `BeforeSend` for redaction, and think twice
before attaching a body that can carry tokens, passwords or personal data.

```csharp
var sw = Stopwatch.StartNew();
await handler.InvokeAsync();
sw.Stop();

SauronSdk.TrackTransaction(
    name: "GET /api/users",
    durationMs: sw.Elapsed.TotalMilliseconds,
    op: "http",
    status: "ok",
    httpMethod: "GET",
    httpStatus: 200,
    url: "/api/users",
    distinctId: "user-123");
```

#### Example: an ASP.NET Core request, with request and response bodies

Middleware, so every route gets it. Both streams need help: the request body is
forward-only until `EnableBuffering()`, and the response body has to be
swapped for a `MemoryStream` to be readable after the pipeline runs.

```csharp
using System.Diagnostics;
using Sauron;

public sealed class SauronTransactionMiddleware
{
    private readonly RequestDelegate _next;

    public SauronTransactionMiddleware(RequestDelegate next) => _next = next;

    public async Task InvokeAsync(HttpContext ctx)
    {
        // A request body is a forward-only stream; without this it is already
        // consumed by the time the handler returns.
        ctx.Request.EnableBuffering();
        var requestBody = await new StreamReader(ctx.Request.Body).ReadToEndAsync();
        ctx.Request.Body.Position = 0;

        // Same problem downstream: swap in a seekable buffer, then copy it
        // back to the real stream so the client still gets its response.
        var original = ctx.Response.Body;
        using var buffer = new MemoryStream();
        ctx.Response.Body = buffer;

        var sw = Stopwatch.StartNew();
        try
        {
            await _next(ctx);
        }
        finally
        {
            sw.Stop();
            buffer.Position = 0;
            var responseBody = await new StreamReader(buffer).ReadToEndAsync();
            buffer.Position = 0;
            await buffer.CopyToAsync(original);
            ctx.Response.Body = original;

            // The ROUTE PATTERN, not the resolved path: "/orders/{id}" groups,
            // "/orders/8412" mints a new dashboard row per request.
            var label = ctx.GetEndpoint()?.DisplayName ?? ctx.Request.Path.Value ?? "unknown";

            SauronSdk.TrackTransaction(
                name: $"{ctx.Request.Method} {label}",
                durationMs: sw.Elapsed.TotalMilliseconds,
                op: "http",
                status: ctx.Response.StatusCode < 400 ? "ok" : "error",
                httpMethod: ctx.Request.Method,
                httpStatus: ctx.Response.StatusCode,
                url: ctx.Request.Path.Value,
                distinctId: ctx.User?.Identity?.Name,
                tags: new Dictionary<string, object?>
                {
                    ["route"] = label,
                    ["tier"] = ctx.User?.FindFirst("plan")?.Value ?? "free",
                },
                extra: new Dictionary<string, object?>
                {
                    ["request"] = requestBody,
                    ["response"] = responseBody,
                    ["query"] = ctx.Request.QueryString.Value,
                    // Header VALUES are omitted on purpose — `Authorization`
                    // and `Cookie` live there.
                    ["request_headers"] = ctx.Request.Headers.Keys.ToArray(),
                });
        }
    }
}

// Program.cs
app.UseMiddleware<SauronTransactionMiddleware>();
```

On the dashboard: **Transactions → the row → expand**. Both bodies render as a
JSON tree, and every one of these finds it:

```text
extra.response:~9001        # substring, inside the stored response body
@tag.route:/orders          # indexed tag
op:http http.status:>=500   # the failures
duration:>2s                # the slow ones
```

#### Example: a SQL query (`Npgsql`)

Put the **statement** in `extra` and keep `name` a stable label — a query with
literals baked in would mint a new dashboard row per execution.

```csharp
using System.Diagnostics;
using Npgsql;
using Sauron;

public static async Task<List<Order>> RecentOrdersAsync(NpgsqlDataSource db, Guid userId)
{
    const string Sql =
        "SELECT id, total FROM orders WHERE user_id = $1 ORDER BY created_at DESC LIMIT 20";

    var sw = Stopwatch.StartNew();
    var extra = new Dictionary<string, object?>
    {
        ["statement"] = Sql,
        // Bind PARAMETERS are user data. Log them only if you have decided
        // that is acceptable, or log their shape instead.
        ["params"] = new object?[] { userId },
    };

    try
    {
        await using var cmd = db.CreateCommand(Sql);
        cmd.Parameters.AddWithValue(userId);
        await using var reader = await cmd.ExecuteReaderAsync();

        var rows = new List<Order>();
        while (await reader.ReadAsync())
            rows.Add(new Order(reader.GetGuid(0), reader.GetDecimal(1)));

        sw.Stop();
        extra["row_count"] = rows.Count;
        SauronSdk.TrackTransaction(
            name: "SELECT orders",          // the LABEL, not the statement
            durationMs: sw.Elapsed.TotalMilliseconds,
            op: "db",
            status: "ok",
            tags: new Dictionary<string, object?> { ["db"] = "postgres", ["table"] = "orders" },
            extra: extra);
        return rows;
    }
    catch (NpgsqlException ex)
    {
        sw.Stop();
        extra["error"] = ex.Message;
        SauronSdk.TrackTransaction(
            name: "SELECT orders",
            durationMs: sw.Elapsed.TotalMilliseconds,
            op: "db",
            status: "error",
            tags: new Dictionary<string, object?> { ["db"] = "postgres", ["table"] = "orders" },
            extra: extra);
        throw;
    }
}
```

Then `@tag.table:orders duration:>500ms` is your slow-query list.

### `StartWorkflow`

```csharp
public WorkflowResult StartWorkflow(string name, bool force = false)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string` | — | Workflow name. Trimmed; rejected (not truncated) if empty after trimming or over 120 characters. |
| `force` | `bool` | `false` | When an active workflow already exists: `false` makes this a no-op returning `AlreadyActive`; `true` supersedes it — the active workflow is closed with `$workflow_cancel` (`reason: "superseded"`) before the new one starts. |

Returns a `WorkflowResult { Status, WorkflowId }`. Starts a named, explicitly-bounded span of
activity: from this call until `EndWorkflow`/`CancelWorkflow`, every `Track`,
`CaptureException`, `CaptureMessage` and `TrackTransaction` call is stamped with
`workflow_id`/`workflow_name`, and a `$workflow_start` analytics event is emitted (also
carrying `workflow_id`/`workflow_name` in its `properties`). `workflow_id` is a fresh
`Guid.NewGuid()` minted on every call — never derived from anything (session, user, name
hash) — because the server rolls counters up on `(app_id, workflow_id)` app-wide; a reused
or deterministic id would merge unrelated environments' counts into one row.

Workflows are entirely optional: an app that never calls `StartWorkflow` sees no
`workflow_id`/`workflow_name` fields on any item, ever — they are omitted from the wire, not
sent as `null`.

| `Status` | Meaning |
| --- | --- |
| `Ok` | Started (or, with `force: true`, superseded the previous one and started). |
| `AlreadyActive` | A workflow is already active and `force` was not passed. |
| `InvalidName` | `name` is empty after trimming, or over 120 characters after trimming. |
| `Disabled` | No initialized/enabled client (before `Init`, after `Close`/dispose, after a `401`/`403`), **or an unexpected internal error.** |

```csharp
using (SauronSdk.PushScope())
{
    var result = SauronSdk.StartWorkflow("checkout");
    if (result.Status != WorkflowStatus.Ok)
        logger.LogWarning("StartWorkflow failed: {Status}", result.Status);

    // ... tracked events / captured errors here are stamped with this workflow ...

    SauronSdk.EndWorkflow();
}
```

### `EndWorkflow`

```csharp
public WorkflowResult EndWorkflow(string? name = null)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string?` | `null` | When given, must match the active workflow's (trimmed) name or the call is a no-op. Omit to end whichever workflow is active. |

Returns a `WorkflowResult`. Emits `$workflow_end` (with `duration_ms` in its `properties`,
computed from the workflow's start time) for the workflow **being closed** — i.e. it is
stamped with that workflow, not the cleared state — then clears it from the scope.

| `Status` | Meaning |
| --- | --- |
| `Ok` | Ended; `WorkflowId` is the one that was ended. |
| `NotActive` | No workflow is active on the current scope. |
| `NameMismatch` | `name` was given and does not match the active workflow's name — **including** when `name` is itself malformed (blank, or over 120 characters): a bad explicit name always reports `NameMismatch` here, never `InvalidName` (that status is reachable only from `StartWorkflow`). |
| `Disabled` | No initialized/enabled client, or an unexpected internal error. |

### `CancelWorkflow`

```csharp
public WorkflowResult CancelWorkflow(string? name = null, string? reason = null)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `string?` | `null` | Same matching rule as `EndWorkflow`. |
| `reason` | `string?` | `null` | Free-form cancellation reason. Blank/null defaults to `"user"`; otherwise trimmed and capped at 120 characters (not truncated silently past that — the cap is the wire limit, not a validation failure). |

Returns a `WorkflowResult` with the same status table as `EndWorkflow`. Emits
`$workflow_cancel` with `duration_ms` **and** `reason` in its `properties` (the `force:
true` supersede path in `StartWorkflow` reuses this internally with the literal reason
`"superseded"`). `reason` is never sent on `$workflow_end`.

```csharp
SauronSdk.CancelWorkflow(reason: "user backed out at payment step");
```

### `GetWorkflow`

```csharp
public ActiveWorkflow? GetWorkflow()
```

```csharp
public sealed record ActiveWorkflow(string WorkflowId, string Name, DateTimeOffset StartedAt);
```

No parameters. Returns the workflow currently bounding the active scope, or `null` if none.
Like the rest of the scope API it is a pure ambient read with no `Disabled` status of its
own, so it works before `Init` and after `Close`. It returns `null`:

- before any `StartWorkflow` (nothing has ever been started);
- after `EndWorkflow`/`CancelWorkflow` closes the active workflow;
- after `Close`/`Dispose`, **for a workflow left un-ended on the process-wide global
  scope** — teardown clears it, so a later `Init` starts clean rather than answering
  `AlreadyActive` from the previous run's leftovers. Consistent with "an abandoned workflow
  is the server's call to make," teardown clears the state but never emits a
  `$workflow_cancel` of its own.

It does **not** return `null` for a workflow started inside a `using (SauronSdk.PushScope())`
block you are still inside — that is precisely what per-request isolation means, and such a
workflow needs no teardown anyway, since the scope is discarded when the block exits.

**Abandonment (30 minutes).** A workflow that receives no further stamped event for 30
minutes reads as **abandoned** on the dashboard — this is derived server-side, purely from
the last stamped event's timestamp, when the workflow is displayed. There is nothing to
configure and no client-side timer: the SDK never expires a workflow locally, `GetWorkflow`
keeps returning it indefinitely until you call `EndWorkflow`/`CancelWorkflow`, and if an app
resumes activity and stamps another event under the same still-active workflow, it simply
reads as active again — "abandoned" is a display label, not a stored state transition.

**State flows with `AsyncLocal<T>`, not a static field.** The active workflow lives on the
same `AsyncLocal` `Scope` that `SetUser`/`SetTag`/`PushScope` already use (see "Scope &
metadata" below) — deliberately, since this is a **server** SDK: one
process handles many concurrent requests, and a plain static field would let one request's
`StartWorkflow` stamp another request's errors, or let one request's `EndWorkflow` be
swallowed by another's `AlreadyActive`. Concretely, this means:

- Always `StartWorkflow`/`EndWorkflow` inside the same `using (SauronSdk.PushScope())` block
  (or the same ambient call chain) that will observe it — exactly like the per-request
  scope middleware recipe below.
- It flows across `await` and into a `Task.Run` **started from inside** that block, because
  `Task.Run` captures the ambient `ExecutionContext` by default. A workflow started before
  `Task.Run` is still visible inside it.
- It does **not** flow *backward*: a workflow started inside a detached `Task.Run` (or any
  code that isn't awaited before the enclosing `PushScope` block exits) is invisible to the
  caller once that block has moved on — `AsyncLocal` only propagates parent-to-child.
- Fire-and-forget background work, a hosted/background service, a queued job processed on
  its own thread, or any code that hops execution contexts without capturing them (e.g.
  `ExecutionContext.SuppressFlow()`, or scheduling onto a bare `ThreadPool` work item) will
  **not** see a request's workflow at all. Give that code its own `PushScope()` and its own
  `StartWorkflow` call rather than assuming it inherits one from a caller that has already
  returned.

### `AddBreadcrumb`

```csharp
public void AddBreadcrumb(Breadcrumb breadcrumb)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `breadcrumb` | `Breadcrumb` | — | Required. `SauronClient.AddBreadcrumb` throws `ArgumentNullException` when `null`. |

Returns `void`. Runs `BeforeBreadcrumb` (drop on `null`, replace on non-null),
then appends to the active scope's ring buffer, dropping the oldest once the ring
exceeds `MaxBreadcrumbs`. Breadcrumbs ride along on errors captured afterwards
(`CaptureException` / `CaptureMessage`) — not on events, identifies or
transactions.

`SauronSdk.AddBreadcrumb` before `Init` is safe: it records onto the global scope
with a ring size of 100 and no `BeforeBreadcrumb` hook.

```csharp
SauronSdk.AddBreadcrumb(new Breadcrumb
{
    Type = "http",
    Category = "outbound",
    Message = "POST /charges",
    Level = "info",
    Data = new Dictionary<string, object?> { ["status"] = 402 },
});
```

### `SetUser`

```csharp
public static void SetUser(SauronUser? user)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `user` | `SauronUser?` | — | The user to attribute. Pass `null` to clear. |

Returns `void`. Sets the user on the **active** scope — the pushed scope when one
is live, otherwise the process-wide global scope.

```csharp
SauronSdk.SetUser(new SauronUser { Id = "user-123", Email = "a@b.co", Username = "ada" });
SauronSdk.SetUser(null); // sign-out
```

### `SetTag`

```csharp
public static void SetTag(string key, string value)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `string` | — | Tag key. Null/empty keys are ignored. |
| `value` | `string` | — | Tag value. Scope tags are string-valued (per-call `tags` arguments accept `object?`). |

Returns `void`.

```csharp
SauronSdk.SetTag("service", "checkout");
```

### `SetTags`

```csharp
public static void SetTags(IReadOnlyDictionary<string, string> tags)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `tags` | `IReadOnlyDictionary<string, string>` | — | Tags to set. A `null` dictionary is ignored; entries are applied one by one, overwriting existing keys. |

Returns `void`.

```csharp
SauronSdk.SetTags(new Dictionary<string, string>
{
    ["service"] = "checkout",
    ["region"] = "eu-west-1",
});
```

### `SetContext`

```csharp
public static void SetContext(string key, object? value)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `string` | — | Context block name. Null/empty is ignored. |
| `value` | `object?` | — | The block — any JSON-serializable value, typically a dictionary or POCO. |

Returns `void`. Context blocks are dev-owned and land in the item's `contexts`
map — distinct from the machine-generated envelope `context`.

```csharp
SauronSdk.SetContext("tenant", new Dictionary<string, object?>
{
    ["id"] = "acme",
    ["tier"] = "enterprise",
});
```

### `SetExtra`

```csharp
public static void SetExtra(string key, object? value)
```

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `key` | `string` | — | Extra key. Null/empty is ignored. |
| `value` | `object?` | — | Any JSON-serializable value. |

Returns `void`.

```csharp
SauronSdk.SetExtra("correlation_id", Activity.Current?.Id);
```

### `PushScope`

```csharp
public static IDisposable PushScope()
```

No parameters. Returns an `IDisposable` handle; disposing restores the previous
scope. The pushed scope is an isolated **clone** of the currently active scope
taken at push time, so:

- writes inside the block never reach the parent or global scope;
- later writes to the global scope are **not** seen by an already-pushed scope;
- scopes nest, and the handle is idempotent (double dispose is a no-op);
- the scope is `AsyncLocal`, so it flows across `await` and into `Task.Run`
  continuations started inside the block — and concurrent requests never see each
  other's scope.

```csharp
using (SauronSdk.PushScope())
{
    SauronSdk.SetUser(new SauronUser { Id = userId });
    SauronSdk.SetTag("route", "/checkout");
    SauronSdk.AddBreadcrumb(new Breadcrumb { Type = "navigation", Message = "entered checkout" });

    try { await CheckoutAsync(); }
    catch (Exception ex) { SauronSdk.CaptureException(ex); throw; }
} // scope restored — nothing leaks into the next request
```

### `FlushAsync`

```csharp
public Task FlushAsync()
```

No parameters. Returns a `Task` that completes once the buffered items have been
serialized into envelopes and the pending queue has been drained (or a transient
failure has left envelopes queued for later). `SauronSdk.FlushAsync()` returns
`Task.CompletedTask` before `Init`.

**Never throws.** Delivery problems — network errors, an unserializable property
value, even flushing after `Close` — are logged (with `Debug = true`) and the task
completes successfully; telemetry must not fail the app it is observing. Nothing is
treated as delivered unless it was: an envelope whose send failed stays queued and
is retried on the next flush.

```csharp
await SauronSdk.FlushAsync();
```

### `Flush`

```csharp
public void Flush()
```

No parameters. Returns `void`. Blocking wrapper over `FlushAsync`
(`GetAwaiter().GetResult()`); prefer `FlushAsync` on any async path.

```csharp
SauronSdk.Flush();
```

### `Close` and `Dispose`

```csharp
// SauronSdk
public static void Close()

// SauronClient
public void Close()   // == Dispose()
public void Dispose()
```

No parameters. Returns `void`. Unsubscribes the auto-capture handlers (so a late
crash cannot dispatch onto a torn-down client), stops the flush timer, performs a
**blocking best-effort final flush**, and disposes the `HttpClient` when the
client owns one (i.e. when `HttpMessageHandler` was supplied — the injected
handler itself is not disposed). `SauronSdk.Close()` also clears `Current`, so
subsequent facade calls are no-ops.

`SauronClient` implements `IDisposable` only — there is **no** `IAsyncDisposable`
/ `DisposeAsync`, so `await using` does not apply. On an async shutdown path,
`await FlushAsync()` first and then dispose:

```csharp
await client.FlushAsync();
client.Dispose();

// synchronous paths:
using var client = new SauronClient(options);   // disposed (and flushed) at scope exit
// or, with the facade:
AppDomain.CurrentDomain.ProcessExit += (_, _) => SauronSdk.Close();
```

### `Breadcrumb`

```csharp
public sealed class Breadcrumb
{
    public string Type { get; set; } = "default";
    public string? Category { get; set; }
    public string? Message { get; set; }
    public string? Level { get; set; }
    public DateTimeOffset Timestamp { get; set; } = DateTimeOffset.UtcNow;
    public Dictionary<string, object?>? Data { get; set; }
}
```

| Property | Type | Default | Description |
| --- | --- | --- | --- |
| `Type` | `string` | `"default"` | Kind, e.g. `navigation`, `http`, `log`. An empty value is written as `"default"`. |
| `Category` | `string?` | `null` | Grouping category, e.g. `auth`, `ui.click`. |
| `Message` | `string?` | `null` | Human-readable message. |
| `Level` | `string?` | `null` | `debug\|info\|warning\|error\|fatal`. |
| `Timestamp` | `DateTimeOffset` | `DateTimeOffset.UtcNow` at construction | Serialized round-trip (`"O"`). |
| `Data` | `Dictionary<string, object?>?` | `null` | Free-form payload. Serialized as `{}` when `null`. |

### `SauronUser`

```csharp
public sealed class SauronUser
{
    public string? Id { get; set; }
    public string? Email { get; set; }
    public string? Username { get; set; }
}
```

All three properties default to `null`. `Id` is what `TrackTransaction` falls
back to for `distinctId`.

### `Dsn`

```csharp
public sealed class Dsn
{
    public static Dsn Parse(string dsn);
    public string Protocol { get; }    // "http" | "https"
    public string PublicKey { get; }   // DSN user component
    public string Host { get; }        // includes a non-default port
    public string ProjectId { get; }   // DSN path segment — despite the name, this
                                        // is the environment id (the ingest key now
                                        // lives on the environment, not the app)
    public string Raw { get; }         // the original string
    public string EnvelopeUrl { get; } // {protocol}://{host}/api/{environment_id}/envelope
}
```

`Parse` throws `ArgumentException` for an empty string, a non-`http(s)` scheme, a
missing/empty public key, a missing host, or a missing environment id. A password
component, if present, is ignored. Handy for validating configuration at startup
instead of silently landing in no-op mode:

```csharp
try { Dsn.Parse(dsn); }
catch (ArgumentException ex) { throw new InvalidOperationException($"Bad SAURON_DSN: {ex.Message}"); }
```

### `BeforeSend`

```csharp
public Func<object, object?>? BeforeSend { get; set; }
```

Runs on every outgoing item — event, error, identify and transaction — in a
single chokepoint before buffering. Return the item to send it, a different
object to replace it, or `null` to drop it. A throwing hook drops the item and
logs when `Debug` is on.

The concrete item DTOs (`EventItem`, `ErrorItem`, `IdentifyItem`,
`TransactionItem`) are **internal** to the `Sauron` assembly, so consumer code
cannot pattern-match or cast them. From outside the assembly you can gate on the
runtime type name (or use reflection) and drop what you do not want:

```csharp
BeforeSend = item => item.GetType().Name switch
{
    "IdentifyItem" => null,   // never ship identify calls from this service
    _ => item,
};
```

If you need field-level redaction, do it at the call site (build the
`properties` / `extra` dictionaries already scrubbed) rather than in
`BeforeSend`.

## Scope & metadata

A scope carries a user, tags, context blocks, extra values and a bounded
breadcrumb ring. There is one process-wide **global** scope plus an `AsyncLocal`
stack of pushed scopes; `SauronSdk`'s scope methods always write to the innermost
active one.

Precedence, highest first:

1. **Per-call arguments** on `CaptureException` / `CaptureMessage` / `Track` —
   `tags` by key, `contexts` by block name, `extra` by key, `user` as a whole.
   A per-call value is never overwritten by a scope value.
2. **Active scope** — the innermost pushed scope, which was cloned from its
   parent at push time, so a child sees its parent's values and can shadow them
   locally without mutating the parent.
3. **Global scope** — where `SauronOptions.Tags` / `Contexts` / `Extra` are
   seeded at client construction. Later `SetTag` / `SetContext` / `SetExtra`
   calls made outside any pushed scope overwrite those seeds.

What each item type receives:

| Item | user | tags | contexts | extra | breadcrumbs |
| --- | --- | --- | --- | --- | --- |
| `error` (`CaptureException`, `CaptureMessage`) | yes (scope fills in when the call passed none) | yes | yes | yes | yes |
| `event` (`Track`) | no | yes | yes | yes | no |
| `transaction` (`TrackTransaction`) | id only, as the `distinctId` fallback | no | no | no | no |
| `identify` (`Identify`) | no | no | no | no | no |

Empty `tags` / `contexts` / `extra` are normalized to `null` and omitted from the
wire rather than sent as `{}`.

Note that `SauronOptions.Tags` seeds are strings, while per-call `tags` arguments
are `IReadOnlyDictionary<string, object?>`; both end up in the same `tags` map on
the wire.

## ASP.NET Core integration

The SDK has no ASP.NET Core dependency and ships no `IServiceCollection`
extension method — wire it up with the three pieces below. Both styles work; pick
one and stay consistent.

### 1. Registration

Facade style (simplest — no injection needed anywhere):

```csharp
using Sauron;

var builder = WebApplication.CreateBuilder(args);

SauronSdk.Init(new SauronOptions
{
    Dsn = builder.Configuration["Sauron:Dsn"]!,
    Release = typeof(Program).Assembly.GetName().Version?.ToString(),
    AutoCaptureUnhandled = true,
});
```

DI style (inject `SauronClient` into controllers and services). Register with a
factory so the container owns disposal:

```csharp
builder.Services.AddSingleton(sp => new SauronClient(new SauronOptions
{
    Dsn = sp.GetRequiredService<IConfiguration>()["Sauron:Dsn"]!,
}));
```

```csharp
public sealed class OrdersController : ControllerBase
{
    private readonly SauronClient _sauron;
    public OrdersController(SauronClient sauron) => _sauron = sauron;

    [HttpPost]
    public IActionResult Create(OrderDto dto)
    {
        _sauron.Track("order_created", User.FindFirst("sub")!.Value);
        return Ok();
    }
}
```

Remember the scope API is ambient: an injected client still reads the scope that
the middleware below pushed.

### 2. Per-request scope + timing middleware

```csharp
using System.Diagnostics;
using Sauron;

public sealed class SauronMiddleware
{
    private readonly RequestDelegate _next;
    public SauronMiddleware(RequestDelegate next) => _next = next;

    public async Task InvokeAsync(HttpContext context)
    {
        var sw = Stopwatch.StartNew();

        // AsyncLocal — the scope flows across the awaited _next(context).
        using (SauronSdk.PushScope())
        {
            SauronSdk.SetTag("http.method", context.Request.Method);

            var uid = context.User?.FindFirst("sub")?.Value;
            if (uid is not null)
                SauronSdk.SetUser(new SauronUser { Id = uid });

            SauronSdk.AddBreadcrumb(new Breadcrumb
            {
                Type = "http",
                Category = "request",
                Message = $"{context.Request.Method} {context.Request.Path}",
            });

            try
            {
                await _next(context);
            }
            finally
            {
                sw.Stop();
                SauronSdk.TrackTransaction(
                    name: $"{context.Request.Method} {context.Request.Path}",
                    durationMs: sw.Elapsed.TotalMilliseconds,
                    op: "http",
                    httpMethod: context.Request.Method,
                    httpStatus: context.Response.StatusCode,
                    url: context.Request.Path);
            }
        }
    }
}
```

```csharp
app.UseMiddleware<SauronMiddleware>();   // register early, before endpoint routing
```

### 3. Exception handling

Register the handler **inside** the scope middleware's `using` block so captures
carry the request's user, tags and breadcrumbs. On .NET 8 an `IExceptionHandler`
works, but it runs after the scope middleware only if you place
`UseExceptionHandler` after `UseMiddleware<SauronMiddleware>()`:

```csharp
public sealed class SauronExceptionHandler : IExceptionHandler
{
    public ValueTask<bool> TryHandleAsync(
        HttpContext context, Exception exception, CancellationToken ct)
    {
        SauronSdk.CaptureException(exception, tags: new Dictionary<string, object?>
        {
            ["http.method"] = context.Request.Method,
            ["http.path"] = context.Request.Path.Value,
        });
        return ValueTask.FromResult(false); // let the default handler produce the response
    }
}
```

```csharp
builder.Services.AddExceptionHandler<SauronExceptionHandler>();
builder.Services.AddProblemDetails();
// ...
app.UseMiddleware<SauronMiddleware>();
app.UseExceptionHandler();
```

Or catch inline in the middleware and rethrow:

```csharp
try { await _next(context); }
catch (Exception ex) { SauronSdk.CaptureException(ex); throw; }
```

Do not rely on `AutoCaptureUnhandled` for request errors — ASP.NET Core handles
them, so `AppDomain.UnhandledException` never fires. It is there for background
threads and genuine process crashes.

### 4. Graceful shutdown

The buffer is in memory. Drain it on `ApplicationStopping`, otherwise the last
few seconds of events die with the process:

```csharp
var app = builder.Build();

app.Lifetime.ApplicationStopping.Register(SauronSdk.Close);   // facade style
```

With DI, the container disposes the singleton it created at host shutdown, and
`SauronClient.Dispose` performs the final flush. For a deterministic,
async-friendly drain, add a hosted service:

```csharp
public sealed class SauronDrain : IHostedService
{
    private readonly SauronClient _sauron;
    public SauronDrain(SauronClient sauron) => _sauron = sauron;

    public Task StartAsync(CancellationToken ct) => Task.CompletedTask;
    public Task StopAsync(CancellationToken ct) => _sauron.FlushAsync();
}
```

```csharp
builder.Services.AddHostedService<SauronDrain>();
```

## Transport & delivery

- **Batching.** Items are buffered in memory and turned into envelopes on a
  flush. A flush happens on the `FlushInterval` timer (default 5s; disabled when
  `FlushInterval <= TimeSpan.Zero`), as soon as `MaxBatch` items are buffered
  (default 30), on an explicit `Flush`/`FlushAsync`, and on `Close`/`Dispose`.
- **Envelope size.** One flush produces one envelope per `MaxItemsPerEnvelope`
  items (default 1000, matching the server ceiling). A backlog larger than the
  cap is split rather than rejected wholesale.
- **Gzip.** Bodies strictly larger than `GzipThresholdBytes` (default 1024) are
  gzipped and sent with `Content-Encoding: gzip`. Smaller bodies are sent as-is.
- **Pending queue.** Serialized envelopes go into a byte-capped FIFO queue
  (`MaxQueueBytes`, default 1 MiB). Over the cap the **oldest** envelopes are
  dropped. Draining is FIFO; a transiently-failed envelope stops the drain so
  ordering survives the outage.
- **Offline persistence.** Setting `OfflineDir` writes each pending envelope to
  its own file (zero-padded sequence name, `.env`), reloads them at construction,
  and deletes each on delivery — at-least-once across restarts. Off by default.
- **Retry policy.** Up to **3 attempts** per envelope. Backoff is exponential
  with full jitter, `100ms * 2^(attempt-1)` plus jitter, capped at **30s**. On
  `429` a `Retry-After` header (delta-seconds or HTTP-date) is honored, clamped
  to `[0, 30s]`.

| Status | Behavior |
| --- | --- |
| 2xx | Delivered; envelope acked and removed. |
| 408, 429, 5xx | Retried (up to 3 attempts), then kept in the queue for the next flush. |
| Network / transport exception | Same as above — retried, then kept. |
| 401, 403 | Dropped **and the client is disabled permanently** (`Enabled` becomes `false`). A bad key is never retried. |
| 413 | Dropped without retry — the same bytes would fail identically and would head-of-line block the FIFO queue. |
| 400, 404, other 4xx | Dropped with a log line. |

The envelope `context` block carries a per-client random `device_id`, the
detected OS (`linux`/`windows`/`macos`/`unknown`) and `runtime = dotnet` with
`Environment.Version`.

`HttpClient` ownership: with no `HttpMessageHandler` option the SDK uses one
static, process-wide `HttpClient` that it never disposes. With a handler
supplied, it wraps it in a dedicated `HttpClient` (`disposeHandler: false`) and
disposes that client on `Dispose` — your handler stays yours.

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| Nothing arrives, no errors | Ingest is not exposed at `/api/{environment_id}/envelope` on the host root. A DSN cannot express a path prefix, so a proxy that serves Sauron under e.g. `/sauron/` silently swallows envelopes. | Expose the ingest at `/api/{environment_id}/envelope` on the DSN host root. |
| Nothing arrives, `Enabled == false` at startup | DSN missing or unparseable — the client is in no-op mode. | Set `Debug = true` to see `[sauron] disabled: ...`, or validate with `Dsn.Parse(dsn)` at startup. |
| Worked, then stopped; `Enabled == false` | Ingest answered `401`/`403`; the client disabled itself. | Check the public key and the environment id in the DSN. |
| Events lost at process exit | The buffer is in memory and the flush timer never fired. | Call `SauronSdk.Close()` / `await client.FlushAsync()` on shutdown (see the ASP.NET Core recipe). |
| `ArgumentException: distinctId is required` | `Track` / `Identify` were called without a distinct id — the wire contract has no anonymous fallback. | Pass a stable user id. |
| Breadcrumbs missing from an error | They are attached from the **active** scope; a scope pushed after the crumbs, or crumbs added after the capture, will not appear. Also check `MaxBreadcrumbs` and `BeforeBreadcrumb`. | Add crumbs inside the same `PushScope` block that captures. |
| Tags set in one request show up in another | Set outside a pushed scope, so they landed on the process-wide global scope. | Wrap request handling in `using (SauronSdk.PushScope())`. |
| `item is EventItem` does not compile | The item DTOs are internal to the assembly. | Match on `item.GetType().Name` in `BeforeSend`, or redact at the call site. |
| Nothing in logs at all | `Debug` is off — the SDK is silent by design. | Set `Debug = true`; diagnostics go to `stderr` prefixed `[sauron]`. |

## Development

```bash
cd sdks/csharp

dotnet restore
dotnet build                 # builds Sauron + Sauron.Tests (Sauron.slnx)
dotnet test                  # xUnit suite
dotnet pack Sauron/Sauron.csproj -c Release -o ./nupkg
```

There is no separate typecheck step — `Nullable` is enabled, so `dotnet build`
surfaces nullability warnings. The test project has `InternalsVisibleTo` access,
which is why tests can reference the internal item DTOs directly.

### Why there is a `global.json` here

`global.json` sets an SDK **floor of 9.0.200**, and that floor is about the
solution format, not the target framework. This directory ships only
`Sauron.slnx` — the XML solution format — and support for it landed in SDK
9.0.200. On an older SDK the commands above fail with
`MSB1003: Specify a project or solution file`, which reads like a missing file
rather than an SDK that cannot parse the one that is there.

`"rollForward": "latestMajor"` keeps it a floor: a newer SDK (10.x and up) is
fine and is what most machines will use. The floor is deliberately *not* an
exact pin — `net8.0` in the two `.csproj` files already pins what gets compiled,
and `RollForward: Major` pins what the test host may run on. If you do have an
SDK below the floor, the error names both the version wanted and this file.

## License

LGPL-3.0-only — GNU Lesser General Public License v3.0. See
[LICENSE](../../LICENSE); LGPLv3 applies on top of the GNU GPL v3 in
[COPYING](../../COPYING).

Repo: <https://github.com/edraj/sauron> · Wiki:
<https://github.com/edraj/sauron/wiki>
