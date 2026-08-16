using System.Collections.Generic;
using Xunit;

namespace Sauron.Tests;

/// <summary>
/// The cap on a transaction's <c>extra</c>.
/// </summary>
/// <remarks>
/// Worth its own suite because the failure it prevents is invisible from the
/// outside: transactions ship in BATCHED envelopes, and ingest rejects the whole
/// envelope past <c>INGEST_MAX_BODY_BYTES</c>. One oversized response body does not lose
/// one span — it loses every unrelated span batched alongside it, with a 400 the
/// transport drops without retrying.
/// </remarks>
public class TransactionExtraTests
{
    [Fact]
    public void SmallPayloadPassesThrough()
    {
        var extra = new Dictionary<string, object?> { ["request"] = "{\"page\":1}" };
        var capped = TransactionExtra.Cap(extra);
        Assert.Equal("{\"page\":1}", capped["request"]);
        Assert.False(capped.ContainsKey("_truncated"));
    }

    [Fact]
    public void OversizedPayloadBecomesAMarker()
    {
        var extra = new Dictionary<string, object?>
        {
            ["response"] = new string('x', TransactionExtra.MaxBytes + 1),
        };
        var capped = TransactionExtra.Cap(extra);
        Assert.True((bool)capped["_truncated"]!);
        Assert.True((int)capped["_bytes"]! > TransactionExtra.MaxBytes);
        // The whole map goes, not just the offending key.
        Assert.False(capped.ContainsKey("response"));
    }

    [Fact]
    public void MeasuresUtf8BytesNotCharacters()
    {
        // Under the cap by char count, over it by bytes. Measured wrong, the
        // envelope is ~2x the size the SDK believed it was sending.
        var extra = new Dictionary<string, object?>
        {
            ["body"] = new string('é', TransactionExtra.MaxBytes - 100),
        };
        Assert.True((bool)TransactionExtra.Cap(extra)["_truncated"]!);
    }

    [Fact]
    public void LimitMatchesEveryOtherSdk()
    {
        Assert.Equal(16 * 1024, TransactionExtra.MaxBytes);
    }

    [Fact]
    public void CapCopiesRatherThanAliasingTheCallerMap()
    {
        // The item is QUEUED, not sent inline, so a caller mutating their own
        // dictionary after the call would otherwise change what ships.
        var extra = new Dictionary<string, object?> { ["tier"] = "free" };
        var capped = TransactionExtra.Cap(extra);
        extra["tier"] = "premium";
        Assert.Equal("free", capped["tier"]);
    }
}
