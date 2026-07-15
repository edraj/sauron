using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Xunit;

namespace Sauron.Tests;

/// <summary>
/// C1 — Scope + AsyncLocal isolation. These tests mutate the process-global scope,
/// so the whole family shares one non-parallel collection and resets between cases.
/// </summary>
[Collection("SauronScope")]
public class ScopeTests
{
    public ScopeTests() => ScopeManager.ResetForTests();

    [Fact]
    public void GlobalAndChildTags_BothApplyToError()
    {
        ScopeManager.Global.SetTag("env", "prod");

        using (ScopeManager.PushScope())
        {
            ScopeManager.Current.SetTag("req", "42");

            var item = new ErrorItem();
            ScopeManager.Current.ApplyToError(item);

            Assert.Equal("prod", item.Tags["env"]);
            Assert.Equal("42", item.Tags["req"]);
        }
    }

    [Fact]
    public void PushScope_RestoresParentOnDispose()
    {
        ScopeManager.Global.SetTag("env", "prod");

        using (ScopeManager.PushScope())
        {
            ScopeManager.Current.SetTag("req", "1");
            Assert.True(ScopeManager.Current.Tags.ContainsKey("req"));

            using (ScopeManager.PushScope())
            {
                ScopeManager.Current.SetTag("nested", "y");
                Assert.True(ScopeManager.Current.Tags.ContainsKey("nested"));
            }

            // inner disposed: parent scope restored (nested gone, req kept)
            Assert.False(ScopeManager.Current.Tags.ContainsKey("nested"));
            Assert.True(ScopeManager.Current.Tags.ContainsKey("req"));
        }

        // outer disposed: back to global (req gone, env kept)
        Assert.False(ScopeManager.Current.Tags.ContainsKey("req"));
        Assert.Equal("prod", ScopeManager.Current.Tags["env"]);
    }

    [Fact]
    public void ChildScope_DoesNotMutateGlobal()
    {
        using (ScopeManager.PushScope())
        {
            ScopeManager.Current.SetTag("only", "child");
        }
        Assert.False(ScopeManager.Global.Tags.ContainsKey("only"));
    }

    [Fact]
    public void BreadcrumbRing_CapsAtMax_DroppingOldest()
    {
        var s = new Scope();
        for (int i = 0; i < 5; i++)
            s.AddBreadcrumb(new Breadcrumb { Message = i.ToString() }, max: 3);

        Assert.Equal(new[] { "2", "3", "4" }, s.Breadcrumbs.Select(b => b.Message).ToArray());
    }

    [Fact]
    public async Task ConcurrentScopes_DoNotLeak()
    {
        async Task<string?> Run(string id)
        {
            using (ScopeManager.PushScope())
            {
                ScopeManager.Current.SetUser(new SauronUser { Id = id });
                ScopeManager.Current.SetTag("id", id);
                await Task.Delay(25);
                var tag = ScopeManager.Current.Tags.TryGetValue("id", out var v) ? v : null;
                var user = ScopeManager.Current.User?.Id;
                // both the async-local tag and user must still be this task's own
                return tag == user ? tag : $"LEAK:{tag}/{user}";
            }
        }

        var results = await Task.WhenAll(Task.Run(() => Run("A")), Task.Run(() => Run("B")));
        Assert.Equal(new[] { "A", "B" }, results.OrderBy(x => x).ToArray());
    }
}
