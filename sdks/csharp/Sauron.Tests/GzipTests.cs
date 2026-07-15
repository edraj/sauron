using System.Collections.Generic;
using System.IO;
using System.IO.Compression;
using System.Text;
using System.Text.Json;
using Xunit;

namespace Sauron.Tests;

public class GzipTests
{
    private static byte[] Decompress(byte[] gz)
    {
        using var input = new MemoryStream(gz);
        using var gzip = new GZipStream(input, CompressionMode.Decompress);
        using var output = new MemoryStream();
        gzip.CopyTo(output);
        return output.ToArray();
    }

    [Fact]
    public void MaybeGzip_AboveThreshold_CompressesAndRoundTrips()
    {
        var body = Encoding.UTF8.GetBytes(new string('a', 4096));
        var outBytes = Gzip.MaybeGzip(body, 1024, out bool gzipped);

        Assert.True(gzipped);
        Assert.True(outBytes.Length < body.Length); // highly compressible input shrinks
        Assert.Equal(body, Decompress(outBytes));
    }

    [Fact]
    public void MaybeGzip_BelowThreshold_PassesThrough()
    {
        var body = Encoding.UTF8.GetBytes("small");
        var outBytes = Gzip.MaybeGzip(body, 1024, out bool gzipped);

        Assert.False(gzipped);
        Assert.Same(body, outBytes); // exact passthrough, no allocation
    }

    [Fact]
    public void Transport_LargeBody_GzipsAndSetsContentEncoding()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions { GzipThresholdBytes = 64 });

        var blob = new string('x', 500);
        client.Track("evt", "u1", new Dictionary<string, object?> { ["blob"] = blob });
        client.Flush();

        Assert.Equal("gzip", handler.LastContentEncoding);
        Assert.NotNull(handler.LastBodyBytes);
        // Raw wire bytes carry the gzip magic header (0x1f 0x8b).
        Assert.Equal(0x1f, handler.LastBodyBytes![0]);
        Assert.Equal(0x8b, handler.LastBodyBytes[1]);
        // The raw bytes round-trip to the original JSON envelope.
        var json = Encoding.UTF8.GetString(Decompress(handler.LastBodyBytes!));
        using var doc = JsonDocument.Parse(json);
        Assert.Equal("evt", doc.RootElement.GetProperty("items")[0].GetProperty("name").GetString());
    }

    [Fact]
    public void Transport_SmallBody_NotGzipped()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler); // default threshold 1024

        client.Track("evt", "u1");
        client.Flush();

        Assert.True(string.IsNullOrEmpty(handler.LastContentEncoding));
        Assert.NotNull(handler.LastBody); // decoded JSON is available
        Assert.Equal("application/json", handler.LastRequest!.Content!.Headers.ContentType!.MediaType);
    }
}
