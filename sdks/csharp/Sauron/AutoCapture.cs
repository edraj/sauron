using System;
using System.Threading.Tasks;

namespace Sauron;

/// <summary>
/// Opt-in global uncaught-error capture (init flag <see cref="SauronOptions.AutoCaptureUnhandled"/>,
/// default off). Subscribes to <see cref="AppDomain.UnhandledException"/> and
/// <see cref="TaskScheduler.UnobservedTaskException"/>; each handler captures the exception with
/// <c>mechanism.handled = false</c> and performs a best-effort synchronous flush so the report
/// survives the imminent crash. The handlers never suppress termination or mark the task observed —
/// the runtime's default crash/exit behavior is preserved. <see cref="Dispose"/> unsubscribes.
/// </summary>
internal sealed class AutoCapture : IDisposable
{
    private const string DomainMechanism = "AppDomain.UnhandledException";
    private const string TaskMechanism = "TaskScheduler.UnobservedTaskException";

    private readonly SauronClient _client;
    private readonly UnhandledExceptionEventHandler _domainHandler;
    private readonly EventHandler<UnobservedTaskExceptionEventArgs> _taskHandler;
    private bool _disposed;

    private AutoCapture(SauronClient client)
    {
        _client = client;
        _domainHandler = OnUnhandledException;
        _taskHandler = OnUnobservedTaskException;
    }

    /// <summary>Wire the process-global uncaught-error handlers and return the live installation.</summary>
    public static AutoCapture Install(SauronClient client)
    {
        var auto = new AutoCapture(client);
        AppDomain.CurrentDomain.UnhandledException += auto._domainHandler;
        TaskScheduler.UnobservedTaskException += auto._taskHandler;
        return auto;
    }

    /// <summary>
    /// Handler for <see cref="AppDomain.UnhandledException"/>. Internal so tests can invoke it
    /// directly (raising the real event would terminate the test host).
    /// </summary>
    internal void OnUnhandledException(object? sender, UnhandledExceptionEventArgs e)
    {
        if (e.ExceptionObject is Exception ex)
            Capture(ex, DomainMechanism);
        // A non-Exception payload (rare) carries no stack/type to report — ignore it.
    }

    /// <summary>
    /// Handler for <see cref="TaskScheduler.UnobservedTaskException"/>. Does not call
    /// <see cref="UnobservedTaskExceptionEventArgs.SetObserved"/> — the default behavior is preserved.
    /// </summary>
    internal void OnUnobservedTaskException(object? sender, UnobservedTaskExceptionEventArgs e)
    {
        if (e.Exception is Exception ex)
            Capture(ex, TaskMechanism);
    }

    private void Capture(Exception exception, string mechanismType)
    {
        try
        {
            _client.CaptureUnhandled(exception, mechanismType);
            // Best-effort synchronous flush: the process is likely terminating, so the background
            // timer won't get another tick to deliver this report.
            _client.Flush();
        }
        catch
        {
            // A crash-time handler must never throw.
        }
    }

    public void Dispose()
    {
        if (_disposed)
            return;
        _disposed = true;
        AppDomain.CurrentDomain.UnhandledException -= _domainHandler;
        TaskScheduler.UnobservedTaskException -= _taskHandler;
    }
}
