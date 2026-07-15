using System;
using System.Text.Json;

namespace Sauron.Tests;

/// <summary>Shared helpers for the SDK unit tests.</summary>
internal static class TestUtil
{
    /// <summary>Build a client wired to a capturing handler with the background timer parked.</summary>
    public static SauronClient NewClient(CapturingHandler handler, SauronOptions? opts = null)
    {
        opts ??= new SauronOptions();
        opts.Dsn = "https://pub123@example.com/42";
        opts.HttpMessageHandler = handler;
        opts.FlushInterval = TimeSpan.FromHours(1);
        opts.MaxBatch = 1000;
        return new SauronClient(opts);
    }

    /// <summary>Parse the first envelope item out of a serialized body, detached from the source document.</summary>
    public static JsonElement FirstItem(string body)
    {
        using var doc = JsonDocument.Parse(body);
        var items = doc.RootElement.GetProperty("items");
        return JsonDocument.Parse(items[0].GetRawText()).RootElement;
    }
}
