using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace Sauron;

/// <summary>
/// Buffered background HTTP transport. Items are queued in memory and flushed
/// either on a timer (<c>flush_interval</c>), when <c>max_batch</c> is reached,
/// or explicitly via <see cref="FlushAsync"/>. One envelope is built per flush,
/// pushed onto a bounded pending-queue, and drained to the ingest with gzip
/// compression and a retry/backoff policy.
/// </summary>
internal sealed class Transport : IDisposable
{
    /// <summary>Outcome of a single envelope send attempt-cycle.</summary>
    private enum SendOutcome
    {
        /// <summary>Accepted (2xx) — remove from the queue.</summary>
        Delivered,
        /// <summary>Permanently rejected (non-retryable 4xx) — remove from the queue.</summary>
        Dropped,
        /// <summary>Transiently failed after exhausting retries — keep in the queue for later.</summary>
        Retry,
    }

    private const int MaxAttempts = 3;
    private static readonly TimeSpan MaxBackoff = TimeSpan.FromSeconds(30);
    private static readonly Random _rng = new();

    private readonly Dsn _dsn;
    private readonly SauronOptions _options;
    private readonly HttpClient _http;
    private readonly bool _ownsHttp;
    private readonly EnvelopeContext _context;
    private readonly BoundedQueue _queue;
    private readonly SemaphoreSlim _drainLock = new(1, 1);

    private readonly object _gate = new();
    private readonly List<object> _buffer = new();
    private readonly Timer _timer;

    private volatile bool _disabled;
    private volatile bool _disposed;

    public Transport(Dsn dsn, SauronOptions options, HttpClient http, bool ownsHttp)
    {
        _dsn = dsn;
        _options = options;
        _http = http;
        _ownsHttp = ownsHttp;
        _context = BuildContext();
        _queue = new BoundedQueue(options.MaxQueueBytes, options.OfflineDir);

        var interval = options.FlushInterval > TimeSpan.Zero ? options.FlushInterval : Timeout.InfiniteTimeSpan;
        _timer = new Timer(_ => OnTimer(), null, interval, interval);
    }

    public bool Disabled => _disabled;

    public void Enqueue(object item)
    {
        if (_disabled || _disposed)
            return;

        bool shouldFlush;
        lock (_gate)
        {
            _buffer.Add(item);
            shouldFlush = _buffer.Count >= Math.Max(1, _options.MaxBatch);
        }

        if (shouldFlush)
            _ = FlushAsync();
    }

    private void OnTimer()
    {
        try
        {
            _ = FlushAsync();
        }
        catch
        {
            // Timer callbacks must never throw.
        }
    }

    /// <summary>Build an envelope from any buffered items, enqueue it, and drain pending envelopes.</summary>
    public async Task FlushAsync()
    {
        if (_disabled)
            return;

        List<object>? batch = null;
        lock (_gate)
        {
            if (_buffer.Count > 0)
            {
                batch = new List<object>(_buffer);
                _buffer.Clear();
            }
        }

        if (batch is not null)
        {
            // Split into bounded envelopes. A single envelope carrying the whole
            // buffer could exceed the server's per-envelope item limit, which is a
            // non-retryable 400 — so a backlog built up during an outage would be
            // discarded wholesale on the first flush after recovery.
            int chunkSize = Math.Max(1, _options.MaxItemsPerEnvelope);
            for (int i = 0; i < batch.Count; i += chunkSize)
            {
                var chunk = batch.GetRange(i, Math.Min(chunkSize, batch.Count - i));
                try
                {
                    var envelope = BuildEnvelope(chunk);
                    string json = JsonSerializer.Serialize(envelope, SauronJson.Options);
                    _queue.Push(Encoding.UTF8.GetBytes(json));
                }
                catch (Exception ex)
                {
                    // Serialization runs over caller-supplied properties/tags/extra, so it is
                    // caller-fallible: a reference cycle or a throwing property getter raises
                    // here, and this is application code's call stack. Dropping the chunk is the
                    // only option — bytes that cannot be produced cannot be queued or retried —
                    // but it must not take the flush down with it, or one poisoned item would
                    // also strand every well-formed item behind it AND surface as an application
                    // failure. Chunk granularity (not per item) is deliberate: re-serializing
                    // item-by-item to isolate the culprit would pay the whole cost twice on a
                    // path that is already the unhappy one.
                    Log($"envelope build failed for {chunk.Count} item(s); dropping them: {Describe(ex)}");
                }
            }
        }

        await DrainQueueAsync().ConfigureAwait(false);
    }

