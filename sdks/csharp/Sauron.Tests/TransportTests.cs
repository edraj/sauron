using System;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Text.Json;
using System.Threading.Tasks;
using Xunit;

namespace Sauron.Tests;

public class TransportTests
{
    private static SauronClient NewClient(CapturingHandler handler, string dsn = "https://pub123@example.com/42")
        => new(new SauronOptions
        {
            Dsn = dsn,
            HttpMessageHandler = handler,
            FlushInterval = TimeSpan.FromHours(1), // keep the background timer out of the way
            MaxBatch = 1000,
        });

    private static JsonElement FirstItem(string body)
    {
        using var doc = JsonDocument.Parse(body);
        var root = doc.RootElement;
        // Re-parse to detach the element from the disposed document.
        var items = root.GetProperty("items");
        return JsonDocument.Parse(items[0].GetRawText()).RootElement;
    }

    [Fact]
    public void Flush_PostsToEnvelopeUrl_WithKeyHeader()
    {
        var handler = new CapturingHandler();
        using var client = NewClient(handler);

        client.Track("signup", "user-1");
        client.Flush();

        Assert.Equal(1, handler.RequestCount);
        Assert.Equal(HttpMethod.Post, handler.LastRequest!.Method);
        Assert.Equal("https://example.com/api/42/envelope", handler.LastRequest.RequestUri!.ToString());

        Assert.True(handler.LastRequest.Headers.TryGetValues("X-Sauron-Key", out var keys));
        Assert.Equal("pub123", keys!.Single());

        Assert.Equal("application/json", handler.LastRequest.Content!.Headers.ContentType!.MediaType);
    }

    [Fact]
    public void Envelope_HasRequiredSdkHeaderAndContext()
    {
        var handler = new CapturingHandler();
        using var client = NewClient(handler);

        client.Track("evt", "u");
        client.Flush();

        using var doc = JsonDocument.Parse(handler.LastBody!);
        var root = doc.RootElement;

        var sdk = root.GetProperty("header").GetProperty("sdk");
        Assert.Equal("sauron-dotnet", sdk.GetProperty("name").GetString());
        Assert.Equal("0.1.0", sdk.GetProperty("version").GetString());

        Assert.Equal("production", root.GetProperty("header").GetProperty("environment").GetString());
        Assert.False(string.IsNullOrEmpty(root.GetProperty("header").GetProperty("sent_at").GetString()));

        var runtime = root.GetProperty("context").GetProperty("runtime");
        Assert.Equal("dotnet", runtime.GetProperty("name").GetString());
        Assert.False(string.IsNullOrEmpty(root.GetProperty("context").GetProperty("device").GetProperty("device_id").GetString()));
    }

    [Fact]
    public void Track_ProducesEventItem_SnakeCaseShape()
    {
        var handler = new CapturingHandler();
        using var client = NewClient(handler);

        client.Track("signup", "user-1", new Dictionary<string, object?> { ["plan"] = "pro" });
        client.Flush();

        var item = FirstItem(handler.LastBody!);
        Assert.Equal("event", item.GetProperty("type").GetString());
        Assert.Equal("signup", item.GetProperty("name").GetString());
        Assert.Equal("user-1", item.GetProperty("distinct_id").GetString());
        Assert.Equal("pro", item.GetProperty("properties").GetProperty("plan").GetString());
        Assert.False(string.IsNullOrEmpty(item.GetProperty("timestamp").GetString()));
        Assert.Equal(JsonValueKind.Null, item.GetProperty("session_id").ValueKind);
        Assert.Equal(JsonValueKind.Null, item.GetProperty("screen").ValueKind);
    }

