using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using Xunit;

namespace Sauron.Tests;

/// <summary>
/// C9 — the standing golden-envelope guard. Serializing the shared golden (a server-shaped
/// error item WITH breadcrumbs + tags + user + fingerprint, an event, an identify, and a
/// transaction) must be byte/shape-compatible with the locked ingest wire contract
/// (<c>backend/crates/sauron-core/src/envelope.rs</c>): snake_case keys, item <c>type</c>
/// tags, and the reconciled <c>ErrorItem</c> field set.
/// </summary>
[Collection("SauronScope")]
public class EnvelopeGoldenTests
{
    public EnvelopeGoldenTests() => ScopeManager.ResetForTests();

    /// <summary>
    /// The canonical JSON this SDK must emit. Field names snake_case; error items carry the
    /// reconciled set (event_id/level/timestamp/exception/message/breadcrumbs/tags/fingerprint/user).
    /// </summary>
    private const string GoldenJson = """
    {
      "header": {
        "dsn": "https://pk_test@localhost:8081/1",
        "sdk": { "name": "sauron-dotnet", "version": "0.3.0" },
        "sent_at": "2026-07-12T10:30:00.123Z",
        "environment": "production",
        "release": "svc@1.4.2"
      },
      "context": {
        "device": { "device_id": "dev-abc" },
        "os": { "name": "linux", "version": "6.1" },
        "app": {},
        "runtime": { "name": "dotnet", "version": "8.0.0" },
        "user": null
      },
      "items": [
        {
          "type": "error",
          "event_id": "a1b2c3d4e5f6478090a1b2c3d4e5f601",
          "level": "error",
          "timestamp": "2026-07-12T10:29:58.900Z",
          "exception": {
            "type": "System.InvalidOperationException",
            "value": "x is not valid",
            "mechanism": { "type": "onunhandledrejection", "handled": false },
            "stacktrace": [
              { "function": "LoadUser", "module": "App.Users", "filename": "Users.cs", "abs_path": null, "lineno": 42, "colno": null, "in_app": true }
            ]
          },
          "message": null,
          "breadcrumbs": [
            { "type": "navigation", "category": "history", "message": null, "level": "info", "timestamp": "2026-07-12T10:29:50.000Z", "data": { "from": "/", "to": "/settings" } }
          ],
          "tags": { "env": "prod", "req": "42" },
          "fingerprint": ["custom-group"],
          "user": { "id": "u_123", "email": null, "username": null },
          "session_id": null,
          "screen": null
        },
        {
          "type": "event",
          "name": "checkout_completed",
          "distinct_id": "u_123",
          "properties": { "cart_value": 42.5 },
          "timestamp": "2026-07-12T10:29:40.000Z",
          "session_id": null,
          "screen": null
        },
        {
          "type": "identify",
          "distinct_id": "u_123",
          "anonymous_id": null,
          "traits": { "plan": "pro" },
          "timestamp": "2026-07-12T10:29:39.000Z"
        },
        {
          "type": "transaction",
          "name": "GET /api/users",
          "op": "http",
          "duration_ms": 12.5,
          "status": "ok",
          "http_method": "GET",
          "http_status": 200,
          "url": "/api/users",
          "distinct_id": "u_123",
          "session_id": null,
          "timestamp": "2026-07-12T10:29:41.000Z"
        }
      ]
    }
    """;

