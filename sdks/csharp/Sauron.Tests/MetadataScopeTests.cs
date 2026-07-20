using System;
using System.Collections.Generic;
using System.Text.Json;
using Xunit;

namespace Sauron.Tests;

/// <summary>Metadata-scope feature: init defaults, per-call overrides, and analytics parity.</summary>
[Collection("SauronScope")]
public class MetadataScopeTests
{
    public MetadataScopeTests() => ScopeManager.ResetForTests();

    [Fact]
    public void InitDefaults_SeedGlobalScope_AndApplyToCapturedError()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions
        {
            Tags = new Dictionary<string, string> { ["env"] = "prod" },
            Contexts = new Dictionary<string, object?> { ["order"] = new Dictionary<string, object?> { ["id"] = 7 } },
            Extra = new Dictionary<string, object?> { ["build"] = "123" },
        });

        client.CaptureMessage("hi");
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("prod", item.GetProperty("tags").GetProperty("env").GetString());
        Assert.Equal(7, item.GetProperty("contexts").GetProperty("order").GetProperty("id").GetInt32());
        Assert.Equal("123", item.GetProperty("extra").GetProperty("build").GetString());
    }

    [Fact]
    public void CapturedError_MergesPerCallOverScope_ContextsExtra()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        ScopeManager.Current.SetContext("order", new Dictionary<string, object?> { ["id"] = 1 });
        ScopeManager.Current.SetExtra("build", "scope");

        try { throw new InvalidOperationException("x"); }
        catch (Exception ex)
        {
            client.CaptureException(ex,
                contexts: new Dictionary<string, object?> { ["order"] = new Dictionary<string, object?> { ["id"] = 99 } },
                extra: new Dictionary<string, object?> { ["req"] = "call" });
        }
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal(99, item.GetProperty("contexts").GetProperty("order").GetProperty("id").GetInt32()); // block name wins
        Assert.Equal("call", item.GetProperty("extra").GetProperty("req").GetString());                   // per-call key
        Assert.Equal("scope", item.GetProperty("extra").GetProperty("build").GetString());                // scope key kept
    }

    [Fact]
    public void CaptureMessage_CarriesPerCallMetadata()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.CaptureMessage("hi",
            tags: new Dictionary<string, object?> { ["k"] = "v" },
            contexts: new Dictionary<string, object?> { ["c"] = new Dictionary<string, object?> { ["n"] = 1 } },
            extra: new Dictionary<string, object?> { ["e"] = "x" });
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("v", item.GetProperty("tags").GetProperty("k").GetString());
        Assert.Equal(1, item.GetProperty("contexts").GetProperty("c").GetProperty("n").GetInt32());
        Assert.Equal("x", item.GetProperty("extra").GetProperty("e").GetString());
    }

    [Fact]
    public void TrackedEvent_CarriesScopeAndPerCallMetadata()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        ScopeManager.Current.SetTag("env", "prod");
        client.Track("checkout", "u_1",
            tags: new Dictionary<string, object?> { ["plan"] = "pro" },
            contexts: new Dictionary<string, object?> { ["cart"] = new Dictionary<string, object?> { ["n"] = 3 } },
            extra: new Dictionary<string, object?> { ["ab"] = "v2" });
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("event", item.GetProperty("type").GetString());
        Assert.Equal("prod", item.GetProperty("tags").GetProperty("env").GetString());  // scope
        Assert.Equal("pro", item.GetProperty("tags").GetProperty("plan").GetString());  // per-call
        Assert.Equal(3, item.GetProperty("contexts").GetProperty("cart").GetProperty("n").GetInt32());
        Assert.Equal("v2", item.GetProperty("extra").GetProperty("ab").GetString());
    }

    [Fact]
    public void TrackedEvent_OmitsMetadata_WhenNoneSet()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.Track("plain", "u_1");
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.False(item.TryGetProperty("tags", out _));
        Assert.False(item.TryGetProperty("contexts", out _));
        Assert.False(item.TryGetProperty("extra", out _));
    }

    [Fact]
    public void Facade_ForwardsPerCallMetadata_ThroughInitializedClient()
    {
        var handler = new CapturingHandler();
        SauronSdk.Init(new SauronOptions
        {
            Dsn = "https://pub123@example.com/42",
            HttpMessageHandler = handler,
            FlushInterval = TimeSpan.FromHours(1),
            MaxBatch = 1000,
        });
        try
        {
            SauronSdk.CaptureMessage("hi",
                contexts: new Dictionary<string, object?> { ["order"] = new Dictionary<string, object?> { ["id"] = 7 } },
                extra: new Dictionary<string, object?> { ["e"] = "x" });
            SauronSdk.Flush();

            var item = TestUtil.FirstItem(handler.LastBody!);
            Assert.Equal(7, item.GetProperty("contexts").GetProperty("order").GetProperty("id").GetInt32());
            Assert.Equal("x", item.GetProperty("extra").GetProperty("e").GetString());
        }
        finally
        {
            SauronSdk.Close();
        }
    }
}
