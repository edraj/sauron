using System;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Sauron;

/// <summary>
/// Result of a workflow lifecycle call. Exactly six statuses, no seventh: <c>Disabled</c>
/// doubles as "no initialized/enabled client" and the safe fallback for an unexpected
/// internal error (never a claim about caller input or workflow state in that case).
/// </summary>
/// <remarks>
/// These six members are PascalCase for .NET callers, but each has an exact lowercase
/// snake_case counterpart that is contract across every Sauron SDK: <c>ok</c>,
/// <c>already_active</c>, <c>not_active</c>, <c>name_mismatch</c>, <c>invalid_name</c>,
/// <c>disabled</c>. Nothing serializes this enum today (it only ever returns to host code
/// inside a <see cref="WorkflowResult"/>), but <see cref="WorkflowStatusJsonConverter"/> is
/// attached so a future diagnostics/logging path that does serialize it emits the wire
/// spelling rather than <c>"Ok"</c>. Do not add a seventh member.
/// </remarks>
[JsonConverter(typeof(WorkflowStatusJsonConverter))]
public enum WorkflowStatus
{
    Ok,
    AlreadyActive,
    NotActive,
    NameMismatch,
    InvalidName,
    Disabled,
}

/// <summary>
/// Serializes <see cref="WorkflowStatus"/> as its exact lowercase wire string (and reads it
/// back). Guards the six contract values against drift if this enum ever reaches JSON.
/// </summary>
internal sealed class WorkflowStatusJsonConverter : JsonConverter<WorkflowStatus>
{
    internal static string ToWire(WorkflowStatus status) => status switch
    {
        WorkflowStatus.Ok => "ok",
        WorkflowStatus.AlreadyActive => "already_active",
        WorkflowStatus.NotActive => "not_active",
        WorkflowStatus.NameMismatch => "name_mismatch",
        WorkflowStatus.InvalidName => "invalid_name",
        WorkflowStatus.Disabled => "disabled",
        _ => throw new ArgumentOutOfRangeException(nameof(status), status, "unknown WorkflowStatus"),
    };

    public override WorkflowStatus Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
        => reader.GetString() switch
        {
            "ok" => WorkflowStatus.Ok,
            "already_active" => WorkflowStatus.AlreadyActive,
            "not_active" => WorkflowStatus.NotActive,
            "name_mismatch" => WorkflowStatus.NameMismatch,
            "invalid_name" => WorkflowStatus.InvalidName,
            "disabled" => WorkflowStatus.Disabled,
            var other => throw new JsonException($"unknown WorkflowStatus '{other}'"),
        };

    public override void Write(Utf8JsonWriter writer, WorkflowStatus value, JsonSerializerOptions options)
        => writer.WriteStringValue(ToWire(value));
}

/// <summary>
/// Outcome of <see cref="SauronClient.StartWorkflow"/> / <see cref="SauronClient.EndWorkflow"/>
/// / <see cref="SauronClient.CancelWorkflow"/>. <see cref="WorkflowId"/> is set on <see cref="WorkflowStatus.Ok"/>.
/// </summary>
public sealed record WorkflowResult(WorkflowStatus Status, string? WorkflowId = null);

/// <summary>
/// The workflow currently bounding the active scope, if any. Held on <see cref="Scope"/>
/// (per-request via <c>AsyncLocal</c>, see <see cref="ScopeManager"/>) — never a static/global
/// field, so concurrent requests never observe each other's workflow.
/// </summary>
public sealed record ActiveWorkflow(string WorkflowId, string Name, DateTimeOffset StartedAt);

/// <summary>Reserved analytics event names for the workflow lifecycle. Spelled exactly — wire contract.</summary>
internal static class WorkflowEvents
{
    internal const string Start = "$workflow_start";
    internal const string End = "$workflow_end";
    internal const string Cancel = "$workflow_cancel";
}

/// <summary>Name/reason normalization shared by <c>StartWorkflow</c>/<c>EndWorkflow</c>/<c>CancelWorkflow</c>.</summary>
internal static class WorkflowNames
{
    internal const int NameMax = 120;
    internal const int ReasonMax = 120;

    /// <summary>
    /// Trim, then reject if empty or over <see cref="NameMax"/> — reject, never truncate.
    /// Returns the trimmed name, or <c>null</c> when invalid.
    /// </summary>
    internal static string? Normalize(string? name)
    {
        if (string.IsNullOrWhiteSpace(name)) return null;
        var trimmed = name.Trim();
        return trimmed.Length == 0 || trimmed.Length > NameMax ? null : trimmed;
    }

    /// <summary>Default to <c>"user"</c> for null/blank input, else trim and cap at <see cref="ReasonMax"/>.</summary>
    internal static string NormalizeReason(string? reason)
    {
        if (string.IsNullOrWhiteSpace(reason)) return "user";
        var trimmed = reason.Trim();
        return trimmed.Length > ReasonMax ? trimmed[..ReasonMax] : trimmed;
    }
}
