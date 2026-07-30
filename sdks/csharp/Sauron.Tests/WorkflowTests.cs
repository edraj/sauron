using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using Xunit;

namespace Sauron.Tests;

/// <summary>
/// Workflows: named, explicitly-bounded spans of activity (<c>StartWorkflow</c> /
/// <c>EndWorkflow</c> / <c>CancelWorkflow</c>). State lives on <see cref="Scope"/>, which is
/// <c>AsyncLocal</c> (see <see cref="ScopeManager"/>) — never a static field, so concurrent
/// requests never observe each other's workflow. That per-request isolation is the whole
/// point of <see cref="Workflow_DoesNotLeak_AcrossConcurrentAsyncFlows"/> below.
/// </summary>
[Collection("SauronScope")]
public class WorkflowTests
{
    public WorkflowTests() => ScopeManager.ResetForTests();

    private static JsonElement[] AllItems(string body)
    {
        using var doc = JsonDocument.Parse(body);
        return doc.RootElement.GetProperty("items").EnumerateArray()
            .Select(e => JsonDocument.Parse(e.GetRawText()).RootElement)
            .ToArray();
    }

    [Fact]
    public void Start_EmitsWorkflowStart_StampedWithNewWorkflow()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        var result = client.StartWorkflow("checkout");
        Assert.Equal(WorkflowStatus.Ok, result.Status);
        Assert.False(string.IsNullOrEmpty(result.WorkflowId));

        client.Flush();
        var items = AllItems(handler.LastBody!);
        Assert.Single(items);

        var item = items[0];
        Assert.Equal("event", item.GetProperty("type").GetString());
        Assert.Equal("$workflow_start", item.GetProperty("name").GetString());
        Assert.Equal(result.WorkflowId, item.GetProperty("workflow_id").GetString());
        Assert.Equal("checkout", item.GetProperty("workflow_name").GetString());

        // Contract item 6: also present in the event's own `properties`.
        var props = item.GetProperty("properties");
        Assert.Equal(result.WorkflowId, props.GetProperty("workflow_id").GetString());
        Assert.Equal("checkout", props.GetProperty("workflow_name").GetString());

