using System.Net;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;

namespace Sauron.Tests;

/// <summary>
/// A fake <see cref="HttpMessageHandler"/> that records the last request (URL, headers, body)
/// and returns a canned response. Ensures tests never hit the network.
/// </summary>
internal sealed class CapturingHandler : HttpMessageHandler
{
    public HttpRequestMessage? LastRequest { get; private set; }
    public string? LastBody { get; private set; }
    public int RequestCount { get; private set; }
    public HttpStatusCode ResponseStatus { get; set; } = HttpStatusCode.OK;

    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        RequestCount++;
        LastRequest = request;
        LastBody = request.Content is null
            ? null
            : await request.Content.ReadAsStringAsync().ConfigureAwait(false);

        return new HttpResponseMessage(ResponseStatus);
    }
}
