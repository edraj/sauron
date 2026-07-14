using System;
using System.Collections.Generic;
using System.Threading.Tasks;

namespace Sauron;

/// <summary>
/// Static facade over a single process-wide <see cref="SauronClient"/>.
/// Call <see cref="Init(SauronOptions)"/> once at startup, then use the static
/// dispatch methods. All calls are no-ops until initialized.
/// </summary>
public static class SauronSdk
{
    private static SauronClient? _client;
    private static readonly object _gate = new();

    /// <summary>Initialize the SDK with a DSN string (uses defaults for everything else).</summary>
    public static void Init(string dsn) => Init(new SauronOptions { Dsn = dsn });

    /// <summary>Initialize the SDK. Replaces (and closes) any previously-initialized client.</summary>
    public static void Init(SauronOptions options)
    {
        var client = new SauronClient(options);
        SauronClient? previous;
        lock (_gate)
        {
            previous = _client;
            _client = client;
        }
        previous?.Close();
    }

    /// <summary>The current client, if initialized.</summary>
    public static SauronClient? Current
    {
        get { lock (_gate) { return _client; } }
    }

    /// <summary>Track a product-analytics event. <paramref name="distinctId"/> is required.</summary>
    public static void Track(string @event, string distinctId, IReadOnlyDictionary<string, object?>? properties = null)
        => Current?.Track(@event, distinctId, properties);

    /// <summary>Capture a native exception.</summary>
    public static void CaptureException(
        Exception exception,
        SauronUser? user = null,
        string level = "error",
        IReadOnlyDictionary<string, object?>? tags = null)
        => Current?.CaptureException(exception, user, level, tags);

    /// <summary>Capture a plain message (default level <c>info</c>).</summary>
    public static void CaptureMessage(string message, string level = "info")
        => Current?.CaptureMessage(message, level);

    /// <summary>Identify a user with traits.</summary>
    public static void Identify(string distinctId, IReadOnlyDictionary<string, object?>? traits = null)
        => Current?.Identify(distinctId, traits);

    /// <summary>Flush buffered items immediately (async).</summary>
    public static Task FlushAsync() => Current?.FlushAsync() ?? Task.CompletedTask;

    /// <summary>Flush buffered items immediately (blocking).</summary>
    public static void Flush() => Current?.Flush();

    /// <summary>Flush and stop the SDK.</summary>
    public static void Close()
    {
        SauronClient? client;
        lock (_gate)
        {
            client = _client;
            _client = null;
        }
        client?.Close();
    }
}
