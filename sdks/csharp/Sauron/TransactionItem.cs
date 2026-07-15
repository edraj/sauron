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

    public string Timestamp { get; set; } = string.Empty;
}
