using System;
using Xunit;

namespace Sauron.Tests;

public class InitTests
{
    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    public void MissingRelease_Throws(string? release)
    {
        var ex = Assert.Throws<ArgumentException>(() => new SauronClient(new SauronOptions
        {
            Dsn = "https://pk_test@localhost:8081/1",
            Release = release,
            HttpMessageHandler = new CapturingHandler(),
        }));
        Assert.Contains("Release", ex.Message);
    }

    [Fact]
    public void InvalidDsn_StillDisablesWithoutThrowing()
    {
        using var client = new SauronClient(new SauronOptions { Dsn = "not-a-dsn", Release = "1.0.0", HttpMessageHandler = new CapturingHandler() });
        Assert.False(client.Enabled);
    }

    [Fact]
    public void InvalidDsn_MissingRelease_Throws()
    {
        // A present-but-invalid DSN still demands a Release, matching Flutter's `isConfigured`.
        var ex = Assert.Throws<ArgumentException>(() => new SauronClient(new SauronOptions
        {
            Dsn = "not-a-dsn",
            HttpMessageHandler = new CapturingHandler(),
        }));
        Assert.Contains("Release", ex.Message);
    }

    [Fact]
    public void EmptyDsn_MissingRelease_StaysDisabledWithoutThrowing()
    {
        using var client = new SauronClient(new SauronOptions { Dsn = "", HttpMessageHandler = new CapturingHandler() });
        Assert.False(client.Enabled);
    }

    /// <summary>
    /// The two-argument facade overload added in 1.6.0 alongside the
    /// <c>[Obsolete]</c> <c>Init(string dsn)</c>. Without it the only
    /// non-obsolete way to initialize from a DSN string was to hand-build a
    /// <see cref="SauronOptions"/>, so every "quickstart" snippet in the docs
    /// had to either use the obsolete overload or stop being a one-liner.
    ///
    /// Asserts the client is ENABLED, not merely non-null: a constructed but
    /// disabled client is what a swallowed DSN or release failure looks like.
    /// No events are captured, so the <c>Close()</c> below flushes an empty
    /// buffer and sends nothing — there is no real endpoint behind this DSN.
    /// </summary>
    [Fact]
    public void Init_WithDsnAndRelease_ConstructsAnEnabledClient()
    {
        SauronSdk.Init("https://pk_test@localhost:8081/1", "svc@1.4.2");
        try
        {
            var client = SauronSdk.Current;
            Assert.NotNull(client);
            Assert.True(client!.Enabled);
        }
        finally
        {
            SauronSdk.Close();
        }
    }

    /// <summary>
    /// The release reaches the wire, trimmed — so <c>" svc@1.4.2 "</c> cannot
    /// become a second entry beside <c>svc@1.4.2</c> in the dashboard's
    /// release switcher. Asserted on the envelope header rather than on
    /// <c>SauronOptions</c>, because the header is what the server stores.
    /// </summary>
    [Fact]
    public void ReleaseReachesTheEnvelopeHeaderTrimmed()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions { Release = "  svc@1.4.2  " });
        client.CaptureMessage("hi");
        client.Flush();

        using var doc = System.Text.Json.JsonDocument.Parse(handler.LastBody!);
        Assert.Equal(
            "svc@1.4.2",
            doc.RootElement.GetProperty("header").GetProperty("release").GetString());
    }

    /// <summary>
    /// The one-argument overload is obsolete, not removed — a caller passing a
    /// non-blank DSN still gets the same <see cref="ArgumentException"/> the
    /// constructor has thrown since 1.6.0; the attribute only explains it
    /// earlier.
    /// </summary>
    [Fact]
    public void Init_WithDsnOnly_StillThrowsForANonBlankDsn()
    {
#pragma warning disable CS0618 // the point of this test
        var ex = Assert.Throws<ArgumentException>(() => SauronSdk.Init("https://pk_test@localhost:8081/1"));
#pragma warning restore CS0618
        Assert.Contains("Release", ex.Message);
    }
}
