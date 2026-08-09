using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Xunit;

namespace Sauron.Tests;

/// <summary>
/// A handler that records every request body (decoded, in order) and replies with a scripted
/// status sequence, defaulting to 200 once the script runs out. Unlike <see cref="CapturingHandler"/>
/// it keeps ALL bodies, which is what identifies WHICH queued envelope a given request carried.
/// </summary>
internal sealed class RecordingHandler : HttpMessageHandler
{
    private readonly Queue<HttpStatusCode> _statuses;

    public RecordingHandler(params HttpStatusCode[] statuses)
        => _statuses = new Queue<HttpStatusCode>(statuses);

    /// <summary>Decoded request bodies, oldest first.</summary>
    public List<string> Bodies { get; } = new();

    public int RequestCount => Bodies.Count;

    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        var bytes = request.Content is null
            ? Array.Empty<byte>()
            : await request.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
        Bodies.Add(Encoding.UTF8.GetString(bytes));
        return new HttpResponseMessage(_statuses.Count > 0 ? _statuses.Dequeue() : HttpStatusCode.OK);
    }

    /// <summary>The event names carried by each recorded request, oldest first.</summary>
    public List<string> EventNames()
    {
        var names = new List<string>();
        foreach (var body in Bodies)
        {
            using var doc = JsonDocument.Parse(body);
            names.Add(doc.RootElement.GetProperty("items")[0].GetProperty("name").GetString() ?? "");
        }
        return names;
    }
}

/// <summary>
/// The SDK's no-throw guarantee at the transport boundary: nothing that goes wrong while draining
/// the pending-envelope queue may surface from <c>FlushAsync</c>, because that would turn a
/// telemetry failure into an application failure in the host being observed.
///
/// The throw is injected through <see cref="SauronOptions.DelayHook"/> — the retry-backoff sleep.
/// That is the one awaited call inside the drain loop that a test can reach, and it sits in
/// exactly the same unguarded position as the real-world culprit: <c>Gzip.MaybeGzip</c>, which
/// runs at the top of <c>SendAsync</c>, OUTSIDE the per-attempt retry <c>try</c>. Both propagate
/// through <c>SendAsync</c> into <c>DrainQueueAsync</c>, which has a <c>finally</c> but no
/// <c>catch</c>.
/// </summary>
public class NoThrowGuaranteeTests
{
    /// <summary>
    /// One envelope per item plus a parked timer, so a single Flush pushes N envelopes and drains
    /// them in one pass; gzip disabled so recorded bodies are readable JSON.
    /// </summary>
    private static SauronOptions Options(RecordingHandler handler) => new()
    {
        Dsn = "https://pub123@example.com/42",
        HttpMessageHandler = handler,
        FlushInterval = TimeSpan.FromHours(1),
        MaxBatch = 1000,
        MaxItemsPerEnvelope = 1,
        GzipThresholdBytes = int.MaxValue,
    };

    /// <summary>
    /// A throw from inside the drain must not reach the caller, and the envelope it was carrying
    /// must still be queued afterwards — a failed send is not a delivery.
    /// </summary>
    [Fact]
    public void ThrowingSend_DoesNotSurfaceFromFlush_AndEnvelopeSurvives()
    {
        var handler = new RecordingHandler(HttpStatusCode.InternalServerError);
        var options = Options(handler);
        options.DelayHook = _ => throw new InvalidOperationException("boom inside the drain");
        using var client = new SauronClient(options);

        client.Track("a", "u1");
        client.Flush(); // must not throw

        Assert.Equal(1, handler.RequestCount);

        // The envelope was never delivered, so it must still be queued: with the throw removed
        // the very next flush re-sends it.
        options.DelayHook = _ => Task.CompletedTask;
        client.Flush();

        Assert.Equal(new List<string> { "a", "a" }, handler.EventNames());
    }

    /// <summary>
    /// An envelope-local failure must not head-of-line block the queue: the drain keeps going and
    /// delivers the envelopes behind it, while the failed one stays queued for the next flush.
    /// </summary>
    [Fact]
    public void EnvelopeLocalThrow_KeepsDraining_AndDoesNotAckTheFailedEnvelope()
    {
        var handler = new RecordingHandler(HttpStatusCode.InternalServerError);
        var options = Options(handler);
        int hookCalls = 0;
        options.DelayHook = _ =>
        {
            hookCalls++;
            if (hookCalls == 1)
                throw new InvalidOperationException("boom on the first envelope only");
            return Task.CompletedTask;
        };
        using var client = new SauronClient(options);

        client.Track("a", "u1");
        client.Track("b", "u1");
        client.Flush(); // must not throw

        // "a" threw; "b" was still attempted and accepted in the same pass.
        Assert.Equal(new List<string> { "a", "b" }, handler.EventNames());

        // "a" was not acked, so it is redelivered; "b" was, so it is not sent again.
        client.Flush();
        Assert.Equal(new List<string> { "a", "b", "a" }, handler.EventNames());
    }