    /// <summary>
    /// Deliver queued envelopes in FIFO order. A delivered or permanently-dropped envelope is
    /// acked (removed); on a transient failure we stop and keep the remaining envelopes so they
    /// survive the outage (and, with disk persistence, a restart).
    /// </summary>
    /// <remarks>
    /// This is the SDK's no-throw boundary. <see cref="FlushAsync"/> is called from application
    /// code and from <see cref="Dispose"/>, so nothing in here may propagate: an SDK that throws
    /// into the app it is observing has turned a telemetry failure into an application failure.
    /// The <c>try/finally</c> that used to be here had no <c>catch</c>, and not every await in the
    /// loop is covered by <c>SendAsync</c>'s per-attempt retry <c>try</c> — <c>Gzip.MaybeGzip</c>
    /// runs before it and <c>DelayAsync</c> (which may be a caller-supplied hook) runs after it —
    /// so a failure there escaped all the way out of <c>FlushAsync</c>.
    ///
    /// Failures are classified rather than lumped together, because "keep going" and "stop" have
    /// opposite costs here; see <see cref="IsDrainFatal"/>. Both paths log through
    /// <see cref="Log"/> and neither acks an envelope it did not deliver.
    /// </remarks>
    private async Task DrainQueueAsync()
    {
        try
        {
            await _drainLock.WaitAsync().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            // Taking the lock is itself fallible and needs its own guard, because it sits outside
            // the try/finally below: Dispose() disposes _drainLock, so a timer- or Enqueue-driven
            // flush racing a Close() lands an ObjectDisposedException right here. Nothing was
            // acquired, so there is nothing to release and the queue is untouched.
            Log($"drain skipped; could not acquire the drain lock: {Describe(ex)}");
            return;
        }

        try
        {
            foreach (var entry in _queue.Snapshot())
            {
                if (_disabled)
                    return;

                SendOutcome outcome;
                try
                {
                    outcome = await SendAsync(entry.Payload).ConfigureAwait(false);
                }
                catch (Exception ex) when (!IsDrainFatal(ex))
                {
                    // Envelope-local failure: something about THIS payload (or this attempt's
                    // out-of-retry-try steps) blew up. Leave it queued — it was never delivered,
                    // and acking it here would be exactly the silent data loss the queue exists
                    // to prevent — but keep draining, so one unsendable envelope cannot
                    // head-of-line block every envelope behind it. That is the same call made for
                    // 413 in SendAsync, and it is why the failed entry is skipped rather than
                    // `break`-ing the pass. It stays at the head of the queue, is retried on the
                    // next flush, and if it is permanently unsendable the queue's byte cap
                    // eventually evicts it (EnforceCap drops the oldest first) — so it cannot
                    // wedge the queue forever without any further bookkeeping here.
                    Log($"sending an envelope threw: {Describe(ex)}; keeping it queued and continuing.");
                    continue;
                }

                if (outcome == SendOutcome.Retry)
                    break; // preserve FIFO; retry this and later entries on the next flush

                _queue.Ack(entry);
            }
        }
        catch (Exception ex)
        {
            // Fatal to the whole pass (see IsDrainFatal), plus anything raised by the queue
            // snapshot/ack bookkeeping itself. Stop with the queue intact: nothing on this path
            // acks, so every undelivered envelope is still pending for the next flush.
            //
            // Swallowed rather than rethrown even for OutOfMemoryException: the no-throw
            // guarantee is unconditional, and the caller asked to flush telemetry, not to be
            // handed the SDK's internal failure. Log() is what keeps it from being invisible.
            Log($"drain aborted: {Describe(ex)}; {_queue.Count} envelope(s) kept for later.");
        }
        finally
        {
            try
            {
                _drainLock.Release();
            }
            catch (ObjectDisposedException)
            {
                // Dispose() ran while this pass was in flight. A throw from a finally would
                // replace the swallowed failure and escape after all, which is the one thing
                // this method must not do.
            }
        }
    }

