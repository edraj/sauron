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
            var envelope = BuildEnvelope(batch);
            string json = JsonSerializer.Serialize(envelope, SauronJson.Options);
            _queue.Push(Encoding.UTF8.GetBytes(json));
        }

        await DrainQueueAsync().ConfigureAwait(false);
    }

    /// <summary>
    /// Deliver queued envelopes in FIFO order. A delivered or permanently-dropped envelope is
    /// acked (removed); on a transient failure we stop and keep the remaining envelopes so they
    /// survive the outage (and, with disk persistence, a restart).
    /// </summary>
    private async Task DrainQueueAsync()
    {
        await _drainLock.WaitAsync().ConfigureAwait(false);
        try
        {
            foreach (var entry in _queue.Snapshot())
            {
                if (_disabled)
                    return;

                var outcome = await SendAsync(entry.Payload).ConfigureAwait(false);
                if (outcome == SendOutcome.Retry)
                    break; // preserve FIFO; retry this and later entries on the next flush

                _queue.Ack(entry);
            }
        }
        finally
        {
            _drainLock.Release();
        }
    }

    private Envelope BuildEnvelope(List<object> batch) => new()
    {
        Header = new EnvelopeHeader
        {
            Dsn = _dsn.Raw,
            Sdk = new SdkInfo { Name = SauronSdkMeta.Name, Version = SauronSdkMeta.Version },
            SentAt = Iso8601Now(),
            Environment = _options.Environment,
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

    /// <summary>Transient statuses worth retrying: request timeout, payload-too-large, rate-limit, and all 5xx.</summary>
    private static bool IsRetryable(int status)
        => status == 408 || status == 413 || status == 429 || status >= 500;

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

        _drainLock.Dispose();

        if (_ownsHttp)
            _http.Dispose();
    }
}
