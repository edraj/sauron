using System;
using System.Linq;
using Xunit;

namespace Sauron.Tests;

public class StackTraceExtractorTests
{
    [Fact]
    public void Parse_SyntheticTrace_ReversesToCrashLast_AndParsesFields()
    {
        // .NET stack traces are crash-first: DoWork (the throw site) is the top line.
        var trace = string.Join("\n", new[]
        {
            "   at MyApp.Service.DoWork(Int32 id) in /home/app/Service.cs:line 42",
            "   at MyApp.Program.Main() in /home/app/Program.cs:line 10",
            "   at System.Runtime.ExceptionServices.Foo.Bar()",
        });

        var frames = StackTraceExtractor.Parse(trace);

        Assert.Equal(3, frames.Count);

        // Crash frame must be LAST per the wire contract (call-site -> crash).
        var crash = frames[^1];
        Assert.StartsWith("DoWork", crash.Function);
        Assert.Equal("MyApp.Service", crash.Module);
        Assert.Equal("Service.cs", crash.Filename);
        Assert.Equal("/home/app/Service.cs", crash.AbsPath);
        Assert.Equal(42, crash.Lineno);
        Assert.True(crash.InApp);

        // System.* frame is not in-app.
        var systemFrame = frames.Single(f => f.Module == "System.Runtime.ExceptionServices.Foo");
        Assert.False(systemFrame.InApp);
    }

    [Fact]
    public void Extract_RealException_HasCrashFrameLast()
    {
        Exception captured;
        try
        {
            ThrowingMethod();
            throw new InvalidOperationException("unreachable");
        }
        catch (Exception ex)
        {
            captured = ex;
        }

        var frames = StackTraceExtractor.Extract(captured);

        Assert.NotEmpty(frames);
        // The deepest (throw site) frame is last; it should reference ThrowingMethod.
        Assert.Contains(nameof(ThrowingMethod), frames[^1].Function);
        Assert.True(frames[^1].InApp);
    }

    [Fact]
    public void Parse_NullOrEmpty_ReturnsEmpty()
    {
        Assert.Empty(StackTraceExtractor.Parse(null));
        Assert.Empty(StackTraceExtractor.Parse(string.Empty));
    }

    [Fact]
    public void Parse_WithInAppInclude_OnlyMatchingPrefixesAreInApp()
    {
        var trace = string.Join("\n", new[]
        {
            "   at MyApp.Service.DoWork()",
            "   at Vendor.Lib.Helper.Run()",
        });

        var frames = StackTraceExtractor.Parse(trace, new[] { "MyApp." });

        var mine = frames.Single(f => f.Module == "MyApp.Service");
        var vendor = frames.Single(f => f.Module == "Vendor.Lib.Helper");
        Assert.True(mine.InApp);
        Assert.False(vendor.InApp);
    }

    private static void ThrowingMethod()
        => throw new InvalidOperationException("boom");
}
