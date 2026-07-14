# C# SDK — `Sauron`

Server-side .NET SDK (`net8.0`, namespace `Sauron`). Dispatches product-analytics
events and captured exceptions over a buffered background HTTP transport (`HttpClient`
+ a timer flush), with JSON via `System.Text.Json`. **No auto-instrumentation** — a
plain server-side dispatch API. Source: [`sdks/csharp`](../sdks/csharp). SDK header
name: `sauron-dotnet`.

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

## API

| Method | Signature |
| --- | --- |
| `Init` | `Init(string dsn)` / `Init(SauronOptions options)` |
| `Track` | `Track(string @event, string distinctId, IReadOnlyDictionary<string, object?>? properties = null)` |
| `CaptureException` | `CaptureException(Exception exception, SauronUser? user = null, string level = "error", IReadOnlyDictionary<string, object?>? tags = null)` |
| `CaptureMessage` | `CaptureMessage(string message, string level = "info")` |
| `Identify` | `Identify(string distinctId, IReadOnlyDictionary<string, object?>? traits = null)` |
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
        tags: new Dictionary<string, object?> { ["component"] = "checkout" });
}

SauronSdk.CaptureMessage("nightly job finished", "info");
```

### Identify a user

```csharp
SauronSdk.Identify("user-42", new Dictionary<string, object?> { ["plan"] = "pro" });
```

### Flush / close

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
