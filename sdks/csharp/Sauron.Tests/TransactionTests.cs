using System.Text.Json;
using Xunit;

namespace Sauron.Tests;

/// <summary>C4 — trackTransaction emits a snake_case transaction item; op defaults to custom;
/// distinct_id falls back to the scoped user id.</summary>
[Collection("SauronScope")]
public class TransactionTests
{
    public TransactionTests() => ScopeManager.ResetForTests();

    [Fact]
    public void TrackTransaction_EmitsTransactionItem_SnakeCaseShape()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.TrackTransaction(
            "GET /api/users", 12.5,
            op: "http", status: "ok", httpMethod: "GET", httpStatus: 200,
            url: "/api/users", distinctId: "u1");
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("transaction", item.GetProperty("type").GetString());
        Assert.Equal("GET /api/users", item.GetProperty("name").GetString());
        Assert.Equal("http", item.GetProperty("op").GetString());
        Assert.Equal(12.5, item.GetProperty("duration_ms").GetDouble());
        Assert.Equal("ok", item.GetProperty("status").GetString());
        Assert.Equal("GET", item.GetProperty("http_method").GetString());
        Assert.Equal(200, item.GetProperty("http_status").GetInt32());
        Assert.Equal("/api/users", item.GetProperty("url").GetString());
        Assert.Equal("u1", item.GetProperty("distinct_id").GetString());
        Assert.False(string.IsNullOrEmpty(item.GetProperty("timestamp").GetString()));
    }

    [Fact]
    public void TrackTransaction_DefaultsOpToCustom()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.TrackTransaction("background-job", 5.0);
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("custom", item.GetProperty("op").GetString());
    }

    [Fact]
    public void TrackTransaction_DistinctId_FallsBackToScopedUser()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        ScopeManager.Current.SetUser(new SauronUser { Id = "scoped-user" });
        client.TrackTransaction("work", 5.0);
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("scoped-user", item.GetProperty("distinct_id").GetString());
    }

    [Fact]
    public void TrackTransaction_NoDistinctId_NoScopedUser_SerializesNull()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.TrackTransaction("work", 5.0);
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal(JsonValueKind.Null, item.GetProperty("distinct_id").ValueKind);
    }

    [Fact]
    public void TrackTransaction_BeforeInit_IsNoOp()
    {
        SauronSdk.Close();
        SauronSdk.TrackTransaction("op", 1.0); // must not throw
    }
}
