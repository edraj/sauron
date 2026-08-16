using System;
using System.Collections.Generic;
using System.Text;
using System.Text.Json;

namespace Sauron;

/// <summary>
/// The size cap on a transaction's developer-supplied <c>extra</c>.
/// </summary>
/// <remarks>
/// Its own type rather than a private helper inside <see cref="SauronClient"/> so the
/// limit and its behaviour are directly testable — the failure this guards against (an
/// oversized payload taking a whole batched envelope down with it) is not visible from
/// the outside once it happens.
/// </remarks>
public static class TransactionExtra
{
    /// <summary>
    /// Largest serialized <c>extra</c> a single transaction may carry, in bytes.
    /// </summary>
    /// <remarks>
    /// Transactions are the highest-volume signal and they ship in BATCHED envelopes, so
    /// one oversized payload does not fail alone — ingest rejects the whole envelope past
    /// <c>INGEST_MAX_BODY_BYTES</c> (1 MiB by default) and every unrelated span batched
    /// with it is lost. Since the motivating use of transaction <c>extra</c> is request
    /// and response bodies, that is not a remote hazard.
    ///
    /// Kept identical across all five SDKs. If it moves, it moves everywhere.
    /// </remarks>
    public const int MaxBytes = 16 * 1024;

    /// <summary>
    /// Cap a transaction's <c>extra</c>, substituting a marker when it is too large.
    /// </summary>
    /// <remarks>
    /// Replaces the WHOLE map rather than trimming keys: a half-written JSON value is
    /// worse than an honest marker, and per-key trimming would make the result depend on
    /// key iteration order, which differs across the five SDKs. The marker is
    /// deliberately readable on the dashboard — <c>_truncated</c> says data was dropped
    /// rather than silently serving a short object that looks complete.
    ///
    /// A value that cannot be serialized at all (a cycle, an unsupported type) becomes
    /// the same marker with <c>_bytes: -1</c>, because the alternative is throwing from
    /// inside <c>TrackTransaction</c> — and an SDK that crashes the app it is measuring
    /// is worse than one that drops a payload.
    /// </remarks>
    internal static Dictionary<string, object?> Cap(
        IReadOnlyDictionary<string, object?> extra,
        int maxBytes = MaxBytes)
    {
        int bytes;
        try
        {
            // Measured through the SAME serializer the transport uses, so the number
            // here is the number the wire will cost. Encoding to UTF-8 bytes rather
            // than counting chars: the char count undercounts every non-ASCII byte,
            // which is exactly what a response body full of user text is made of.
            var json = JsonSerializer.Serialize(extra);
            bytes = Encoding.UTF8.GetByteCount(json);
        }
        catch (Exception)
        {
            return Truncated(-1);
        }

        if (bytes <= maxBytes)
        {
            return new Dictionary<string, object?>(
                (IDictionary<string, object?>)extra);
        }

        return Truncated(bytes);
    }

    private static Dictionary<string, object?> Truncated(int bytes) =>
        new()
        {
            ["_truncated"] = true,
            ["_bytes"] = bytes,
        };
}
