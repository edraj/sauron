using Sauron;

// Minimal server-side Sauron example.
//
// Reads its ingest DSN from the SAURON_DSN environment variable. If the variable
// is unset (or the DSN is invalid) the SDK runs in no-op mode, so this program
// stays runnable even without a live ingest gateway.
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
    Release = "csharp-server-example@0.1.0",
    Debug = true,
});

const string distinctId = "user-42";

// 2. Identify the acting user with a few traits.
SauronSdk.Identify(distinctId, new Dictionary<string, object?>
{
    ["email"] = "ada@example.com",
    ["name"] = "Ada Lovelace",
    ["plan"] = "pro",
});

// 3. Track a product-analytics event.
SauronSdk.Track("order_placed", distinctId, new Dictionary<string, object?>
{
    ["order_id"] = "ord_1001",
    ["amount"] = 49.99,
    ["currency"] = "USD",
    ["items"] = 3,
});

// 4. Capture an exception from a failing operation.
try
{
    ProcessOrder("ord_1001");
}
catch (Exception ex)
{
    SauronSdk.CaptureException(
        ex,
        user: new SauronUser { Id = distinctId, Email = "ada@example.com", Username = "ada" },
        level: "error",
        tags: new Dictionary<string, object?>
        {
            ["order_id"] = "ord_1001",
            ["component"] = "checkout",
        });
}

// 5. Flush buffered items and shut the client down cleanly before exit.
SauronSdk.Flush();
SauronSdk.Close();

Console.WriteLine("Done. Sent identify, track, and one captured exception (if a DSN was configured).");

// A stand-in operation that fails so we have something to capture.
static void ProcessOrder(string orderId)
    => throw new InvalidOperationException($"payment gateway declined order {orderId}");