    /// <summary>
    /// Whether a failure ends the whole drain pass instead of just the current envelope.
    /// Fatal means "the next envelope would hit this identically", so continuing would only
    /// multiply the damage (and the log noise) without delivering anything:
    /// <list type="bullet">
    /// <item><see cref="OperationCanceledException"/> (incl. <c>TaskCanceledException</c>) — the
    /// flush is being torn down. Note this does NOT capture per-request HTTP timeouts: those
    /// throw inside <c>SendAsync</c>'s retry <c>try</c> and stay classified as transient there.</item>
    /// <item><see cref="ObjectDisposedException"/> — a dependency (HttpClient, the drain lock) was
    /// disposed under us; every later send would fail the same way.</item>
    /// <item><see cref="OutOfMemoryException"/> — compressing/queueing the next envelope, which may
    /// be larger, can only make an exhausted process worse.</item>
    /// </list>
    /// Everything else — a malformed payload, a caller-supplied hook throwing, an unexpected
    /// library bug — is treated as envelope-local, because the envelope behind it probably still
    /// sends fine.
    /// </summary>
    private static bool IsDrainFatal(Exception ex)
        => ex is OperationCanceledException or ObjectDisposedException or OutOfMemoryException;

    private Envelope BuildEnvelope(List<object> batch) => new()
    {
        Header = new EnvelopeHeader
        {
            Dsn = _dsn.Raw,
            Sdk = new SdkInfo { Name = SauronSdkMeta.Name, Version = SauronSdkMeta.Version },
            SentAt = Iso8601Now(),
            Release = _options.Release,
        },
        Context = _context,
        Items = batch,
    };

    /// <summary>
    /// POST one serialized envelope, applying gzip (over the threshold) and the retry policy:
    /// retry on 408/413/429/5xx and network errors (honoring <c>Retry-After</c> on 429), drop on
    /// 400/401/403/404, up to <see cref="MaxAttempts"/> attempts with backoff capped at 30s.
    /// </summary>
    private async Task<SendOutcome> SendAsync(byte[] jsonBytes)
    {
        byte[] payload = Gzip.MaybeGzip(jsonBytes, _options.GzipThresholdBytes, out bool gzipped);

        for (int attempt = 1; attempt <= MaxAttempts; attempt++)
        {
            TimeSpan delay;
            try
            {
                using var request = new HttpRequestMessage(HttpMethod.Post, _dsn.EnvelopeUrl);
                var content = new ByteArrayContent(payload);
                content.Headers.ContentType = new MediaTypeHeaderValue("application/json") { CharSet = "utf-8" };
                if (gzipped)
                    content.Headers.ContentEncoding.Add("gzip");
                request.Content = content;
                request.Headers.TryAddWithoutValidation("X-Sauron-Key", _dsn.PublicKey);

                using var response = await _http.SendAsync(request).ConfigureAwait(false);

                if (response.IsSuccessStatusCode)
                    return SendOutcome.Delivered;

                int status = (int)response.StatusCode;

                if (status == 401 || status == 403)
                {
                    // Hard auth failure: disable and drop; never retry a bad key.
                    _disabled = true;
                    Log($"auth failure ({status}); disabling SDK.");
                    return SendOutcome.Dropped;
                }

                if (status == 413)
                {
                    // The envelope is already serialized, so there is nothing left to
                    // shrink here — and retrying the same bytes can only fail the same
                    // way. Retrying instead head-of-line blocked the whole FIFO queue
                    // forever, so drop this envelope and keep the rest moving.
                    // Envelopes are item-capped at build time, so this is now rare.
                    Log("envelope rejected as too large (413); dropping it.");
                    return SendOutcome.Dropped;
                }

                if (!IsRetryable(status))
                {
                    // Non-retryable client error (e.g. 400, 404): drop the envelope.
                    Log($"non-retryable status {status}; dropping envelope.");
                    return SendOutcome.Dropped;
                }

                if (attempt >= MaxAttempts)
                {
                    Log($"retries exhausted ({MaxAttempts}); last status {status}; keeping envelope for later.");
                    return SendOutcome.Retry;
                }

                delay = status == 429
                    ? RetryAfterDelay(response) ?? Backoff(attempt)
                    : Backoff(attempt);
            }
            catch (Exception ex)
            {
                // Network / transport error: retryable.
                if (attempt >= MaxAttempts)
                {
                    Log($"send failed after {MaxAttempts} attempts: {ex.Message}; keeping envelope for later.");
                    return SendOutcome.Retry;
                }
                delay = Backoff(attempt);
            }

            await DelayAsync(delay).ConfigureAwait(false);
        }

        return SendOutcome.Retry;
    }

