using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Threading.Tasks;

namespace Sauron;

/// <summary>Configuration for a <see cref="SauronClient"/>.</summary>
public sealed class SauronOptions
{
    /// <summary>Ingest DSN (required): <c>https://&lt;public_key&gt;@&lt;host&gt;/&lt;project_id&gt;</c>.</summary>
    public string Dsn { get; set; } = string.Empty;

    /// <summary>Deployment environment. Default <c>production</c>.</summary>
    public string Environment { get; set; } = "production";

    /// <summary>Optional release identifier.</summary>
    public string? Release { get; set; }

    /// <summary>Error sample rate in [0, 1]. Default 1.0.</summary>
    public double SampleRate { get; set; } = 1.0;

    /// <summary>Background flush interval. Default 5 seconds.</summary>
    public TimeSpan FlushInterval { get; set; } = TimeSpan.FromSeconds(5);

    /// <summary>Flush automatically once this many items are buffered. Default 30.</summary>
    public int MaxBatch { get; set; } = 30;

    /// <summary>Emit diagnostic logging to stderr. Default false.</summary>
    public bool Debug { get; set; } = false;

    /// <summary>Module prefixes considered "in app" for stack frames. When null, everything outside System./Microsoft. is in-app.</summary>
    public IReadOnlyList<string>? InAppInclude { get; set; }

    /// <summary>Test seam: inject a custom <see cref="HttpMessageHandler"/> (e.g. a fake) so no network is hit.</summary>
    public HttpMessageHandler? HttpMessageHandler { get; set; }
}

/// <summary>A user attributed to a captured exception.</summary>
public sealed class SauronUser
{
    public string? Id { get; set; }
    public string? Email { get; set; }
    public string? Username { get; set; }
}

/// <summary>
/// A configured Sauron client. Dispatches product-analytics events, exceptions,
/// messages and identify calls to the ingest gateway over a buffered transport.
/// </summary>
public sealed class SauronClient : IDisposable
{
    private static readonly Random Rng = new();

    private readonly SauronOptions _options;
    private readonly Transport? _transport;
    private readonly bool _enabled;

    public SauronClient(SauronOptions options)
    {
        _options = options ?? throw new ArgumentNullException(nameof(options));

        Dsn dsn;
        try
        {
            dsn = Dsn.Parse(options.Dsn);
        }
        catch (ArgumentException ex)
        {
            // Disabled (no-op) mode when the DSN is missing/invalid — log, don't throw at init.
            if (options.Debug)
                Console.Error.WriteLine($"[sauron] disabled: {ex.Message}");
            _enabled = false;
            _transport = null;
            return;
        }

        HttpClient http;
        bool ownsHttp;
        if (options.HttpMessageHandler is not null)
        {
            http = new HttpClient(options.HttpMessageHandler, disposeHandler: false);
            ownsHttp = true;
        }
        else
        {
            http = SharedHttp;
            ownsHttp = false;
        }

        _transport = new Transport(dsn, options, http, ownsHttp);
        _enabled = true;
    }

    // A single shared HttpClient for the default (non-test) path.
    private static readonly HttpClient SharedHttp = new();

    /// <summary>Whether this client will dispatch (false = disabled/no-op due to bad DSN).</summary>
    public bool Enabled => _enabled && _transport is { Disabled: false };

    /// <summary>Track a product-analytics event. <paramref name="distinctId"/> is required by the wire contract.</summary>
    public void Track(string @event, string distinctId, IReadOnlyDictionary<string, object?>? properties = null)
    {
        if (!_enabled || _transport is null)
            return;
        if (string.IsNullOrEmpty(@event))
            throw new ArgumentException("event name is required.", nameof(@event));
        if (string.IsNullOrEmpty(distinctId))
            throw new ArgumentException("distinctId is required.", nameof(distinctId));

        var item = new EventItem
        {
            Name = @event,
            DistinctId = distinctId,
            Properties = properties is null ? new() : new Dictionary<string, object?>(properties),
            Timestamp = Transport.Iso8601Now(),
        };
        _transport.Enqueue(item);
    }

    /// <summary>Capture a native exception as an error item.</summary>
    public void CaptureException(
        Exception exception,
        SauronUser? user = null,
        string level = "error",
        IReadOnlyDictionary<string, object?>? tags = null)
    {
        if (!_enabled || _transport is null)
            return;
        if (exception is null)
            throw new ArgumentNullException(nameof(exception));

        // Error sampling.
        if (_options.SampleRate < 1.0)
        {
            double roll;
            lock (Rng) { roll = Rng.NextDouble(); }
            if (roll >= _options.SampleRate)
                return;
        }

        var item = new ErrorItem
        {
            EventId = Guid.NewGuid().ToString("N"),
            Level = string.IsNullOrEmpty(level) ? "error" : level,
            Timestamp = Transport.Iso8601Now(),
            Exception = new ExceptionInfo
            {
                Type = exception.GetType().FullName ?? exception.GetType().Name,
                Value = exception.Message,
                Mechanism = new MechanismInfo { Type = "generic", Handled = true },
                Stacktrace = StackTraceExtractor.Extract(exception, _options.InAppInclude),
            },
            Tags = tags is null ? new() : new Dictionary<string, object?>(tags),
            User = user is null ? null : new UserInfo { Id = user.Id, Email = user.Email, Username = user.Username },
        };
        _transport.Enqueue(item);
    }

    /// <summary>Capture a plain message as an error item (default level <c>info</c>).</summary>
    public void CaptureMessage(string message, string level = "info")
    {
        if (!_enabled || _transport is null)
            return;
        if (message is null)
            throw new ArgumentNullException(nameof(message));

        var item = new ErrorItem
        {
            EventId = Guid.NewGuid().ToString("N"),
            Level = string.IsNullOrEmpty(level) ? "info" : level,
            Timestamp = Transport.Iso8601Now(),
            Exception = null,
            Message = message,
        };
        _transport.Enqueue(item);
    }

    /// <summary>Identify a user with traits.</summary>
    public void Identify(string distinctId, IReadOnlyDictionary<string, object?>? traits = null)
    {
        if (!_enabled || _transport is null)
            return;
        if (string.IsNullOrEmpty(distinctId))
            throw new ArgumentException("distinctId is required.", nameof(distinctId));

        var item = new IdentifyItem
        {
            DistinctId = distinctId,
            Traits = traits is null ? new() : new Dictionary<string, object?>(traits),
            Timestamp = Transport.Iso8601Now(),
        };
        _transport.Enqueue(item);
    }

    /// <summary>Flush buffered items immediately (async).</summary>
    public Task FlushAsync() => _transport?.FlushAsync() ?? Task.CompletedTask;

    /// <summary>Flush buffered items immediately (blocking).</summary>
    public void Flush() => FlushAsync().GetAwaiter().GetResult();

    /// <summary>Flush and stop the client.</summary>
    public void Close() => Dispose();

    public void Dispose() => _transport?.Dispose();
}
