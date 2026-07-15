using System.Diagnostics;
using Sauron;

// Minimal server-side Sauron example (SDK v0.3.0).
//
// Reads its ingest DSN from the SAURON_DSN environment variable. If the variable
// is unset (or the DSN is invalid) the SDK runs in no-op mode, so this program
// stays runnable — and exits 0 — even without a live ingest gateway.
//
//   export SAURON_DSN="https://<public_key>@<host>/<project_id>"
//   dotnet run

var dsn = Environment.GetEnvironmentVariable("SAURON_DSN");

if (string.IsNullOrWhiteSpace(dsn))
{
    Console.Error.WriteLine(
        "SAURON_DSN is not set — the SDK will run in no-op mode. " +
        "Set it to dispatch to a live ingest gateway, e.g.:\n" +
        "  export SAURON_DSN=\"https://<public_key>@<host>/<project_id>\"");
}

// 1. Initialize the process-wide client once at startup.
SauronSdk.Init(new SauronOptions
{
    Dsn = dsn ?? string.Empty,
    Environment = "development",
    Release = "csharp-server-example@0.3.0",
    Debug = true,
});

// Process-wide defaults live on the global scope — every captured error inherits them.
SauronSdk.SetTag("service", "checkout");

const string distinctId = "user-42";

// 2. Identify the acting user with a few traits.
SauronSdk.Identify(distinctId, new Dictionary<string, object?>
{
    ["email"] = "ada@example.com",
    ["name"] = "Ada Lovelace",
    ["plan"] = "pro",
});

// 3. Handle one request inside an isolated per-request scope. `using PushScope()`
//    layers a clone that is torn down at the end of the block, so the user and
//    tag set here ride along on anything captured inside — and never leak out.
using (SauronSdk.PushScope())
{
    SauronSdk.SetUser(new SauronUser { Id = distinctId, Email = "ada@example.com", Username = "ada" });
    SauronSdk.SetTag("order_id", "ord_1001");

    // Track a product-analytics event.
    SauronSdk.Track("order_placed", distinctId, new Dictionary<string, object?>
    {
        ["order_id"] = "ord_1001",
        ["amount"] = 49.99,
        ["currency"] = "USD",
    });

    // Leave a breadcrumb, then deliberately capture an exception. The breadcrumb,
    // the scoped user and both tags (global `service` + scoped `order_id`) attach
    // to the captured error automatically.
    SauronSdk.AddBreadcrumb(new Breadcrumb
    {
        Category = "checkout",
        Message = "charging payment gateway",
        Level = "info",
    });

    var sw = Stopwatch.StartNew();
    try
    {
        ProcessOrder("ord_1001");
    }
    catch (Exception ex)
    {
        SauronSdk.CaptureException(ex, level: "error");
    }
    sw.Stop();

    // 4. Time the request with one transaction. `distinctId` is omitted on purpose:
    //    it falls back to the scoped user id set above.
    SauronSdk.TrackTransaction(
        name: "POST /orders",
        durationMs: sw.Elapsed.TotalMilliseconds,
        op: "http.server",
        status: "internal_error",
        httpMethod: "POST",
        httpStatus: 500,
        url: "/orders");
}

// 5. Flush buffered items and shut the client down cleanly before exit.
SauronSdk.Flush();
SauronSdk.Close();

Console.WriteLine(
    "Done. Sent identify, a tracked event, a scoped exception (with breadcrumb + user + tags) " +
    "and one transaction (if a DSN was configured).");

// A stand-in operation that fails so we have something to capture.
static void ProcessOrder(string orderId)
    => throw new InvalidOperationException($"payment gateway declined order {orderId}");
