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

    /// <summary>Capture a native exception. <paramref name="fingerprint"/> is an optional grouping override.</summary>
    public static void CaptureException(
        Exception exception,
        SauronUser? user = null,
        string level = "error",
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyList<string>? fingerprint = null)
        => Current?.CaptureException(exception, user, level, tags, fingerprint);

    /// <summary>Capture a plain message (default level <c>info</c>). <paramref name="fingerprint"/> is an optional grouping override.</summary>
    public static void CaptureMessage(string message, string level = "info", IReadOnlyList<string>? fingerprint = null)
        => Current?.CaptureMessage(message, level, fingerprint);

    /// <summary>Identify a user with traits.</summary>
    public static void Identify(string distinctId, IReadOnlyDictionary<string, object?>? traits = null)
        => Current?.Identify(distinctId, traits);

    /// <summary>Emit a performance transaction. <paramref name="distinctId"/> falls back to the scoped user id.</summary>
    public static void TrackTransaction(
        string name,
        double durationMs,
        string op = "custom",
        string? status = null,
        string? httpMethod = null,
        int? httpStatus = null,
        string? url = null,
        string? distinctId = null)
        => Current?.TrackTransaction(name, durationMs, op, status, httpMethod, httpStatus, url, distinctId);

    // ---- Scope API -----------------------------------------------------

    /// <summary>Set the user on the active scope (global when no scope is pushed). <c>null</c> clears it.</summary>
    public static void SetUser(SauronUser? user) => ScopeManager.Current.SetUser(user);

    /// <summary>Set a tag on the active scope.</summary>
    public static void SetTag(string key, string value) => ScopeManager.Current.SetTag(key, value);

    /// <summary>Set several tags on the active scope.</summary>
    public static void SetTags(IReadOnlyDictionary<string, string> tags) => ScopeManager.Current.SetTags(tags);

    /// <summary>Set a named context block on the active scope.</summary>
    public static void SetContext(string key, object? value) => ScopeManager.Current.SetContext(key, value);

    /// <summary>Set an extra value on the active scope.</summary>
    public static void SetExtra(string key, object? value) => ScopeManager.Current.SetExtra(key, value);

    /// <summary>
    /// Record a breadcrumb on the active scope. Uses the initialized client's
    /// <c>BeforeBreadcrumb</c> hook and <c>MaxBreadcrumbs</c> when available; otherwise
    /// records with defaults (still a no-op-safe call before init).
    /// </summary>
    public static void AddBreadcrumb(Breadcrumb breadcrumb)
    {
        var client = Current;
        if (client is not null)
            client.AddBreadcrumb(breadcrumb);
        else
            ScopeManager.Current.AddBreadcrumb(breadcrumb, 100);
    }

    /// <summary>
    /// Push an isolated scope for the duration of a <c>using</c> block (per-request isolation):
    /// <c>using (SauronSdk.PushScope()) { ... }</c>. Restores the previous scope on dispose.
    /// </summary>
    public static IDisposable PushScope() => ScopeManager.PushScope();

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