    /// <summary>
    /// A failure that dooms the whole pass (cancellation / a disposed dependency) must stop the
    /// drain instead of hammering every remaining envelope with the same doomed call — and must
    /// leave the entire queue intact, in FIFO order, for the next flush.
    /// </summary>
    [Fact]
    public void DrainFatalThrow_StopsThePass_AndKeepsWholeQueue()
    {
        var handler = new RecordingHandler(HttpStatusCode.InternalServerError);
        var options = Options(handler);
        int hookCalls = 0;
        options.DelayHook = _ =>
        {
            hookCalls++;
            if (hookCalls == 1)
                throw new OperationCanceledException("drain cancelled");
            return Task.CompletedTask;
        };
        using var client = new SauronClient(options);

        client.Track("a", "u1");
        client.Track("b", "u1");
        client.Flush(); // must not throw

        // Stopped at "a": "b" was never attempted on this pass.
        Assert.Equal(new List<string> { "a" }, handler.EventNames());

        options.DelayHook = _ => Task.CompletedTask;
        client.Flush();
        Assert.Equal(new List<string> { "a", "a", "b" }, handler.EventNames());
    }

    /// <summary>
    /// The same hole with no test hook involved at all: <c>Close()</c> disposes the drain lock, so
    /// a later flush — an app that flushes on a background timer or from a shutdown hook it does
    /// not sequence against Close, or simply calls Flush twice — used to get an
    /// <see cref="ObjectDisposedException"/> from <c>SemaphoreSlim.WaitAsync</c> straight out of
    /// <c>FlushAsync</c>. Flushing a closed client must be an inert no-op.
    /// </summary>
    [Fact]
    public void FlushAfterClose_IsInert_AndDoesNotThrow()
    {
        var handler = new RecordingHandler();
        var client = new SauronClient(Options(handler));

        client.Track("a", "u1");
        client.Close(); // flushes "a", then tears the transport down

        client.Track("b", "u1");
        client.Flush(); // must not throw

        Assert.Equal(new List<string> { "a" }, handler.EventNames());
    }

    /// <summary>
    /// An item the serializer cannot handle (here: a reference cycle in caller-supplied
    /// properties) is built OUTSIDE the drain, in <c>FlushAsync</c> itself. It must not escape
    /// either, and it must not take out the flush: items buffered alongside it still ship.
    /// </summary>
    [Fact]
    public void UnserializableItem_DoesNotSurfaceFromFlush_AndLaterItemsStillShip()
    {
        var handler = new RecordingHandler();
        var options = Options(handler);
        using var client = new SauronClient(options);

        var cycle = new Node();
        cycle.Self = cycle; // System.Text.Json has no ReferenceHandler configured -> throws

        client.Track("poison", "u1", new Dictionary<string, object?> { ["node"] = cycle });
        client.Track("good", "u1");
        client.Flush(); // must not throw

        Assert.Equal(new List<string> { "good" }, handler.EventNames());
    }

    private sealed class Node
    {
        public Node? Self { get; set; }
    }

    /// <summary>
    /// A flush racing a close completes and does not throw.
    ///
    /// **What this does NOT cover, stated because the honest scope is narrower than the name
    /// would suggest.** It does not reproduce the abandoned-waiter hang.
    /// <c>SemaphoreSlim.Dispose()</c> clears already-queued async waiters without completing or
    /// faulting them, so a waiter queued at that instant never resumes — but
    /// <c>Transport.Dispose()</c> runs <c>FlushAsync().GetAwaiter().GetResult()</c> FIRST, and
    /// that flush serializes on the same lock, so any waiter behind it is released before the
    /// disposal runs. The window is the few instructions between that flush returning and the
    /// disposal executing, reachable only by a timer- or Enqueue-driven flush landing inside it.
    /// A mutation run confirmed this test passes with <c>_drainLock.Dispose()</c> restored.
    ///
    /// The fix — not disposing the semaphore at all — is therefore justified by reasoning rather
    /// than by this test: <c>SemaphoreSlim</c> holds a disposable resource only once
    /// <c>AvailableWaitHandle</c> has been read, this type never reads it, so the call was
    /// unnecessary and its only possible effect was to abandon waiters. Removing an unnecessary
    /// operation whose sole failure mode is an unrecoverable hang needs no test to justify; what
    /// needed saying is that this test is not that justification.
    /// </summary>
    [Fact]
    public async Task AFlushRacingACloseCompletesAndDoesNotThrow()
    {
        var handler = new RecordingHandler();
        var options = Options(handler);
        // Hold the drain lock open long enough for Dispose() to land mid-await.
        var release = new TaskCompletionSource();
        options.DelayHook = _ => release.Task;
        // A 500 forces the retry path, which is what awaits DelayHook.
        var failing = new RecordingHandler(HttpStatusCode.InternalServerError);
        options.HttpMessageHandler = failing;

        var client = new SauronClient(options);
        client.Track("evt", "u1");
        var draining = client.FlushAsync();
        // Let the flush take the lock and reach the backoff await.
        await Task.Delay(100);

        var closing = Task.Run(() => client.Dispose());
        await Task.Delay(50);
        release.SetResult();

        var both = Task.WhenAll(draining, closing);
        var finished = await Task.WhenAny(both, Task.Delay(TimeSpan.FromSeconds(10)));
        Assert.Same(both, finished);
        // ...and neither surfaced an exception, which is the original guarantee.
        await both;
    }
}
