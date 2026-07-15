using System.IO;
using System.IO.Compression;

namespace Sauron;

/// <summary>
/// Request-body compression. The ingest accepts <c>Content-Encoding: gzip</c>; the SDK
/// compresses the envelope only when it is large enough to be worth it.
/// </summary>
internal static class Gzip
{
    /// <summary>
    /// Gzip <paramref name="body"/> when its length exceeds <paramref name="threshold"/> bytes.
    /// Below (or at) the threshold the original array is returned unchanged (<paramref name="gzipped"/>
    /// = <c>false</c>) so small payloads pay no compression cost.
    /// </summary>
    public static byte[] MaybeGzip(byte[] body, int threshold, out bool gzipped)
    {
        if (body is null || threshold < 0 || body.Length <= threshold)
        {
            gzipped = false;
            return body;
        }

        using var output = new MemoryStream();
        using (var gz = new GZipStream(output, CompressionLevel.Optimal, leaveOpen: true))
        {
            gz.Write(body, 0, body.Length);
        }

        gzipped = true;
        return output.ToArray();
    }
}
