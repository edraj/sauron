using System.Collections.Generic;
using System.Text.Json;
using Xunit;

namespace Sauron.Tests;

/// <summary>C3 — before-send runs on every outgoing item; null drops, a returned object replaces.</summary>
[Collection("SauronScope")]
public class BeforeSendTests
{
    public BeforeSendTests() => ScopeManager.ResetForTests();

    [Fact]
    public void BeforeSend_CanRedactEventProperty()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions
        {
            BeforeSend = item =>
            {
                if (item is EventItem ev && ev.Properties.ContainsKey("email"))
                    ev.Properties["email"] = "[redacted]";
                return item;
            },
        });

        client.Track("signup", "u1", new Dictionary<string, object?> { ["email"] = "a@b.c" });
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("[redacted]", item.GetProperty("properties").GetProperty("email").GetString());
    }

    [Fact]
    public void BeforeSend_ReturningNull_DropsError()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions
        {
            BeforeSend = item => item is ErrorItem ? null : item,
        });

        client.CaptureMessage("boom");
        client.Flush();

        // The only item was dropped, so nothing is buffered and nothing is sent.
        Assert.Equal(0, handler.RequestCount);
    }

    [Fact]
    public void BeforeSend_RunsOnEveryItemType()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions
        {
            // Drop identify + transaction, keep event + error.
            BeforeSend = item => (item is IdentifyItem || item is TransactionItem) ? null : item,
        });

        client.Track("e", "u1");
        client.Identify("u1");
        client.CaptureMessage("boom");
        client.TrackTransaction("op", 3.0);
        client.Flush();

        using var doc = JsonDocument.Parse(handler.LastBody!);
        var items = doc.RootElement.GetProperty("items");
        Assert.Equal(2, items.GetArrayLength());
        var types = new HashSet<string?>
        {
            items[0].GetProperty("type").GetString(),
            items[1].GetProperty("type").GetString(),
        };
        Assert.Contains("event", types);
        Assert.Contains("error", types);
        Assert.DoesNotContain("identify", types);
        Assert.DoesNotContain("transaction", types);
    }

    [Fact]
    public void BeforeSend_CanReplaceItem()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions
        {
            BeforeSend = _ => new EventItem { Name = "replaced", DistinctId = "sys", Timestamp = "2026-01-01T00:00:00Z" },
        });

        client.Track("original", "u1");
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("replaced", item.GetProperty("name").GetString());
    }
}
