using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;

namespace Sauron.Tests;

/// <summary>
/// A fake handler that plays back a scripted sequence of responses (each step returns a
/// response or throws to model a network error). Records how many requests were made.
/// Once the script is exhausted it returns <c>200 OK</c>.
/// </summary>
internal sealed class ScriptedHandler : HttpMessageHandler
{
    private readonly Queue<Func<HttpResponseMessage>> _steps;

    public int RequestCount { get; private set; }

    public ScriptedHandler(params Func<HttpResponseMessage>[] steps)
        => _steps = new Queue<Func<HttpResponseMessage>>(steps);

    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        RequestCount++;
        await Task.Yield();
        if (_steps.Count == 0)
            return new HttpResponseMessage(HttpStatusCode.OK);
        return _steps.Dequeue()(); // may throw -> surfaces as a faulted task (network error)
    }

    public static Func<HttpResponseMessage> Ok() => () => new HttpResponseMessage(HttpStatusCode.OK);

    public static Func<HttpResponseMessage> Status(int code) => () => new HttpResponseMessage((HttpStatusCode)code);

    public static Func<HttpResponseMessage> RetryAfter(int seconds) => () =>
    {
        var r = new HttpResponseMessage(HttpStatusCode.TooManyRequests);
        r.Headers.Add("Retry-After", seconds.ToString());
        return r;
    };

    public static Func<HttpResponseMessage> Boom() => () => throw new HttpRequestException("network down");
}
