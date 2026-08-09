using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using Xunit;

namespace Sauron.Tests;

/// <summary>
/// Captures the envelope this SDK <b>actually posts</b> into
/// <c>sdks/wire-fixtures/csharp.json</c>, where the backend's
/// <c>cargo test -p sauron-core --test sdk_wire_conformance</c> feeds it through the real
/// <c>serde</c> deserializer.
/// </summary>
/// <remarks>
/// <see cref="EnvelopeGoldenTests"/> compares against a literal authored in this repo,
/// which is how the <c>js</c> SDK shipped <c>exception.type: null</c> — wire-invalid
/// against a non-<c>Option</c> <c>String</c> — while passing every test on both sides.
/// The envelope is all-or-nothing, so one such item is a 400 for the whole batch and
/// every SDK drops a 400 without retrying.
/// </remarks>
[Collection("SauronScope")]
public class WireFixtureTests
{
    public WireFixtureTests() => ScopeManager.ResetForTests();

    [Fact]
    public async Task PostedEnvelopeIsWrittenAsTheWireFixture()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions
        {
            Release = "svc@1.4.2",
        });

        ScopeManager.Current.SetUser(new SauronUser { Id = "u_123", Email = "a@b.co" });
        ScopeManager.Current.SetTag("env", "prod");
        client.AddBreadcrumb(new Breadcrumb
        {
            Type = "navigation",
            Category = "history",
            Message = "went to /settings",
            Level = "info",
            Data = new Dictionary<string, object?> { ["from"] = "/", ["to"] = "/settings" },
        });

        client.Identify("u_123", new Dictionary<string, object?> { ["plan"] = "pro" });
        client.Track("checkout_completed", "u_123", new Dictionary<string, object?> { ["cart_value"] = 42.5 });
        try
        {
            throw new InvalidOperationException("x is not valid");
        }
        catch (InvalidOperationException ex)
        {
            client.CaptureException(ex);
        }
        client.CaptureMessage("payment provider returned a soft decline", "warning");
        client.TrackTransaction(
            "GET /api/users",
            durationMs: 128.4,
            op: "http",
            status: "ok",
            httpMethod: "GET",
            httpStatus: 200,
            url: "/api/users",
            distinctId: "u_123");

        await client.FlushAsync();

        Assert.Equal(1, handler.RequestCount);
        var body = handler.LastBody!;
        using var doc = JsonDocument.Parse(body);
        var items = doc.RootElement.GetProperty("items").EnumerateArray().ToList();
        var types = items.Select(i => i.GetProperty("type").GetString()).ToList();
        foreach (var required in new[] { "error", "event", "identify", "transaction" })
            Assert.Contains(required, types);
        Assert.Equal(2, types.Count(t => t == "error")); // exception + message

        // Any exception block must carry a real type string (non-Option on the wire),
        // and the item's text has to survive where the backend reads it.
        foreach (var item in items.Where(i => i.GetProperty("type").GetString() == "error"))
        {
            string? text = item.TryGetProperty("message", out var m) && m.ValueKind == JsonValueKind.String
                ? m.GetString()
                : null;
            if (item.TryGetProperty("exception", out var exc) && exc.ValueKind == JsonValueKind.Object)
            {
                var ty = exc.GetProperty("type");
                Assert.Equal(JsonValueKind.String, ty.ValueKind);
                Assert.False(string.IsNullOrEmpty(ty.GetString()));
                if (text is null && exc.TryGetProperty("value", out var v) && v.ValueKind == JsonValueKind.String)
                    text = v.GetString();
            }
            Assert.False(string.IsNullOrEmpty(text));
        }

        WireFixtureIo.Write("csharp", body);
    }
}
