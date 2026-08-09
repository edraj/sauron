using System;
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
        // `body` is declared non-nullable, so returning it from inside a `body is null` guard was
        // a provable null escape (CS8603). Throwing on the argument that is actually wrong is the
        // honest fix.
        //
        // This is NOT merely relocating the same exception one frame later, as an earlier version
        // of this comment claimed. The old path returned null, `new ByteArrayContent(null)` threw
        // INSIDE Transport.SendAsync's retry try-block, and the blanket `catch (Exception)` there
        // classified it as a transient send failure. This throw happens at the MaybeGzip call
        // site, which sits OUTSIDE that try — and `DrainQueueAsync` has a `finally` but no
        // `catch`, so it would propagate out of FlushAsync into the host application, breaking the
        // SDK's no-throw guarantee.
        //
        // That path is unreachable today: the only caller passes `Encoding.UTF8.GetBytes(json)`,
        // which never returns null. Written down rather than papered over, because "unreachable"
        // is a property of today's callers, not of this method.
        if (body is null)
            throw new ArgumentNullException(nameof(body));

        if (threshold < 0 || body.Length <= threshold)
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
