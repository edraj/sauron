using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Http;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace Sauron;

/// <summary>
/// Buffered background HTTP transport. Items are queued in memory and flushed
/// either on a timer (<c>flush_interval</c>), when <c>max_batch</c> is reached,
/// or explicitly via <see cref="FlushAsync"/>. One envelope is built per flush.
/// </summary>
internal sealed class Transport : IDisposable
{
    private readonly Dsn _dsn;
    private readonly SauronOptions _options;
    private readonly HttpClient _http;
    private readonly bool _ownsHttp;
    private readonly EnvelopeContext _context;

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

    /// <summary>Flush all buffered items immediately as a single envelope POST.</summary>
    public async Task FlushAsync()
    {
        if (_disabled)
            return;

        List<object> batch;
        lock (_gate)
        {
            if (_buffer.Count == 0)
                return;
            batch = new List<object>(_buffer);
            _buffer.Clear();
        }

        var envelope = new Envelope
        {
            Header = new EnvelopeHeader
            {
                Dsn = _dsn.Raw,
                Sdk = new SdkInfo { Name = "sauron-dotnet", Version = "0.1.0" },
                SentAt = Iso8601Now(),
                Environment = _options.Environment,
                Release = _options.Release,
            },
            Context = _context,
            Items = batch,
        };

        string json = JsonSerializer.Serialize(envelope, SauronJson.Options);
        await SendAsync(json, batch).ConfigureAwait(false);
    }

    private async Task SendAsync(string json, List<object> batch)
    {
        const int maxAttempts = 3;
        for (int attempt = 1; attempt <= maxAttempts; attempt++)
        {
            try
            {
                using var request = new HttpRequestMessage(HttpMethod.Post, _dsn.EnvelopeUrl)
                {
                    Content = new StringContent(json, Encoding.UTF8, "application/json"),
                };
                request.Headers.TryAddWithoutValidation("X-Sauron-Key", _dsn.PublicKey);

                using var response = await _http.SendAsync(request).ConfigureAwait(false);

                if (response.IsSuccessStatusCode)
                    return;

                var status = (int)response.StatusCode;
                if (status == 401 || status == 403)
                {
                    // Hard auth failure: disable and stop; do not retry forever.
                    _disabled = true;
                    Log($"auth failure ({status}); disabling SDK.");
                    return;
                }

                if (status == 429 || status >= 500)
                {
                    if (attempt < maxAttempts)
                    {
                        await Task.Delay(BackoffMs(attempt)).ConfigureAwait(false);
                        continue;
                    }
                    Log($"dropping {batch.Count} item(s) after {maxAttempts} attempts (last status {status}).");
                    return;
                }

                // Other 4xx: not retryable; drop.
                Log($"non-retryable status {status}; dropping {batch.Count} item(s).");
                return;
            }
            catch (Exception ex)
            {
                if (attempt < maxAttempts)
                {
                    await Task.Delay(BackoffMs(attempt)).ConfigureAwait(false);
                    continue;
                }
                Log($"send failed after {maxAttempts} attempts: {ex.Message}");
                return;
            }
        }
    }

    private static int BackoffMs(int attempt) => 100 * (int)Math.Pow(2, attempt - 1);

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

        if (_ownsHttp)
            _http.Dispose();
    }
}
