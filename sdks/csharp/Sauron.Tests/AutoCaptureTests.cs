using System;
using System.Threading.Tasks;
using Xunit;

namespace Sauron.Tests;

/// <summary>
/// C8 — opt-in auto uncaught-error capture. Off by default; when enabled, unhandled
/// exceptions are captured with <c>mechanism.handled = false</c> and the default
/// crash/exit behavior is preserved (the handler never suppresses termination).
/// </summary>
[Collection("SauronScope")]
public class AutoCaptureTests
{
    public AutoCaptureTests() => ScopeManager.ResetForTests();

    [Fact]
    public void AutoCapture_NotInstalled_WhenOptionOff()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler); // default: AutoCaptureUnhandled = false
        Assert.Null(client.AutoCapture);
    }

    [Fact]
    public void AutoCapture_Installed_WhenOptedIn()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions { AutoCaptureUnhandled = true });
        Assert.NotNull(client.AutoCapture);
    }

    [Fact]
    public void AutoCapture_NotInstalled_WhenDisabledDsn()
    {
        // A bad DSN puts the client in no-op mode: auto-capture must not wire itself.
        using var client = new SauronClient(new SauronOptions { Dsn = "not-a-dsn", AutoCaptureUnhandled = true });
        Assert.False(client.Enabled);
        Assert.Null(client.AutoCapture);
    }

    [Fact]
    public void UnhandledException_Captured_WithHandledFalse()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions { AutoCaptureUnhandled = true });

        var ex = new InvalidOperationException("boom");
        client.AutoCapture!.OnUnhandledException(this, new UnhandledExceptionEventArgs(ex, isTerminating: false));

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("error", item.GetProperty("type").GetString());
        Assert.Equal("System.InvalidOperationException", item.GetProperty("exception").GetProperty("type").GetString());
        Assert.Equal("boom", item.GetProperty("exception").GetProperty("value").GetString());
        Assert.False(item.GetProperty("exception").GetProperty("mechanism").GetProperty("handled").GetBoolean());
    }

    [Fact]
    public void UnobservedTaskException_Captured_WithHandledFalse()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions { AutoCaptureUnhandled = true });

        var ex = new AggregateException(new InvalidOperationException("async boom"));
        client.AutoCapture!.OnUnobservedTaskException(this, new UnobservedTaskExceptionEventArgs(ex));

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("error", item.GetProperty("type").GetString());
        Assert.False(item.GetProperty("exception").GetProperty("mechanism").GetProperty("handled").GetBoolean());
    }

    [Fact]
    public void UnhandledException_NonExceptionPayload_IsIgnored()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions { AutoCaptureUnhandled = true });

        // ExceptionObject can, in theory, be a non-Exception. Must not throw, must not send.
        client.AutoCapture!.OnUnhandledException(this, new UnhandledExceptionEventArgs("just a string", isTerminating: false));

        Assert.Equal(0, handler.RequestCount);
    }

    [Fact]
    public void Dispose_UnsubscribesHandlers_AndIsIdempotent()
    {
        var handler = new CapturingHandler();
        var client = TestUtil.NewClient(handler, new SauronOptions { AutoCaptureUnhandled = true });
        var auto = client.AutoCapture!;

        client.Dispose(); // runs the uninstaller
        auto.Dispose();   // calling the uninstaller again is a no-op and must not throw
    }
}