    [Fact]
    public void CaptureException_ProducesErrorItem_WithExceptionAndStacktrace()
    {
        var handler = new CapturingHandler();
        using var client = NewClient(handler);

        try
        {
            throw new InvalidOperationException("kaboom");
        }
        catch (Exception ex)
        {
            client.CaptureException(ex, new SauronUser { Id = "u9", Email = "u@x.io" }, tags: new Dictionary<string, object?> { ["area"] = "billing" });
        }
        client.Flush();

        var item = FirstItem(handler.LastBody!);
        Assert.Equal("error", item.GetProperty("type").GetString());
        Assert.Equal("error", item.GetProperty("level").GetString());
        Assert.False(string.IsNullOrEmpty(item.GetProperty("event_id").GetString()));

        var exc = item.GetProperty("exception");
        Assert.Equal("System.InvalidOperationException", exc.GetProperty("type").GetString());
        Assert.Equal("kaboom", exc.GetProperty("value").GetString());
        Assert.Equal("generic", exc.GetProperty("mechanism").GetProperty("type").GetString());
        Assert.True(exc.GetProperty("mechanism").GetProperty("handled").GetBoolean());
        Assert.True(exc.GetProperty("stacktrace").GetArrayLength() >= 1);

        Assert.Equal("u9", item.GetProperty("user").GetProperty("id").GetString());
        Assert.Equal("billing", item.GetProperty("tags").GetProperty("area").GetString());
    }

    [Fact]
    public void CaptureMessage_ProducesErrorItem_WithMessage_NoException()
    {
        var handler = new CapturingHandler();
        using var client = NewClient(handler);

        client.CaptureMessage("hello world");
        client.Flush();

        var item = FirstItem(handler.LastBody!);
        Assert.Equal("error", item.GetProperty("type").GetString());
        Assert.Equal("info", item.GetProperty("level").GetString());
        Assert.Equal("hello world", item.GetProperty("message").GetString());
        Assert.Equal(JsonValueKind.Null, item.GetProperty("exception").ValueKind);
    }

    [Fact]
    public void Identify_ProducesIdentifyItem()
    {
        var handler = new CapturingHandler();
        using var client = NewClient(handler);

        client.Identify("user-7", new Dictionary<string, object?> { ["email"] = "a@b.c" });
        client.Flush();

        var item = FirstItem(handler.LastBody!);
        Assert.Equal("identify", item.GetProperty("type").GetString());
        Assert.Equal("user-7", item.GetProperty("distinct_id").GetString());
        Assert.Equal("a@b.c", item.GetProperty("traits").GetProperty("email").GetString());
        Assert.Equal(JsonValueKind.Null, item.GetProperty("anonymous_id").ValueKind);
    }

    [Fact]
    public void Flush_BatchesMultipleItems_IntoOneEnvelope()
    {
        var handler = new CapturingHandler();
        using var client = NewClient(handler);

        client.Track("a", "u1");
        client.Track("b", "u2");
        client.Identify("u3");
        client.Flush();

        Assert.Equal(1, handler.RequestCount);
        using var doc = JsonDocument.Parse(handler.LastBody!);
        Assert.Equal(3, doc.RootElement.GetProperty("items").GetArrayLength());
    }

    [Fact]
    public async Task MaxBatch_TriggersAutomaticFlush()
    {
        var handler = new CapturingHandler();
        using var client = new SauronClient(new SauronOptions
        {
            Dsn = "https://pub123@example.com/42",
            HttpMessageHandler = handler,
            FlushInterval = TimeSpan.FromHours(1),
            MaxBatch = 2,
        });

        client.Track("a", "u1");
        client.Track("b", "u2"); // reaching MaxBatch flushes
        await client.FlushAsync();

        Assert.True(handler.RequestCount >= 1);
    }

    [Fact]
    public void EmptyBuffer_Flush_SendsNothing()
    {
        var handler = new CapturingHandler();
        using var client = NewClient(handler);

        client.Flush();

        Assert.Equal(0, handler.RequestCount);
    }

    [Fact]
    public void InvalidDsn_ClientDisabled_NoNetwork()
    {
        var handler = new CapturingHandler();
        using var client = new SauronClient(new SauronOptions
        {
            Dsn = "not-a-valid-dsn",
            HttpMessageHandler = handler,
        });

        Assert.False(client.Enabled);
        client.Track("evt", "u1");
        client.Flush();
        Assert.Equal(0, handler.RequestCount);
    }

    [Fact]
    public void AuthFailure_DisablesClient()
    {
        var handler = new CapturingHandler { ResponseStatus = HttpStatusCode.Unauthorized };
        using var client = NewClient(handler);

        client.Track("evt", "u1");
        client.Flush();

        Assert.False(client.Enabled);
    }
}
