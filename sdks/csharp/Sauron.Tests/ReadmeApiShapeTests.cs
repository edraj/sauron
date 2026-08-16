using System.Collections.Generic;
using Xunit;

namespace Sauron.Tests;

/// <summary>
/// Compile-checks the exact API shapes the README documents.
/// </summary>
/// <remarks>
/// Not a behaviour test — it exists because a README example is the one piece of this
/// SDK that nothing else compiles. Signatures drift, the docs keep telling people to
/// write code that no longer builds, and no suite notices.
/// </remarks>
public class ReadmeApiShapeTests
{
    [Fact]
    public void TrackTransactionAcceptsTheDocumentedShape()
    {
        // No Init(): the static facade is a no-op without a current client, which is
        // exactly what makes this a pure signature check.
        SauronSdk.TrackTransaction(
            name: "POST /orders",
            durationMs: 842.5,
            op: "http",
            status: "ok",
            httpMethod: "POST",
            httpStatus: 201,
            url: "/orders",
            distinctId: "user-123",
            tags: new Dictionary<string, object?> { ["route"] = "/orders", ["tier"] = "premium" },
            extra: new Dictionary<string, object?>
            {
                ["request"] = "{}",
                ["response"] = "{}",
                ["query"] = "?page=1",
                ["request_headers"] = new[] { "content-type" },
            });

        SauronSdk.TrackTransaction(
            name: "SELECT orders",
            durationMs: 12,
            op: "db",
            status: "ok",
            tags: new Dictionary<string, object?> { ["db"] = "postgres", ["table"] = "orders" },
            extra: new Dictionary<string, object?>
            {
                ["statement"] = "SELECT 1",
                ["row_count"] = 20,
                ["params"] = new object?[] { "u_1" },
            });
    }

    /// <summary>The README cites this constant, so it has to be reachable from outside.</summary>
    [Fact]
    public void DocumentedCapConstantIsPublic()
    {
        Assert.Equal(16 * 1024, TransactionExtra.MaxBytes);
    }
}
