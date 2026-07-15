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

    /// <summary>Maximum breadcrumbs retained on a scope's ring buffer. Default 100.</summary>
    public int MaxBreadcrumbs { get; set; } = 100;

    /// <summary>
    /// Optional hook run on each breadcrumb before it is recorded. Return the (possibly
    /// mutated) crumb to keep it, or <c>null</c> to drop it.
    /// </summary>
    public Func<Breadcrumb, Breadcrumb?>? BeforeBreadcrumb { get; set; }

    /// <summary>
    /// Optional hook run on every outgoing item (event, error, identify, transaction)
    /// just before it is buffered for transport. Return the (possibly replaced) item to
    /// send it, or <c>null</c> to drop it. The redaction / PII-scrubbing seam.
    /// </summary>
    public Func<object, object?>? BeforeSend { get; set; }

    /// <summary>
    /// Gzip the request body when it exceeds this many bytes (sets <c>Content-Encoding: gzip</c>).
    /// Default 1024. Set to <see cref="int.MaxValue"/> to effectively disable compression.
    /// </summary>
    public int GzipThresholdBytes { get; set; } = 1024;

    /// <summary>
    /// Byte cap for the in-memory pending-envelope queue (the transient-outage buffer).
    /// When exceeded, the oldest queued envelopes are dropped. Default 1 MiB.
    /// </summary>
    public int MaxQueueBytes { get; set; } = 1_048_576;

    /// <summary>
    /// Opt-in directory for on-disk queue persistence (at-least-once delivery across restarts).
    /// Default <c>null</c> (in-memory only). When set, pending envelopes are written FIFO and
    /// reloaded on the next start; each is deleted once delivered.
    /// </summary>
    public string? OfflineDir { get; set; }

    /// <summary>
    /// Opt-in auto-capture of uncaught errors (default <c>false</c>). When enabled, the client
    /// subscribes to <see cref="AppDomain.UnhandledException"/> and
    /// <see cref="TaskScheduler.UnobservedTaskException"/>, capturing each with
    /// <c>mechanism.handled = false</c> and preserving the runtime's default crash/exit behavior.
    /// Off by default because process-wide handlers are risky on a server; opt in explicitly.
    /// </summary>
    public bool AutoCaptureUnhandled { get; set; } = false;

    /// <summary>Test seam: inject a custom <see cref="HttpMessageHandler"/> (e.g. a fake) so no network is hit.</summary>
    public HttpMessageHandler? HttpMessageHandler { get; set; }

    /// <summary>
    /// Test seam: override the retry backoff sleep. Receives the intended delay and returns when
    /// the "sleep" is done — a no-op implementation makes the retry policy deterministic in tests.
    /// </summary>
    internal Func<TimeSpan, Task>? DelayHook { get; set; }
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
    private readonly AutoCapture? _autoCapture;

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

        // Opt-in only, and only for an enabled client — never wire global handlers in no-op mode.
        if (options.AutoCaptureUnhandled)
            _autoCapture = AutoCapture.Install(this);
    }

    // A single shared HttpClient for the default (non-test) path.
    private static readonly HttpClient SharedHttp = new();

    /// <summary>Whether this client will dispatch (false = disabled/no-op due to bad DSN).</summary>
    public bool Enabled => _enabled && _transport is { Disabled: false };

    /// <summary>The live auto-capture installation when <see cref="SauronOptions.AutoCaptureUnhandled"/> is on; otherwise null.</summary>
    internal AutoCapture? AutoCapture => _autoCapture;

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
        Dispatch(item);
    }

    /// <summary>Record a breadcrumb on the active scope (runs the <c>BeforeBreadcrumb</c> hook first).</summary>
    public void AddBreadcrumb(Breadcrumb breadcrumb)
    {
        if (breadcrumb is null)
            throw new ArgumentNullException(nameof(breadcrumb));

        if (_options.BeforeBreadcrumb is not null)
        {
            Breadcrumb? processed;
            try
            {
                processed = _options.BeforeBreadcrumb(breadcrumb);
            }
            catch (Exception ex)
            {
                Log($"beforeBreadcrumb threw; dropping crumb: {ex.Message}");
                return;
            }
            if (processed is null)
                return;
            breadcrumb = processed;
        }

        ScopeManager.Current.AddBreadcrumb(breadcrumb, _options.MaxBreadcrumbs);
    }

    /// <summary>Emit a performance transaction. <paramref name="distinctId"/> falls back to the scoped user id.</summary>
    public void TrackTransaction(
        string name,
        double durationMs,
        string op = "custom",
        string? status = null,
        string? httpMethod = null,
        int? httpStatus = null,
        string? url = null,
        string? distinctId = null)
    {
        if (!_enabled || _transport is null)
            return;
        if (string.IsNullOrEmpty(name))
            throw new ArgumentException("transaction name is required.", nameof(name));

        var item = new TransactionItem
        {
            Name = name,
            Op = string.IsNullOrEmpty(op) ? "custom" : op,
            DurationMs = durationMs,
            Status = status,
            HttpMethod = httpMethod,
            HttpStatus = httpStatus,
            Url = url,
            DistinctId = distinctId ?? ScopeManager.Current.User?.Id,
            Timestamp = Transport.Iso8601Now(),
        };
        Dispatch(item);
    }

    /// <summary>
    /// Single chokepoint before an item is buffered: run <c>BeforeSend</c> (drop on null,
    /// replace on non-null), then enqueue. Keeps every dispatch path uniform.
    /// </summary>
    private void Dispatch(object item)
    {
        if (_transport is null)
            return;

        if (_options.BeforeSend is not null)
        {
            object? processed;
            try
            {
                processed = _options.BeforeSend(item);
            }
            catch (Exception ex)
            {
                Log($"beforeSend threw; dropping item: {ex.Message}");
                return;
            }
            if (processed is null)
                return;
            item = processed;
        }

        _transport.Enqueue(item);
    }

    private void Log(string message)
    {
        if (_options.Debug)
            Console.Error.WriteLine($"[sauron] {message}");
    }

    /// <summary>
    /// Capture a native exception as an error item. <paramref name="fingerprint"/> is an optional
    /// grouping override honored verbatim by the backend when present.
    /// </summary>
    public void CaptureException(
        Exception exception,
        SauronUser? user = null,
        string level = "error",
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyList<string>? fingerprint = null)
    {
        if (!_enabled || _transport is null)
            return;
        if (exception is null)
            throw new ArgumentNullException(nameof(exception));

        CaptureExceptionCore(
            exception, user, level, tags, fingerprint,
            mechanismType: "generic", handled: true, applySampling: true);
    }

    /// <summary>
    /// Capture an uncaught exception with <c>mechanism.handled = false</c> (used by opt-in
    /// auto-capture). A crash is always kept, so error sampling is bypassed.
    /// </summary>
    internal void CaptureUnhandled(Exception exception, string mechanismType)
    {
        if (!_enabled || _transport is null || exception is null)
            return;

        CaptureExceptionCore(
            exception, user: null, level: "error", tags: null, fingerprint: null,
            mechanismType: mechanismType, handled: false, applySampling: false);
    }

    private void CaptureExceptionCore(
        Exception exception,
        SauronUser? user,
        string level,
        IReadOnlyDictionary<string, object?>? tags,
        IReadOnlyList<string>? fingerprint,
        string mechanismType,
        bool handled,
        bool applySampling)
    {
        // Error sampling (handled captures only; an uncaught crash is always kept).
        if (applySampling && _options.SampleRate < 1.0)
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
                Mechanism = new MechanismInfo
                {
                    Type = string.IsNullOrEmpty(mechanismType) ? "generic" : mechanismType,
                    Handled = handled,
                },
                Stacktrace = StackTraceExtractor.Extract(exception, _options.InAppInclude),
            },
            Tags = tags is null ? new() : new Dictionary<string, object?>(tags),
            Fingerprint = fingerprint is null ? null : new List<string>(fingerprint),
            User = user is null ? null : new UserInfo { Id = user.Id, Email = user.Email, Username = user.Username },
        };
        // Merge the active scope (tags/user under any per-call overrides, plus breadcrumbs).
        ScopeManager.Current.ApplyToError(item);
        Dispatch(item);
    }

    /// <summary>
    /// Capture a plain message as an error item (default level <c>info</c>).
    /// <paramref name="fingerprint"/> is an optional grouping override.
    /// </summary>
    public void CaptureMessage(string message, string level = "info", IReadOnlyList<string>? fingerprint = null)
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
            Fingerprint = fingerprint is null ? null : new List<string>(fingerprint),
        };
        ScopeManager.Current.ApplyToError(item);
        Dispatch(item);
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
        Dispatch(item);
    }

    /// <summary>Flush buffered items immediately (async).</summary>
    public Task FlushAsync() => _transport?.FlushAsync() ?? Task.CompletedTask;

    /// <summary>Flush buffered items immediately (blocking).</summary>
    public void Flush() => FlushAsync().GetAwaiter().GetResult();

    /// <summary>Flush and stop the client.</summary>
    public void Close() => Dispose();

    public void Dispose()
    {
        // Unsubscribe global handlers before tearing down transport so a late crash can't
        // dispatch onto a disposed client.
        _autoCapture?.Dispose();
        _transport?.Dispose();
    }
}
