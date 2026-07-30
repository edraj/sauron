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

    /// <summary>Optional release identifier.</summary>
    public string? Release { get; set; }

    /// <summary>Default tags seeded into the global scope at init (string -> string). Optional.</summary>
    public IReadOnlyDictionary<string, string>? Tags { get; set; }

    /// <summary>Default context blocks seeded into the global scope at init (name -> block). Optional.
    /// DISTINCT from the machine envelope <c>context</c>.</summary>
    public IReadOnlyDictionary<string, object?>? Contexts { get; set; }

    /// <summary>Default extra values seeded into the global scope at init (key -> any). Optional.</summary>
    public IReadOnlyDictionary<string, object?>? Extra { get; set; }

    /// <summary>Error sample rate in [0, 1]. Default 1.0.</summary>
    public double SampleRate { get; set; } = 1.0;

    /// <summary>Background flush interval. Default 5 seconds.</summary>
    public TimeSpan FlushInterval { get; set; } = TimeSpan.FromSeconds(5);

    /// <summary>Flush automatically once this many items are buffered. Default 30.</summary>
    public int MaxBatch { get; set; } = 30;

    /// <summary>
    /// Hard ceiling on items per envelope. Default 1000, matching the server's limit —
    /// a larger envelope is rejected as a non-retryable 400 and its items are lost.
    /// <see cref="MaxBatch"/> only triggers a flush; this is what bounds the request.
    /// </summary>
    public int MaxItemsPerEnvelope { get; set; } = 1000;

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

        // Seed init-default metadata scopes into the process-wide global scope.
        if (options.Tags is not null)
            foreach (var kv in options.Tags)
                ScopeManager.Global.SetTag(kv.Key, kv.Value);
        if (options.Contexts is not null)
            foreach (var kv in options.Contexts)
                ScopeManager.Global.SetContext(kv.Key, kv.Value);
        if (options.Extra is not null)
            foreach (var kv in options.Extra)
                ScopeManager.Global.SetExtra(kv.Key, kv.Value);

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
    public void Track(
        string @event,
        string distinctId,
        IReadOnlyDictionary<string, object?>? properties = null,
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyDictionary<string, object?>? contexts = null,
        IReadOnlyDictionary<string, object?>? extra = null)
    {
        if (!_enabled || _transport is null)
            return;
        if (string.IsNullOrEmpty(@event))
            throw new ArgumentException("event name is required.", nameof(@event));
        if (string.IsNullOrEmpty(distinctId))
            throw new ArgumentException("distinctId is required.", nameof(distinctId));

        TrackCore(@event, distinctId, properties, tags, contexts, extra);
    }

    /// <summary>
    /// Build and dispatch an event item. Shared by the public <see cref="Track"/> (which
    /// validates its arguments first) and the internal workflow lifecycle emitters, which
    /// deliberately pass an EMPTY <paramref name="distinctId"/> when no user is in scope —
    /// see <see cref="WorkflowDistinctId"/> for why that is correct rather than degraded.
    /// </summary>
    private void TrackCore(
        string @event,
        string distinctId,
        IReadOnlyDictionary<string, object?>? properties,
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyDictionary<string, object?>? contexts = null,
        IReadOnlyDictionary<string, object?>? extra = null)
    {
        if (!_enabled || _transport is null)
            return;

        var item = new EventItem
        {
            Name = @event,
            DistinctId = distinctId,
            Properties = properties is null ? new() : new Dictionary<string, object?>(properties),
            Timestamp = Transport.Iso8601Now(),
            Tags = tags is null || tags.Count == 0 ? null : new Dictionary<string, object?>(tags),
            Contexts = contexts is null || contexts.Count == 0 ? null : new Dictionary<string, object?>(contexts),
            Extra = extra is null || extra.Count == 0 ? null : new Dictionary<string, object?>(extra),
        };
        ScopeManager.Current.ApplyToEvent(item);
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
    /// Single chokepoint before an item is buffered: stamp the active workflow (if any),
    /// run <c>BeforeSend</c> (drop on null, replace on non-null), then enqueue. Keeps every
    /// dispatch path uniform and is why a future capture path can't forget to stamp.
    /// </summary>
    private void Dispatch(object item)
    {
        if (_transport is null)
            return;

        StampWorkflow(item);

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
    /// Stamp <c>workflow_id</c>/<c>workflow_name</c> from the active scope onto the item —
    /// error, event, and transaction items only. Never <c>IdentifyItem</c>: the server has
    /// no workflow columns for it. Reads <see cref="ScopeManager.Current"/> directly (not a
    /// static field), so concurrent requests never stamp each other's workflow.
    /// </summary>
    private static void StampWorkflow(object item)
    {
        var wf = ScopeManager.Current.Workflow;
        if (wf is null)
            return;

        switch (item)
        {
            case EventItem e:
                e.WorkflowId = wf.WorkflowId;
                e.WorkflowName = wf.Name;
                break;
            case ErrorItem er:
                er.WorkflowId = wf.WorkflowId;
                er.WorkflowName = wf.Name;
                break;
            case TransactionItem t:
                t.WorkflowId = wf.WorkflowId;
                t.WorkflowName = wf.Name;
                break;
        }
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
        IReadOnlyList<string>? fingerprint = null,
        IReadOnlyDictionary<string, object?>? contexts = null,
        IReadOnlyDictionary<string, object?>? extra = null)
    {
        if (!_enabled || _transport is null)
            return;
        if (exception is null)
            throw new ArgumentNullException(nameof(exception));

        CaptureExceptionCore(
            exception, user, level, tags, fingerprint, contexts, extra,
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
            contexts: null, extra: null,
            mechanismType: mechanismType, handled: false, applySampling: false);
    }

    private void CaptureExceptionCore(
        Exception exception,
        SauronUser? user,
        string level,
        IReadOnlyDictionary<string, object?>? tags,
        IReadOnlyList<string>? fingerprint,
        IReadOnlyDictionary<string, object?>? contexts,
        IReadOnlyDictionary<string, object?>? extra,
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
            Contexts = contexts is null || contexts.Count == 0 ? null : new Dictionary<string, object?>(contexts),
            Extra = extra is null || extra.Count == 0 ? null : new Dictionary<string, object?>(extra),
            Fingerprint = fingerprint is null ? null : new List<string>(fingerprint),
            User = user is null ? null : new UserInfo { Id = user.Id, Email = user.Email, Username = user.Username },
        };
        // Merge the active scope (tags/contexts/extra/user under any per-call overrides, plus breadcrumbs).
        ScopeManager.Current.ApplyToError(item);
        Dispatch(item);
    }

    /// <summary>
    /// Capture a plain message as an error item (default level <c>info</c>).
    /// <paramref name="fingerprint"/> is an optional grouping override.
    /// </summary>
    public void CaptureMessage(
        string message,
        string level = "info",
        IReadOnlyList<string>? fingerprint = null,
        IReadOnlyDictionary<string, object?>? tags = null,
        IReadOnlyDictionary<string, object?>? contexts = null,
        IReadOnlyDictionary<string, object?>? extra = null)
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
            Tags = tags is null ? new() : new Dictionary<string, object?>(tags),
            Contexts = contexts is null || contexts.Count == 0 ? null : new Dictionary<string, object?>(contexts),
            Extra = extra is null || extra.Count == 0 ? null : new Dictionary<string, object?>(extra),
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

    // ---- Workflows -------------------------------------------------------

    /// <summary>
    /// Start a named workflow on the active scope, stamping subsequently tracked
    /// events/errors/transactions with its id/name until <see cref="EndWorkflow"/> /
    /// <see cref="CancelWorkflow"/>. <paramref name="force"/> supersedes an already-active
    /// workflow (emitting <c>$workflow_cancel</c> with <c>reason: "superseded"</c> for it
    /// first); otherwise an active workflow makes this a no-op returning
    /// <see cref="WorkflowStatus.AlreadyActive"/>.
    /// </summary>
    /// <remarks>
    /// The workflow id is a fresh client-generated UUID — the server rolls counters up on
    /// <c>(app_id, workflow_id)</c> app-wide, so a deterministic or reused id would merge
    /// counts from unrelated environments/sessions into one row.
    /// </remarks>
    public WorkflowResult StartWorkflow(string name, bool force = false)
    {
        try
        {
            if (!Enabled)
                return new WorkflowResult(WorkflowStatus.Disabled);

            var normalized = WorkflowNames.Normalize(name);
            if (normalized is null)
            {
                Log($"StartWorkflow: invalid name '{name}'");
                return new WorkflowResult(WorkflowStatus.InvalidName);
            }

            var scope = ScopeManager.Current;
            var active = scope.Workflow;
            if (active is not null && !force)
            {
                Log($"StartWorkflow(\"{normalized}\"): \"{active.Name}\" is already active; pass force: true to replace it");
                return new WorkflowResult(WorkflowStatus.AlreadyActive);
            }

            // Mint the replacement BEFORE closing the one being superseded. Both operations
            // here are effectively infallible in .NET, but the ordering rule is the point:
            // if construction threw after the supersede-cancel had already reached the wire,
            // the outer catch would return Disabled ("nothing changed") while the old
            // workflow was cancelled server-side AND still sitting in scope.Workflow — every
            // later item would then stamp a workflow the server has recorded as cancelled.
            // Minting first makes the only throwing step precede any observable side effect.
            var workflow = new ActiveWorkflow(Guid.NewGuid().ToString(), normalized, DateTimeOffset.UtcNow);

            if (active is not null)
            {
                try
                {
                    EmitWorkflowClose(active, WorkflowEvents.Cancel, "superseded");
                }
                catch (Exception ex)
                {
                    Log($"StartWorkflow: superseding {WorkflowEvents.Cancel} emit threw: {ex.Message}");
                }
            }

            // Set state BEFORE emitting so $workflow_start is itself stamped with it (via
            // the Dispatch chokepoint). A failure emitting from here on still returns Ok
            // with the id: the workflow IS live, and the server materializes the row from
            // the first stamped event it actually receives — a lost $workflow_start is
            // recoverable, a lost local id is not.
            scope.Workflow = workflow;
            try
            {
                EmitWorkflowStart(workflow);
            }
            catch (Exception ex)
            {
                Log($"StartWorkflow: {WorkflowEvents.Start} emit threw: {ex.Message}");
            }
            return new WorkflowResult(WorkflowStatus.Ok, workflow.WorkflowId);
        }
        catch (Exception ex)
        {
            Log($"StartWorkflow threw: {ex.Message}");
            return new WorkflowResult(WorkflowStatus.Disabled);
        }
    }

    /// <summary>
    /// End the active workflow (or the one named <paramref name="name"/>, if given).
    /// Emits <c>$workflow_end</c> with <c>duration_ms</c> and clears the state. A no-op
    /// returning <see cref="WorkflowStatus.NotActive"/> / <see cref="WorkflowStatus.NameMismatch"/>
    /// when the precondition fails.
    /// </summary>
    public WorkflowResult EndWorkflow(string? name = null) => CloseWorkflow(WorkflowEvents.End, name, reason: null);

    /// <summary>
    /// Cancel the active workflow (or the one named <paramref name="name"/>, if given).
    /// Emits <c>$workflow_cancel</c> with <c>duration_ms</c> and <paramref name="reason"/>
    /// (default <c>"user"</c>, trimmed and capped at 120 chars) and clears the state.
    /// </summary>
    public WorkflowResult CancelWorkflow(string? name = null, string? reason = null)
        => CloseWorkflow(WorkflowEvents.Cancel, name, reason);

    /// <summary>The workflow currently bounding the active scope, or <c>null</c> if none.</summary>
    public ActiveWorkflow? GetWorkflow() => ScopeManager.Current.Workflow;

    /// <summary>Shared precondition + close logic for <see cref="EndWorkflow"/>/<see cref="CancelWorkflow"/>.</summary>
    private WorkflowResult CloseWorkflow(string eventName, string? name, string? reason)
    {
        try
        {
            if (!Enabled)
                return new WorkflowResult(WorkflowStatus.Disabled);

            var scope = ScopeManager.Current;
            var active = scope.Workflow;
            if (active is null)
                return new WorkflowResult(WorkflowStatus.NotActive);

            // An explicit name that fails normalization (blank, > 120) also reports
            // NameMismatch here — InvalidName is reachable only from StartWorkflow.
            if (name is not null && WorkflowNames.Normalize(name) != active.Name)
            {
                Log($"{eventName}: \"{name}\" does not match active workflow \"{active.Name}\"");
                return new WorkflowResult(WorkflowStatus.NameMismatch);
            }

            var workflowId = active.WorkflowId;
            try
            {
                EmitWorkflowClose(active, eventName, reason);
            }
            catch (Exception ex)
            {
                Log($"{eventName}: emit threw: {ex.Message}");
            }
            finally
            {
                // Clear even if the emit threw — the caller asked to end/cancel and must
                // never observe the workflow "stuck" active afterwards.
                scope.Workflow = null;
            }
            return new WorkflowResult(WorkflowStatus.Ok, workflowId);
        }
        catch (Exception ex)
        {
            Log($"{eventName} threw: {ex.Message}");
            return new WorkflowResult(WorkflowStatus.Disabled);
        }
    }

    /// <summary>Emit <c>$workflow_start</c> for a freshly-started workflow.</summary>
    private void EmitWorkflowStart(ActiveWorkflow workflow)
    {
        TrackCore(WorkflowEvents.Start, WorkflowDistinctId(), new Dictionary<string, object?>
        {
            ["workflow_id"] = workflow.WorkflowId,
            ["workflow_name"] = workflow.Name,
        });
    }

    /// <summary>
    /// Emit the closing lifecycle event (<c>$workflow_end</c>/<c>$workflow_cancel</c>) for
    /// <paramref name="active"/> while it is STILL the active workflow (so the Dispatch
    /// chokepoint stamps this very item with it). Does not mutate scope state — the caller
    /// owns clearing/replacing it, so the transition never observes a half-mutated scope.
    /// </summary>
    private void EmitWorkflowClose(ActiveWorkflow active, string eventName, string? reason)
    {
        var properties = new Dictionary<string, object?>
        {
            ["workflow_id"] = active.WorkflowId,
            ["workflow_name"] = active.Name,
            ["duration_ms"] = Math.Max(0, (DateTimeOffset.UtcNow - active.StartedAt).TotalMilliseconds),
        };
        if (eventName == WorkflowEvents.Cancel)
            properties["reason"] = WorkflowNames.NormalizeReason(reason);

        TrackCore(eventName, WorkflowDistinctId(), properties);
    }

    /// <summary>
    /// Distinct id for an internally-emitted workflow lifecycle event: the scoped user id
    /// when there is one, otherwise the EMPTY STRING.
    /// </summary>
    /// <remarks>
    /// Empty is correct here, not a degraded fallback, and must not be replaced with a
    /// synthetic id (an <c>anon_*</c> value, the device id, or a <c>"system"</c> sentinel).
    /// The server is built for it:
    /// <list type="bullet">
    /// <item>the <c>bump_workflow</c> call sites in
    /// <c>backend/crates/sauron-pipeline/src/process.rs</c> (~:381 and ~:440) pass
    /// <c>Some(distinct_id.as_str()).filter(|s| !s.is_empty())</c>, so an empty value lands
    /// as SQL <c>NULL</c> on the <c>workflows</c> row;</item>
    /// <item><c>WORKFLOW_OUTCOME_SELECT</c> in <c>backend/crates/sauron-db/src/repo.rs</c>
    /// (~:3162) computes <c>COUNT(DISTINCT w.distinct_id) AS unique_users</c>, and
    /// <c>COUNT(DISTINCT ...)</c> skips NULLs.</item>
    /// </list>
    /// So an anonymous run contributes nothing to <c>unique_users</c>. Any synthetic id
    /// would instead fabricate a user per run — worst of all a per-workflow one, since
    /// <c>workflow_id</c> is a fresh UUID every time. <c>AnalyticsItem.distinct_id</c> is a
    /// required <c>String</c> on the wire (<c>backend/crates/sauron-core/src/envelope.rs</c>
    /// ~:226), so the field is still sent — just empty. This is why the lifecycle emitters
    /// call <see cref="TrackCore"/> rather than the public <see cref="Track"/>, whose
    /// non-empty-distinctId guard applies to ordinary caller events only.
    /// </remarks>
    private static string WorkflowDistinctId()
        => ScopeManager.Current.User?.Id is { Length: > 0 } uid ? uid : string.Empty;

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

        // Drop any workflow left un-ended on the process-wide scope. Deliberately NOT an
        // auto-emitted $workflow_cancel: an abandoned workflow is a legitimate outcome the
        // server derives on read (30 min), and fabricating a cancel would misreport it.
        //
        // Global only — never ScopeManager.Current: at dispose time "current" is whatever
        // async-local scope the disposing thread happens to be in, which has nothing to do
        // with the client being torn down. A workflow on a pushed scope needs no cleanup,
        // since that scope is discarded when its `using` block exits. Without this, a
        // Close()-then-Init() config reload would leave a stale workflow on Global and the
        // next StartWorkflow would return AlreadyActive against a brand-new client.
        // (Symmetric with the constructor, which already seeds ScopeManager.Global.)
        ScopeManager.Global.Workflow = null;
    }
}