    private static Envelope BuildGoldenEnvelope() => new()
    {
        Header = new EnvelopeHeader
        {
            Dsn = "https://pk_test@localhost:8081/1",
            Sdk = new SdkInfo(),
            SentAt = "2026-07-12T10:30:00.123Z",
            Environment = "production",
            Release = "svc@1.4.2",
        },
        Context = new EnvelopeContext
        {
            Device = new DeviceInfo { DeviceId = "dev-abc" },
            Os = new OsInfo { Name = "linux", Version = "6.1" },
            App = new Dictionary<string, object?>(),
            Runtime = new RuntimeInfo { Name = "dotnet", Version = "8.0.0" },
            User = null,
        },
        Items = new List<object>
        {
            new ErrorItem
            {
                EventId = "a1b2c3d4e5f6478090a1b2c3d4e5f601",
                Level = "error",
                Timestamp = "2026-07-12T10:29:58.900Z",
                Exception = new ExceptionInfo
                {
                    Type = "System.InvalidOperationException",
                    Value = "x is not valid",
                    Mechanism = new MechanismInfo { Type = "onunhandledrejection", Handled = false },
                    Stacktrace = new List<StackFrame>
                    {
                        new() { Function = "LoadUser", Module = "App.Users", Filename = "Users.cs", Lineno = 42, InApp = true },
                    },
                },
                Message = null,
                Breadcrumbs = new List<object>
                {
                    new BreadcrumbWire
                    {
                        Type = "navigation",
                        Category = "history",
                        Message = null,
                        Level = "info",
                        Timestamp = "2026-07-12T10:29:50.000Z",
                        Data = new Dictionary<string, object?> { ["from"] = "/", ["to"] = "/settings" },
                    },
                },
                Tags = new Dictionary<string, object?> { ["env"] = "prod", ["req"] = "42" },
                Fingerprint = new List<string> { "custom-group" },
                User = new UserInfo { Id = "u_123", Email = null, Username = null },
            },
            new EventItem
            {
                Name = "checkout_completed",
                DistinctId = "u_123",
                Properties = new Dictionary<string, object?> { ["cart_value"] = 42.5 },
                Timestamp = "2026-07-12T10:29:40.000Z",
            },
            new IdentifyItem
            {
                DistinctId = "u_123",
                AnonymousId = null,
                Traits = new Dictionary<string, object?> { ["plan"] = "pro" },
                Timestamp = "2026-07-12T10:29:39.000Z",
            },
            new TransactionItem
            {
                Name = "GET /api/users",
                Op = "http",
                DurationMs = 12.5,
                Status = "ok",
                HttpMethod = "GET",
                HttpStatus = 200,
                Url = "/api/users",
                DistinctId = "u_123",
                Timestamp = "2026-07-12T10:29:41.000Z",
            },
        },
    };

    [Fact]
    public void SerializedGoldenEnvelope_MatchesTheWireContractShape()
    {
        string actual = JsonSerializer.Serialize(BuildGoldenEnvelope(), SauronJson.Options);

        using var actualDoc = JsonDocument.Parse(actual);
        using var expectedDoc = JsonDocument.Parse(GoldenJson);

        Assert.True(
            JsonDeepEquals(actualDoc.RootElement, expectedDoc.RootElement),
            $"Serialized envelope drifted from the golden fixture.\nActual: {actual}");
    }

    [Fact]
    public void Golden_UsesSnakeCaseKeys_AndItemTypeTags()
    {
        string actual = JsonSerializer.Serialize(BuildGoldenEnvelope(), SauronJson.Options);
        using var doc = JsonDocument.Parse(actual);
        var items = doc.RootElement.GetProperty("items");

        Assert.Equal(new[] { "error", "event", "identify", "transaction" },
            items.EnumerateArray().Select(i => i.GetProperty("type").GetString()).ToArray());

        var error = items[0];
        Assert.True(error.TryGetProperty("event_id", out _));
        Assert.True(error.GetProperty("exception").GetProperty("stacktrace")[0].TryGetProperty("in_app", out _));
        Assert.True(error.GetProperty("exception").GetProperty("stacktrace")[0].TryGetProperty("abs_path", out _));

        var tx = items[3];
        Assert.True(tx.TryGetProperty("duration_ms", out _));
        Assert.True(tx.TryGetProperty("http_status", out _));
        Assert.True(tx.TryGetProperty("distinct_id", out _));
    }

