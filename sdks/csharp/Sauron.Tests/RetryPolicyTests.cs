using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;

namespace Sauron.Tests;

public class RetryPolicyTests
{
    private static SauronClient NewClient(ScriptedHandler handler, List<TimeSpan>? delays = null)
        => new(new SauronOptions
        {
            Dsn = "https://pub123@example.com/42",
            HttpMessageHandler = handler,
            FlushInterval = TimeSpan.FromHours(1),
            MaxBatch = 1000,
            DelayHook = d => { delays?.Add(d); return Task.CompletedTask; },
        });

    [Fact]
    public void Retry429ThenSucceeds_TwoSends()
    {
        var handler = new ScriptedHandler(ScriptedHandler.RetryAfter(0), ScriptedHandler.Ok());
        var client = NewClient(handler);

        client.Track("a", "u1");
        client.Flush();

        Assert.Equal(2, handler.RequestCount);
    }

    [Fact]
    public void Drop400_SingleSend_NoRetry()
    {
        var handler = new ScriptedHandler(ScriptedHandler.Status(400));
        var client = NewClient(handler);

        client.Track("a", "u1");
        client.Flush();

        Assert.Equal(1, handler.RequestCount);
    }

    [Theory]
    [InlineData(401)]
    [InlineData(403)]
    [InlineData(404)]
    public void DropNonRetryable4xx_SingleSend(int status)
    {
        var handler = new ScriptedHandler(ScriptedHandler.Status(status));
        var client = NewClient(handler);

        client.Track("a", "u1");
        client.Flush();

        Assert.Equal(1, handler.RequestCount);
    }

    [Theory]
    [InlineData(408)]
    [InlineData(413)]
    [InlineData(429)]
    [InlineData(500)]
    [InlineData(503)]
    public void RetryTransientThenSucceeds_TwoSends(int status)
    {
        var handler = new ScriptedHandler(ScriptedHandler.Status(status), ScriptedHandler.Ok());
        var client = NewClient(handler);

        client.Track("a", "u1");
        client.Flush();

        Assert.Equal(2, handler.RequestCount);
    }

    [Fact]
    public void GiveUpAfterThreeAttempts_OnPersistent5xx()
    {
        var handler = new ScriptedHandler(
            ScriptedHandler.Status(500),
            ScriptedHandler.Status(500),
            ScriptedHandler.Status(500),
            ScriptedHandler.Status(500),
            ScriptedHandler.Status(500));
        var client = NewClient(handler);

        client.Track("a", "u1");
        client.Flush();

        Assert.Equal(3, handler.RequestCount); // capped at 3 attempts
    }

    [Fact]
    public void RetryNetworkErrorThenSucceeds_TwoSends()
    {
        var handler = new ScriptedHandler(ScriptedHandler.Boom(), ScriptedHandler.Ok());
        var client = NewClient(handler);

        client.Track("a", "u1");
        client.Flush();

        Assert.Equal(2, handler.RequestCount);
    }

    [Fact]
    public void HonorsRetryAfterSeconds_On429()
    {
        var delays = new List<TimeSpan>();
        var handler = new ScriptedHandler(ScriptedHandler.RetryAfter(2), ScriptedHandler.Ok());
        var client = NewClient(handler, delays);

        client.Track("a", "u1");
        client.Flush();

        Assert.Single(delays);
        Assert.Equal(TimeSpan.FromSeconds(2), delays[0]);
    }

    [Fact]
    public void CapsBackoffAt30Seconds_OnHugeRetryAfter()
    {
        var delays = new List<TimeSpan>();
        var handler = new ScriptedHandler(ScriptedHandler.RetryAfter(600), ScriptedHandler.Ok());
        var client = NewClient(handler, delays);

        client.Track("a", "u1");
        client.Flush();

        Assert.Single(delays);
        Assert.True(delays[0] <= TimeSpan.FromSeconds(30), $"expected <= 30s, got {delays[0]}");
    }
}
