using System.IO;
using System.IO.Compression;
using System.Linq;
using System.Net;
using System.Net.Http;
using System.Text;
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

    /// <summary>
    /// The last request body as JSON text — always the logical payload, transparently
    /// gunzipped when the request was gzip-encoded.
    /// </summary>
    public string? LastBody { get; private set; }

    /// <summary>The raw (possibly gzipped) bytes of the last request body, exactly as sent.</summary>
    public byte[]? LastBodyBytes { get; private set; }

    /// <summary>The <c>Content-Encoding</c> of the last request (e.g. <c>gzip</c>), or null.</summary>
    public string? LastContentEncoding { get; private set; }

    public int RequestCount { get; private set; }
    public HttpStatusCode ResponseStatus { get; set; } = HttpStatusCode.OK;

    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        RequestCount++;
        LastRequest = request;
        if (request.Content is null)
        {
            LastBodyBytes = null;
            LastBody = null;
            LastContentEncoding = null;
        }
        else
        {
            LastBodyBytes = await request.Content.ReadAsByteArrayAsync().ConfigureAwait(false);
            LastContentEncoding = request.Content.Headers.ContentEncoding.FirstOrDefault();
            var decoded = LastContentEncoding == "gzip" ? Gunzip(LastBodyBytes) : LastBodyBytes;
            LastBody = Encoding.UTF8.GetString(decoded);
        }

        return new HttpResponseMessage(ResponseStatus);
    }

    private static byte[] Gunzip(byte[] gz)
    {
        using var input = new MemoryStream(gz);
        using var gzip = new GZipStream(input, CompressionMode.Decompress);
        using var output = new MemoryStream();
        gzip.CopyTo(output);
        return output.ToArray();
    }
}
