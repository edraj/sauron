using System;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.Http;
using System.IO;
using System.IO.Compression;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Xunit;

namespace Sauron.Tests;

/// <summary>
/// An envelope carrying more than 1000 items is a non-retryable 400 server-side,
/// which drops the batch. <c>MaxBatch</c> only triggers a flush, so without a
/// separate ceiling a backlog built during an outage would be discarded whole.
/// </summary>
public class EnvelopeItemCapTests
{
    /// <summary>Counts requests and the items each one carried.</summary>
    private sealed class CountingHandler : HttpMessageHandler
    {
        public List<int> Sizes { get; } = new();

        protected override async Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var bytes = request.Content is null
                ? Array.Empty<byte>()
                : await request.Content.ReadAsByteArrayAsync(cancellationToken);
            // The transport gzips larger bodies; decompress before parsing.
            if (bytes.Length > 2 && bytes[0] == 0x1f && bytes[1] == 0x8b)
            {
                using var src = new MemoryStream(bytes);
                using var gz = new GZipStream(src, CompressionMode.Decompress);
                using var dst = new MemoryStream();
                await gz.CopyToAsync(dst, cancellationToken);
                bytes = dst.ToArray();
            }
            using var doc = JsonDocument.Parse(bytes);
            Sizes.Add(doc.RootElement.GetProperty("items").GetArrayLength());
            return new HttpResponseMessage(HttpStatusCode.OK);
        }
    }

    [Fact]
    public void BacklogLargerThanCap_IsSplitAcrossEnvelopes()
    {
        var handler = new CountingHandler();
        var client = new SauronClient(new SauronOptions
        {
            Dsn = "https://pub123@example.com/42",
            HttpMessageHandler = handler,
            FlushInterval = TimeSpan.FromHours(1),
            MaxBatch = 100_000,          // never auto-flushes during the loop
            MaxItemsPerEnvelope = 250,
        });

        for (int i = 0; i < 900; i++)
            client.Track($"e{i}", "u1");
        client.Flush();

        Assert.NotEmpty(handler.Sizes);
        Assert.All(handler.Sizes, n => Assert.True(n <= 250, $"envelope carried {n} items"));
        Assert.Equal(900, handler.Sizes.Sum());
    }
}
