using System.Text.Json.Serialization;

namespace Sauron;

/// <summary>
/// Wire DTO for a performance transaction — one timed operation. Serialized with the
/// shared snake_case policy (<c>duration_ms</c>, <c>http_method</c>, <c>http_status</c>,
/// <c>distinct_id</c>, <c>session_id</c>). Matches the ingest <c>TransactionItem</c>.
/// </summary>
internal sealed class TransactionItem
{
    public string Type { get; set; } = "transaction";

    /// <summary>Route / screen / operation label (the grouping key).</summary>
    public string Name { get; set; } = string.Empty;

    /// <summary>Operation class: <c>navigation|http|resource|screen_load|custom</c>.</summary>
    public string Op { get; set; } = "custom";

    public double DurationMs { get; set; }

    public string? Status { get; set; }
    public string? HttpMethod { get; set; }
    public int? HttpStatus { get; set; }
    public string? Url { get; set; }
    public string? DistinctId { get; set; }
    public string? SessionId { get; set; }

    // Id/name of the bounding workflow (StartWorkflow/EndWorkflow/CancelWorkflow), if any.
    // Omitted (never `null`) when no workflow is active.
    //
    // ALWAYS SET AS A PAIR, never one without the other. The server guards on
    // `if let (Some(id), Some(name))` (backend/crates/sauron-pipeline/src/process.rs), so an
    // id without a name — or a name without an id — is SILENTLY dropped from every workflow
    // query, with nothing erroring. SauronClient.StampWorkflow is the only assignment site
    // and always sets both from a single non-null ActiveWorkflow; keep it that way.
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? WorkflowId { get; set; }

    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? WorkflowName { get; set; }

    public string Timestamp { get; set; } = string.Empty;
}