        var active = client.GetWorkflow();
        Assert.NotNull(active);
        Assert.Equal(result.WorkflowId, active!.WorkflowId);
        Assert.Equal("checkout", active.Name);
    }

    [Fact]
    public void Stamps_Track_CaptureException_CaptureMessage_And_Transaction()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        var start = client.StartWorkflow("checkout");

        client.Track("custom_event", "u1");
        try { throw new InvalidOperationException("boom"); }
        catch (Exception ex) { client.CaptureException(ex); }
        client.CaptureMessage("inline-built path"); // built inline — not via CaptureExceptionCore
        client.TrackTransaction("op", 1.0);

        client.Flush();
        var items = AllItems(handler.LastBody!);
        // items[0] is $workflow_start; the four calls above are items[1..4].
        Assert.Equal(5, items.Length);

        var trackedEvent = items[1];
        Assert.Equal("custom_event", trackedEvent.GetProperty("name").GetString());
        Assert.Equal(start.WorkflowId, trackedEvent.GetProperty("workflow_id").GetString());
        Assert.Equal("checkout", trackedEvent.GetProperty("workflow_name").GetString());

        var capturedException = items[2];
        Assert.Equal("error", capturedException.GetProperty("type").GetString());
        Assert.Equal(start.WorkflowId, capturedException.GetProperty("workflow_id").GetString());
        Assert.Equal("checkout", capturedException.GetProperty("workflow_name").GetString());

        var capturedMessage = items[3];
        Assert.Equal("inline-built path", capturedMessage.GetProperty("message").GetString());
        Assert.Equal(start.WorkflowId, capturedMessage.GetProperty("workflow_id").GetString());
        Assert.Equal("checkout", capturedMessage.GetProperty("workflow_name").GetString());

        var transaction = items[4];
        Assert.Equal("transaction", transaction.GetProperty("type").GetString());
        Assert.Equal(start.WorkflowId, transaction.GetProperty("workflow_id").GetString());
        Assert.Equal("checkout", transaction.GetProperty("workflow_name").GetString());
    }

    [Fact]
    public void Identify_NeverStamped_EvenWhileWorkflowActive()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.StartWorkflow("checkout");
        client.Identify("user-1", new Dictionary<string, object?> { ["plan"] = "pro" });

        client.Flush();
        var items = AllItems(handler.LastBody!);
        var identifyItem = items.Single(i => i.GetProperty("type").GetString() == "identify");

        Assert.False(identifyItem.TryGetProperty("workflow_id", out _));
        Assert.False(identifyItem.TryGetProperty("workflow_name", out _));
    }

    [Fact]
    public void Keys_AreOmittedFromJson_WhenNoWorkflowActive()
    {
        // DTO-level: constructing an item with no workflow set must omit the keys, never
        // serialize them as `null` — checked with TryGetProperty, not a null-value check.
        var eventJson = JsonSerializer.Serialize(
            new EventItem { Name = "n", DistinctId = "d", Timestamp = "t" }, SauronJson.Options);
        using (var d = JsonDocument.Parse(eventJson))
        {
            Assert.False(d.RootElement.TryGetProperty("workflow_id", out _));
            Assert.False(d.RootElement.TryGetProperty("workflow_name", out _));
        }

        var errorJson = JsonSerializer.Serialize(
            new ErrorItem { EventId = "e", Timestamp = "t" }, SauronJson.Options);
        using (var d = JsonDocument.Parse(errorJson))
        {
            Assert.False(d.RootElement.TryGetProperty("workflow_id", out _));
            Assert.False(d.RootElement.TryGetProperty("workflow_name", out _));
        }

        var txJson = JsonSerializer.Serialize(
            new TransactionItem { Name = "n", Timestamp = "t" }, SauronJson.Options);
        using (var d = JsonDocument.Parse(txJson))
        {
            Assert.False(d.RootElement.TryGetProperty("workflow_id", out _));
            Assert.False(d.RootElement.TryGetProperty("workflow_name", out _));
        }

        // End-to-end: an app that never touches the workflow API stays byte-identical.
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);
        client.Track("plain", "u1");
        client.Flush();
        var item = TestUtil.FirstItem(handler.LastBody!);
        Assert.False(item.TryGetProperty("workflow_id", out _));
        Assert.False(item.TryGetProperty("workflow_name", out _));
    }

    [Fact]
    public void Start_WhileActive_ReturnsAlreadyActive_AndEmitsNothing()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        var first = client.StartWorkflow("a");
        Assert.Equal(WorkflowStatus.Ok, first.Status);

        var second = client.StartWorkflow("b");
        Assert.Equal(WorkflowStatus.AlreadyActive, second.Status);
        Assert.Null(second.WorkflowId);

        client.Flush();
        var items = AllItems(handler.LastBody!);
        Assert.Single(items); // only the original $workflow_start — nothing for the rejected start

        var active = client.GetWorkflow();
        Assert.NotNull(active);
        Assert.Equal("a", active!.Name);
        Assert.Equal(first.WorkflowId, active.WorkflowId);
    }

    [Fact]
    public void Force_CancelsWithSuperseded_ThenStartsNew()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        var first = client.StartWorkflow("a");
        var second = client.StartWorkflow("b", force: true);

        Assert.Equal(WorkflowStatus.Ok, second.Status);
        Assert.NotEqual(first.WorkflowId, second.WorkflowId);

        client.Flush();
        var items = AllItems(handler.LastBody!);
        Assert.Equal(3, items.Length);

        Assert.Equal("$workflow_start", items[0].GetProperty("name").GetString());
        Assert.Equal(first.WorkflowId, items[0].GetProperty("workflow_id").GetString());

        // The supersede-cancel is stamped with the OLD workflow (the one being closed),
        // emitted before state is replaced by the new one.
        Assert.Equal("$workflow_cancel", items[1].GetProperty("name").GetString());
        Assert.Equal(first.WorkflowId, items[1].GetProperty("workflow_id").GetString());
        Assert.Equal("a", items[1].GetProperty("workflow_name").GetString());
        Assert.Equal("superseded", items[1].GetProperty("properties").GetProperty("reason").GetString());

        Assert.Equal("$workflow_start", items[2].GetProperty("name").GetString());
        Assert.Equal(second.WorkflowId, items[2].GetProperty("workflow_id").GetString());
        Assert.Equal("b", items[2].GetProperty("workflow_name").GetString());

        var active = client.GetWorkflow();
        Assert.NotNull(active);
        Assert.Equal("b", active!.Name);
        Assert.Equal(second.WorkflowId, active.WorkflowId);
    }

    [Fact]
    public void End_EmitsWorkflowEnd_WithDurationMs_AndClearsScope()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        var start = client.StartWorkflow("checkout");
        var end = client.EndWorkflow();

        Assert.Equal(WorkflowStatus.Ok, end.Status);
        Assert.Equal(start.WorkflowId, end.WorkflowId);
        Assert.Null(client.GetWorkflow());

        client.Flush();
        var items = AllItems(handler.LastBody!);
        var closeItem = items[1];
        Assert.Equal("$workflow_end", closeItem.GetProperty("name").GetString());
        Assert.Equal(start.WorkflowId, closeItem.GetProperty("workflow_id").GetString());
        Assert.Equal("checkout", closeItem.GetProperty("workflow_name").GetString());

        var props = closeItem.GetProperty("properties");
        Assert.True(props.GetProperty("duration_ms").GetDouble() >= 0);
        Assert.False(props.TryGetProperty("reason", out _)); // reason is $workflow_cancel-only
    }

    [Fact]
    public void End_WithMismatchedName_IsNoOp_ReturnsNameMismatch()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.StartWorkflow("checkout");

        var mismatched = client.EndWorkflow("not-checkout");
        Assert.Equal(WorkflowStatus.NameMismatch, mismatched.Status);
        Assert.Null(mismatched.WorkflowId);

        // A malformed explicit name (blank / over the cap) reports NameMismatch too —
        // InvalidName is reachable only from StartWorkflow.
        Assert.Equal(WorkflowStatus.NameMismatch, client.EndWorkflow("   ").Status);
        Assert.Equal(WorkflowStatus.NameMismatch, client.EndWorkflow(new string('z', 130)).Status);

        var active = client.GetWorkflow();
        Assert.NotNull(active);
        Assert.Equal("checkout", active!.Name);

        client.Flush();
        var items = AllItems(handler.LastBody!);
        Assert.Single(items); // only $workflow_start — every mismatched End was a no-op
    }

    [Fact]
    public void End_WithNoneActive_ReturnsNotActive()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        var result = client.EndWorkflow();
        Assert.Equal(WorkflowStatus.NotActive, result.Status);
        Assert.Null(result.WorkflowId);

        var cancelResult = client.CancelWorkflow();
        Assert.Equal(WorkflowStatus.NotActive, cancelResult.Status);
    }

    [Fact]
    public void Cancel_DefaultsReasonToUser_AndCapsAt120()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.StartWorkflow("a");
        var cancelDefault = client.CancelWorkflow();
        Assert.Equal(WorkflowStatus.Ok, cancelDefault.Status);

        client.StartWorkflow("b");
        var longReason = new string('x', 300);
        var cancelLong = client.CancelWorkflow(reason: longReason);
        Assert.Equal(WorkflowStatus.Ok, cancelLong.Status);

        client.Flush();
        var items = AllItems(handler.LastBody!);
        // start(a), cancel(a), start(b), cancel(b)
        Assert.Equal(4, items.Length);

        Assert.Equal("$workflow_cancel", items[1].GetProperty("name").GetString());
        Assert.Equal("user", items[1].GetProperty("properties").GetProperty("reason").GetString());

        var cappedReason = items[3].GetProperty("properties").GetProperty("reason").GetString();
        Assert.Equal(120, cappedReason!.Length);
        Assert.Equal(new string('x', 120), cappedReason);
    }

    [Fact]
    public void Rejects_EmptyAndOverlongNames()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        Assert.Equal(WorkflowStatus.InvalidName, client.StartWorkflow("").Status);
        Assert.Equal(WorkflowStatus.InvalidName, client.StartWorkflow("   ").Status);
        Assert.Equal(WorkflowStatus.InvalidName, client.StartWorkflow(new string('a', 121)).Status);

        // Exactly at the cap is valid — reject, never truncate.
        var atCap = client.StartWorkflow(new string('a', 120));
        Assert.Equal(WorkflowStatus.Ok, atCap.Status);
        client.EndWorkflow();

        // Trimming happens before the length check (and before the emptiness check): a
        // padded-but-short-after-trim name is valid.
        var padded = client.StartWorkflow("   checkout   ");
        Assert.Equal(WorkflowStatus.Ok, padded.Status);
        Assert.Equal("checkout", client.GetWorkflow()!.Name);
        client.EndWorkflow();

        client.Flush();
        var items = AllItems(handler.LastBody!);
        // Nothing was emitted for the three rejected calls: start(120a), end, start(checkout), end.
        Assert.Equal(4, items.Length);
    }

    [Fact]
    public void GetWorkflow_ReturnsNull_BeforeAnyStart()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);
        Assert.Null(client.GetWorkflow());
    }

    [Fact]
    public void LifecycleEvents_SendEmptyDistinctId_WhenNoScopedUser()
    {
        // Empty (not a synthetic "anon_*"/"system"/device id) so the server stores NULL and
        // COUNT(DISTINCT distinct_id) skips it — an anonymous run must not fabricate a user.
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.StartWorkflow("checkout");
        client.EndWorkflow();
        client.Flush();

        var items = AllItems(handler.LastBody!);
        Assert.Equal(2, items.Length);
        foreach (var item in items)
        {
            // The field is still SENT (required String on the wire) — just empty.
            Assert.True(item.TryGetProperty("distinct_id", out var distinctId));
            Assert.Equal(JsonValueKind.String, distinctId.ValueKind);
            Assert.Equal("", distinctId.GetString());
        }
    }

    [Fact]
    public void LifecycleEvents_UseScopedUserId_WhenPresent()
    {
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        ScopeManager.Current.SetUser(new SauronUser { Id = "user-7" });
        client.StartWorkflow("checkout");
        client.CancelWorkflow();
        client.Flush();

        var items = AllItems(handler.LastBody!);
        Assert.Equal(2, items.Length);
        Assert.All(items, item => Assert.Equal("user-7", item.GetProperty("distinct_id").GetString()));
    }

    [Fact]
    public void Track_StillRejectsEmptyDistinctId_ForOrdinaryEvents()
    {
        // The empty-distinct_id path is for the three reserved lifecycle events ONLY; the
        // public Track guard must be untouched.
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        Assert.Throws<ArgumentException>(() => client.Track("ordinary", ""));
        Assert.Throws<ArgumentException>(() => client.Track("ordinary", null!));
    }

    [Fact]
    public void Dispose_ClearsUnendedWorkflow_FromGlobalScope()
    {
        // A Close()-then-Init() config reload must not inherit the previous run's workflow.
        var handler = new CapturingHandler();
        var client = TestUtil.NewClient(handler);

        var first = client.StartWorkflow("checkout"); // ambient/global scope — no PushScope
        Assert.Equal(WorkflowStatus.Ok, first.Status);
        Assert.NotNull(client.GetWorkflow());

        client.Close(); // never ends the workflow, and must NOT emit a cancel of its own

        Assert.Null(client.GetWorkflow());
        Assert.Null(ScopeManager.Global.Workflow);

        // A brand-new client starts clean rather than reporting AlreadyActive.
        using var reinitialized = TestUtil.NewClient(new CapturingHandler());
        var second = reinitialized.StartWorkflow("shipping");
        Assert.Equal(WorkflowStatus.Ok, second.Status);
        Assert.Equal("shipping", reinitialized.GetWorkflow()!.Name);
    }

    [Fact]
    public void Dispose_DoesNotEmitCancel_ForUnendedWorkflow()
    {
        // Contract: teardown CLEARS state but never fabricates a $workflow_cancel — an
        // abandoned workflow is a legitimate server-derived outcome (30 min, on read).
        var handler = new CapturingHandler();
        var client = TestUtil.NewClient(handler);

        client.StartWorkflow("checkout");
        client.Close(); // Close performs a final flush

        var items = AllItems(handler.LastBody!);
        Assert.Equal("$workflow_start", Assert.Single(items).GetProperty("name").GetString());
        Assert.DoesNotContain(items, i => i.GetProperty("name").GetString() == "$workflow_cancel");
    }

    [Fact]
    public void Force_MintsReplacement_EvenThoughSupersedeCancelIsEmittedFirst()
    {
        // Ordering guard (contract addendum 23): the replacement workflow is constructed
        // BEFORE the supersede-cancel reaches the wire, so there is no window in which the
        // old workflow is cancelled server-side yet still sitting in scope.
        var handler = new CapturingHandler();
        using var client = TestUtil.NewClient(handler);

        client.StartWorkflow("a");
        var before = DateTimeOffset.UtcNow;
        var second = client.StartWorkflow("b", force: true);

        Assert.Equal(WorkflowStatus.Ok, second.Status);
        var active = client.GetWorkflow();
        Assert.NotNull(active);
        Assert.Equal(second.WorkflowId, active!.WorkflowId);
        Assert.Equal("b", active.Name);
        Assert.True(active.StartedAt >= before.AddSeconds(-1));
    }

    [Fact]
    public void WorkflowStatus_SerializesAsExactLowercaseWireStrings()
    {
        // The six values are contract across every Sauron SDK; the converter guards them
        // against a future diagnostics path emitting "Ok" instead of "ok".
        Assert.Equal("\"ok\"", JsonSerializer.Serialize(WorkflowStatus.Ok));
        Assert.Equal("\"already_active\"", JsonSerializer.Serialize(WorkflowStatus.AlreadyActive));
        Assert.Equal("\"not_active\"", JsonSerializer.Serialize(WorkflowStatus.NotActive));
        Assert.Equal("\"name_mismatch\"", JsonSerializer.Serialize(WorkflowStatus.NameMismatch));
        Assert.Equal("\"invalid_name\"", JsonSerializer.Serialize(WorkflowStatus.InvalidName));
        Assert.Equal("\"disabled\"", JsonSerializer.Serialize(WorkflowStatus.Disabled));

        // Round-trips, including nested inside a WorkflowResult.
        Assert.Equal(WorkflowStatus.NameMismatch,
            JsonSerializer.Deserialize<WorkflowStatus>("\"name_mismatch\""));
        Assert.Contains("\"already_active\"",
            JsonSerializer.Serialize(new WorkflowResult(WorkflowStatus.AlreadyActive)));
    }

    [Fact]
    public void Disabled_WhenClientNotEnabled_ReturnsDisabledForAllThreeMutators()
    {
        var handler = new CapturingHandler();
        using var client = new SauronClient(new SauronOptions
        {
            Dsn = "not-a-valid-dsn",
            HttpMessageHandler = handler,
        });
        Assert.False(client.Enabled);

        Assert.Equal(WorkflowStatus.Disabled, client.StartWorkflow("a").Status);
        Assert.Equal(WorkflowStatus.Disabled, client.EndWorkflow().Status);
        Assert.Equal(WorkflowStatus.Disabled, client.CancelWorkflow().Status);
        Assert.Null(client.GetWorkflow());
    }

    [Fact]
    public void Facade_BeforeInit_ReturnsDisabled()
    {
        SauronSdk.Close();

        Assert.Equal(WorkflowStatus.Disabled, SauronSdk.StartWorkflow("a").Status);
        Assert.Equal(WorkflowStatus.Disabled, SauronSdk.EndWorkflow().Status);
        Assert.Equal(WorkflowStatus.Disabled, SauronSdk.CancelWorkflow().Status);
        Assert.Null(SauronSdk.GetWorkflow());
    }

    [Fact]
    public void Facade_ForwardsWorkflowCalls_ThroughInitializedClient()
    {
        var handler = new CapturingHandler();
        SauronSdk.Init(new SauronOptions
        {
            Dsn = "https://pub123@example.com/42",
            HttpMessageHandler = handler,
            FlushInterval = TimeSpan.FromHours(1),
            MaxBatch = 1000,
        });
        try
        {
            var start = SauronSdk.StartWorkflow("checkout");
            Assert.Equal(WorkflowStatus.Ok, start.Status);

            var active = SauronSdk.GetWorkflow();
            Assert.NotNull(active);
            Assert.Equal("checkout", active!.Name);

            var end = SauronSdk.EndWorkflow();
            Assert.Equal(WorkflowStatus.Ok, end.Status);
            Assert.Null(SauronSdk.GetWorkflow());
        }
        finally
        {
            SauronSdk.Close();
        }
    }

    /// <summary>
    /// The mandatory concurrency proof: two interleaved async flows, each starting its own
    /// workflow inside its own pushed scope. Neither must see the other's active workflow,
    /// and each one's captured error must be stamped with its own workflow id — never the
    /// other's. Uses <c>BeforeSend</c> as a synchronous capture point (no HTTP/flush
    /// ordering involved) so items from both concurrent flows can be inspected afterwards.
    ///
    /// This could not pass if workflow state were a static/instance field: whichever task's
    /// <see cref="SauronClient.StartWorkflow"/> ran second would see the first task's
    /// workflow already active and get <see cref="WorkflowStatus.AlreadyActive"/> instead of
    /// <see cref="WorkflowStatus.Ok"/>, failing the assertion inside <c>RunAsync</c> below —
    /// or, if isolation were subtly broken in some other way, the two captured errors would
    /// end up stamped with the same (whichever-ran-last) workflow id.
    /// </summary>
    [Fact]
    public async Task Workflow_DoesNotLeak_AcrossConcurrentAsyncFlows()
    {
        var capturedErrors = new ConcurrentBag<ErrorItem>();
        using var client = TestUtil.NewClient(new CapturingHandler(), new SauronOptions
        {
            BeforeSend = item =>
            {
                if (item is ErrorItem err) capturedErrors.Add(err);
                return item;
            },
        });

        async Task<string> RunAsync(string workflowName, int firstDelayMs, int secondDelayMs)
        {
            using (ScopeManager.PushScope())
            {
                var start = client.StartWorkflow(workflowName);
                Assert.Equal(WorkflowStatus.Ok, start.Status);
                var myWorkflowId = start.WorkflowId!;

                await Task.Delay(firstDelayMs);

                // Mid-flight, after the other flow has also started: must see only my own.
                var current = client.GetWorkflow();
                Assert.NotNull(current);
                Assert.Equal(myWorkflowId, current!.WorkflowId);
                Assert.Equal(workflowName, current.Name);

                try { throw new InvalidOperationException(workflowName); }
                catch (Exception ex) { client.CaptureException(ex); }

                await Task.Delay(secondDelayMs);

                var end = client.EndWorkflow();
                Assert.Equal(WorkflowStatus.Ok, end.Status);
                Assert.Equal(myWorkflowId, end.WorkflowId);

                return myWorkflowId;
            }
        }

        var results = await Task.WhenAll(
            Task.Run(() => RunAsync("checkout", 30, 5)),
            Task.Run(() => RunAsync("refund", 5, 30)));

        var checkoutWorkflowId = results[0];
        var refundWorkflowId = results[1];
        Assert.NotEqual(checkoutWorkflowId, refundWorkflowId);

        var checkoutError = Assert.Single(capturedErrors, e => e.Exception!.Value == "checkout");
        var refundError = Assert.Single(capturedErrors, e => e.Exception!.Value == "refund");

        Assert.Equal(checkoutWorkflowId, checkoutError.WorkflowId);
        Assert.Equal(refundWorkflowId, refundError.WorkflowId);

        // The cross-contamination check that a shared/static field would fail:
        Assert.NotEqual(refundWorkflowId, checkoutError.WorkflowId);
        Assert.NotEqual(checkoutWorkflowId, refundError.WorkflowId);
    }
}
