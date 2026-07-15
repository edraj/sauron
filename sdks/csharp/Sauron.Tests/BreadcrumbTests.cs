using System;
using System.Collections.Generic;
using Xunit;

namespace Sauron.Tests;

/// <summary>C2 — breadcrumbs attach to captured errors; the before-breadcrumb hook can drop/mutate.</summary>
[Collection("SauronScope")]
public class BreadcrumbTests
{
    public BreadcrumbTests() => ScopeManager.ResetForTests();

    [Fact]
    public void Breadcrumb_AttachesToCapturedError_WithSnakeCaseShape()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.AddBreadcrumb(new Breadcrumb
        {
            Type = "navigation",
            Category = "auth",
            Message = "login",
            Level = "info",
            Data = new Dictionary<string, object?> { ["user"] = "u1" },
        });
        client.CaptureMessage("boom");
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        var crumbs = item.GetProperty("breadcrumbs");
        Assert.Equal(1, crumbs.GetArrayLength());

        var crumb = crumbs[0];
        Assert.Equal("navigation", crumb.GetProperty("type").GetString());
        Assert.Equal("auth", crumb.GetProperty("category").GetString());
        Assert.Equal("login", crumb.GetProperty("message").GetString());
        Assert.Equal("info", crumb.GetProperty("level").GetString());
        Assert.False(string.IsNullOrEmpty(crumb.GetProperty("timestamp").GetString()));
        Assert.Equal("u1", crumb.GetProperty("data").GetProperty("user").GetString());
    }

    [Fact]
    public void Breadcrumb_AttachesToCapturedException()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.AddBreadcrumb(new Breadcrumb { Message = "step-1" });
        try { throw new InvalidOperationException("x"); }
        catch (Exception ex) { client.CaptureException(ex); }
        client.Flush();

        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.Equal("step-1", item.GetProperty("breadcrumbs")[0].GetProperty("message").GetString());
    }

    [Fact]
    public void BreadcrumbRing_BoundedAt_MaxBreadcrumbs()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions { MaxBreadcrumbs = 2 });

        client.AddBreadcrumb(new Breadcrumb { Message = "a" });
        client.AddBreadcrumb(new Breadcrumb { Message = "b" });
        client.AddBreadcrumb(new Breadcrumb { Message = "c" });
        client.CaptureMessage("boom");
        client.Flush();

        var crumbs = TestUtil.FirstItem(handler.LastBody!).GetProperty("breadcrumbs");
        Assert.Equal(2, crumbs.GetArrayLength());
        Assert.Equal("b", crumbs[0].GetProperty("message").GetString());
        Assert.Equal("c", crumbs[1].GetProperty("message").GetString());
    }

    [Fact]
    public void BeforeBreadcrumb_ReturningNull_DropsCrumb()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions { BeforeBreadcrumb = _ => null });

        client.AddBreadcrumb(new Breadcrumb { Message = "secret" });
        client.CaptureMessage("boom");
        client.Flush();

        Assert.Equal(0, TestUtil.FirstItem(handler.LastBody!).GetProperty("breadcrumbs").GetArrayLength());
    }

    [Fact]
    public void BeforeBreadcrumb_CanMutateCrumb()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler, new SauronOptions
        {
            BeforeBreadcrumb = b => { b.Message = "[redacted]"; return b; },
        });

        client.AddBreadcrumb(new Breadcrumb { Message = "secret" });
        client.CaptureMessage("boom");
        client.Flush();

        var crumb = TestUtil.FirstItem(handler.LastBody!).GetProperty("breadcrumbs")[0];
        Assert.Equal("[redacted]", crumb.GetProperty("message").GetString());
    }

    [Fact]
    public void AddBreadcrumb_BeforeInit_IsNoOpAndDoesNotThrow()
    {
        SauronSdk.Close();
        // No client initialized: must not throw.
        SauronSdk.AddBreadcrumb(new Breadcrumb { Message = "pre-init" });
    }
}