    [Fact]
    public void Golden_ErrorItem_CarriesFingerprintTagsUserBreadcrumbs()
    {
        string actual = JsonSerializer.Serialize(BuildGoldenEnvelope(), SauronJson.Options);
        using var doc = JsonDocument.Parse(actual);
        var error = doc.RootElement.GetProperty("items")[0];

        // fingerprint is an array of strings, honored verbatim by the backend.
        var fp = error.GetProperty("fingerprint");
        Assert.Equal(JsonValueKind.Array, fp.ValueKind);
        Assert.Equal("custom-group", fp[0].GetString());

        Assert.Equal("prod", error.GetProperty("tags").GetProperty("env").GetString());
        Assert.Equal("u_123", error.GetProperty("user").GetProperty("id").GetString());
        Assert.Equal("navigation", error.GetProperty("breadcrumbs")[0].GetProperty("type").GetString());
        Assert.False(error.GetProperty("exception").GetProperty("mechanism").GetProperty("handled").GetBoolean());
    }

    [Fact]
    public void SdkHeader_ReportsVersion_0_3_0()
    {
        string actual = JsonSerializer.Serialize(BuildGoldenEnvelope(), SauronJson.Options);
        using var doc = JsonDocument.Parse(actual);
        var sdk = doc.RootElement.GetProperty("header").GetProperty("sdk");
        Assert.Equal("sauron-dotnet", sdk.GetProperty("name").GetString());
        Assert.Equal("0.3.0", sdk.GetProperty("version").GetString());
    }

    // ---- End-to-end reconciliation through the real capture path ----------

    [Fact]
    public void CapturedError_LiftsScopedBreadcrumbsTagsUser_AndHonorsFingerprintOverride()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        ScopeManager.Current.SetUser(new SauronUser { Id = "u_9", Email = "a@b.co" });
        ScopeManager.Current.SetTag("env", "prod");
        client.AddBreadcrumb(new Breadcrumb { Type = "navigation", Message = "opened" });

        try { throw new System.InvalidOperationException("kaboom"); }
        catch (System.Exception ex)
        {
            client.CaptureException(ex, fingerprint: new[] { "grp-a", "grp-b" });
        }
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("error", item.GetProperty("type").GetString());
        Assert.Equal("opened", item.GetProperty("breadcrumbs")[0].GetProperty("message").GetString());
        Assert.Equal("prod", item.GetProperty("tags").GetProperty("env").GetString());
        Assert.Equal("u_9", item.GetProperty("user").GetProperty("id").GetString());

        var fp = item.GetProperty("fingerprint");
        Assert.Equal(JsonValueKind.Array, fp.ValueKind);
        Assert.Equal(new[] { "grp-a", "grp-b" }, fp.EnumerateArray().Select(e => e.GetString()).ToArray());
    }

    [Fact]
    public void CapturedError_Fingerprint_IsNull_WhenNoOverride()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.CaptureMessage("hello");
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal(JsonValueKind.Null, item.GetProperty("fingerprint").ValueKind);
    }

    // ---- helpers ----------------------------------------------------------

    /// <summary>Order-independent deep JSON equality (object keys by name, arrays positionally).</summary>
    private static bool JsonDeepEquals(JsonElement a, JsonElement b)
    {
        if (a.ValueKind != b.ValueKind)
            return false;

        switch (a.ValueKind)
        {
            case JsonValueKind.Object:
                var ap = a.EnumerateObject().ToDictionary(p => p.Name, p => p.Value);
                var bp = b.EnumerateObject().ToDictionary(p => p.Name, p => p.Value);
                if (ap.Count != bp.Count)
                    return false;
                foreach (var kv in ap)
                {
                    if (!bp.TryGetValue(kv.Key, out var bv) || !JsonDeepEquals(kv.Value, bv))
                        return false;
                }
                return true;
            case JsonValueKind.Array:
                if (a.GetArrayLength() != b.GetArrayLength())
                    return false;
                var al = a.EnumerateArray().ToList();
                var bl = b.EnumerateArray().ToList();
                for (int i = 0; i < al.Count; i++)
                {
                    if (!JsonDeepEquals(al[i], bl[i]))
                        return false;
                }
                return true;
            case JsonValueKind.String:
                return a.GetString() == b.GetString();
            case JsonValueKind.Number:
                return a.GetDouble() == b.GetDouble();
            default:
                return true; // True / False / Null: kind already matched
        }
    }
}
