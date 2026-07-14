using System.Collections.Generic;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Sauron;

/// <summary>
/// Wire DTOs for the Sauron ingest envelope. Serialized with <see cref="System.Text.Json"/>
/// using a snake_case property naming policy. See the ingest wire contract.
/// </summary>
internal static class SauronJson
{
    /// <summary>Shared serializer options: snake_case properties, nulls emitted (the contract lists explicit null fields).</summary>
    public static readonly JsonSerializerOptions Options = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        DefaultIgnoreCondition = JsonIgnoreCondition.Never,
        // Dictionary keys (user properties / traits / tags) are left untouched on purpose.
    };
}

internal sealed class Envelope
{
    public EnvelopeHeader Header { get; set; } = new();
    public EnvelopeContext Context { get; set; } = new();
    public List<object> Items { get; set; } = new();
}

internal sealed class EnvelopeHeader
{
    public string? Dsn { get; set; }
    public SdkInfo Sdk { get; set; } = new();
    public string SentAt { get; set; } = string.Empty;
    public string Environment { get; set; } = "production";
    public string? Release { get; set; }
}

internal sealed class SdkInfo
{
    public string Name { get; set; } = "sauron-dotnet";
    public string Version { get; set; } = "0.1.0";
}

internal sealed class EnvelopeContext
{
    public DeviceInfo Device { get; set; } = new();
    public OsInfo Os { get; set; } = new();
    public Dictionary<string, object?> App { get; set; } = new();
    public RuntimeInfo Runtime { get; set; } = new();
    public object? User { get; set; }
}

internal sealed class DeviceInfo
{
    public string? DeviceId { get; set; }
}

internal sealed class OsInfo
{
    public string? Name { get; set; }
    public string? Version { get; set; }
}

internal sealed class RuntimeInfo
{
    public string? Name { get; set; }
    public string? Version { get; set; }
}

// ---- Items -------------------------------------------------------------

internal sealed class EventItem
{
    public string Type { get; set; } = "event";
    public string Name { get; set; } = string.Empty;
    public string DistinctId { get; set; } = string.Empty;
    public Dictionary<string, object?> Properties { get; set; } = new();
    public string Timestamp { get; set; } = string.Empty;
    public string? SessionId { get; set; }
    public string? Screen { get; set; }
}

internal sealed class ErrorItem
{
    public string Type { get; set; } = "error";
    public string EventId { get; set; } = string.Empty;
    public string Level { get; set; } = "error";
    public string Timestamp { get; set; } = string.Empty;
    public ExceptionInfo? Exception { get; set; }
    public string? Message { get; set; }
    public List<object> Breadcrumbs { get; set; } = new();
    public Dictionary<string, object?> Tags { get; set; } = new();
    public string? Fingerprint { get; set; }
    public UserInfo? User { get; set; }
    public string? SessionId { get; set; }
    public string? Screen { get; set; }
}

internal sealed class ExceptionInfo
{
    public string Type { get; set; } = string.Empty;
    public string? Value { get; set; }
    public MechanismInfo Mechanism { get; set; } = new();
    public List<StackFrame> Stacktrace { get; set; } = new();
}

internal sealed class MechanismInfo
{
    public string Type { get; set; } = "generic";
    public bool Handled { get; set; } = true;
}

internal sealed class UserInfo
{
    public string? Id { get; set; }
    public string? Email { get; set; }
    public string? Username { get; set; }
}

internal sealed class IdentifyItem
{
    public string Type { get; set; } = "identify";
    public string DistinctId { get; set; } = string.Empty;
    public string? AnonymousId { get; set; }
    public Dictionary<string, object?> Traits { get; set; } = new();
    public string Timestamp { get; set; } = string.Empty;
}
