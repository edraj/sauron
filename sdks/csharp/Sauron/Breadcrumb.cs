using System;
using System.Collections.Generic;

namespace Sauron;

/// <summary>
/// A structured breadcrumb — a trail entry recorded on the active <see cref="Scope"/> and
/// attached to errors captured afterwards. Shape matches the ingest wire
/// <c>Breadcrumb</c> (<c>type</c>, <c>category</c>, <c>message</c>, <c>level</c>,
/// <c>timestamp</c>, <c>data</c>).
/// </summary>
public sealed class Breadcrumb
{
    /// <summary>Kind of breadcrumb (e.g. <c>navigation</c>, <c>http</c>, <c>log</c>). Defaults to <c>default</c>.</summary>
    public string Type { get; set; } = "default";

    /// <summary>Optional grouping category (e.g. <c>auth</c>, <c>ui.click</c>).</summary>
    public string? Category { get; set; }

    /// <summary>Human-readable message.</summary>
    public string? Message { get; set; }

    /// <summary>Severity: <c>debug|info|warning|error|fatal</c>.</summary>
    public string? Level { get; set; }

    /// <summary>When the breadcrumb happened. Defaults to now (UTC).</summary>
    public DateTimeOffset Timestamp { get; set; } = DateTimeOffset.UtcNow;

    /// <summary>Free-form structured payload.</summary>
    public Dictionary<string, object?>? Data { get; set; }
}