    /// <summary>
    /// Transient statuses worth retrying: request timeout, rate-limit, and all 5xx.
    /// 413 is excluded deliberately — see the explicit handling in <c>SendAsync</c>.
    /// </summary>
    private static bool IsRetryable(int status)
        => status == 408 || status == 429 || status >= 500;

    /// <summary>Parse a <c>Retry-After</c> header (delta seconds or HTTP-date), clamped to [0, 30s].</summary>
    private static TimeSpan? RetryAfterDelay(HttpResponseMessage response)
    {
        var ra = response.Headers.RetryAfter;
        if (ra is null)
            return null;

        TimeSpan delay;
        if (ra.Delta is TimeSpan d)
            delay = d;
        else if (ra.Date is DateTimeOffset date)
            delay = date - DateTimeOffset.UtcNow;
        else
            return null;

        if (delay < TimeSpan.Zero) delay = TimeSpan.Zero;
        if (delay > MaxBackoff) delay = MaxBackoff;
        return delay;
    }

    /// <summary>Exponential backoff with full jitter, capped at 30s: base = 100ms * 2^(attempt-1).</summary>
    private static TimeSpan Backoff(int attempt)
    {
        double baseMs = 100.0 * Math.Pow(2, attempt - 1);
        double jitter;
        lock (_rng) { jitter = _rng.NextDouble() * baseMs; }
        double ms = Math.Min(baseMs + jitter, MaxBackoff.TotalMilliseconds);
        return TimeSpan.FromMilliseconds(ms);
    }

    private Task DelayAsync(TimeSpan delay)
        => _options.DelayHook is { } hook ? hook(delay) : Task.Delay(delay);

    private EnvelopeContext BuildContext()
    {
        return new EnvelopeContext
        {
            Device = new DeviceInfo { DeviceId = Guid.NewGuid().ToString() },
            Os = new OsInfo { Name = DetectOs(), Version = null },
            App = new Dictionary<string, object?>(),
            Runtime = new RuntimeInfo { Name = "dotnet", Version = Environment.Version.ToString() },
            User = null,
        };
    }

    private static string DetectOs()
    {
        if (RuntimeInformation.IsOSPlatform(OSPlatform.Linux)) return "linux";
        if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows)) return "windows";
        if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX)) return "macos";
        return "unknown";
    }

    internal static string Iso8601Now() => DateTimeOffset.UtcNow.ToString("O");

    private void Log(string message)
    {
        if (_options.Debug)
            Console.Error.WriteLine($"[sauron] {message}");
    }

    /// <summary>
    /// Format an unexpected exception for the debug log. Includes the type name, unlike the
    /// expected-outcome messages above: these are bugs or environment failures, where the type is
    /// half the diagnosis and the message is sometimes empty.
    /// </summary>
    private static string Describe(Exception ex) => $"{ex.GetType().Name}: {ex.Message}";

    public void Dispose()
    {
        if (_disposed)
            return;
        _disposed = true;

        _timer.Dispose();
        try
        {
            FlushAsync().GetAwaiter().GetResult();
        }
        catch
        {
            // best-effort flush on close
        }

        // _drainLock is deliberately NOT disposed.
        //
        // SemaphoreSlim.Dispose() ABANDONS already-queued async waiters — it clears the wait
        // queue without completing or faulting them — so a timer- or Enqueue-driven flush that
        // is already awaiting WaitAsync() when Close() runs never resumes. DrainQueueAsync's
        // catch below cannot help: an abandoned waiter throws nothing, it simply never
        // completes, and the caller's `await FlushAsync()` hangs forever. That is strictly
        // worse than the escaping exception the guard was added to prevent.
        //
        // Not disposing costs nothing: SemaphoreSlim only holds a disposable resource once
        // AvailableWaitHandle has been read, and this type never reads it.

        if (_ownsHttp)
            _http.Dispose();
    }
}
