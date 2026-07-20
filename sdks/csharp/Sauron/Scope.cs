using System;
using System.Collections.Generic;
using System.Threading;

namespace Sauron;

/// <summary>
/// A layer of attribution — user, tags, contexts, extra and a bounded breadcrumb ring —
/// merged onto captured errors. A process-wide global scope holds defaults; per-request
/// isolated scopes are layered over it via <see cref="ScopeManager"/> (<c>AsyncLocal</c>).
/// Reads merge child-over-parent because a child is a <see cref="Clone"/> of its parent.
/// </summary>
internal sealed class Scope
{
    public SauronUser? User { get; set; }
    public Dictionary<string, string> Tags { get; } = new();
    public Dictionary<string, object?> Contexts { get; } = new();
    public Dictionary<string, object?> Extra { get; } = new();
    public List<Breadcrumb> Breadcrumbs { get; } = new();

    public void SetUser(SauronUser? user) => User = user;

    public void SetTag(string key, string value)
    {
        if (string.IsNullOrEmpty(key)) return;
        Tags[key] = value;
    }

    public void SetTags(IReadOnlyDictionary<string, string> tags)
    {
        if (tags is null) return;
        foreach (var kv in tags)
            SetTag(kv.Key, kv.Value);
    }

    public void SetContext(string key, object? value)
    {
        if (string.IsNullOrEmpty(key)) return;
        Contexts[key] = value;
    }

    public void SetExtra(string key, object? value)
    {
        if (string.IsNullOrEmpty(key)) return;
        Extra[key] = value;
    }

    /// <summary>Append a breadcrumb, dropping the oldest once the ring exceeds <paramref name="max"/>.</summary>
    public void AddBreadcrumb(Breadcrumb breadcrumb, int max)
    {
        if (breadcrumb is null) return;
        Breadcrumbs.Add(breadcrumb);
        if (max < 0) max = 0;
        while (Breadcrumbs.Count > max)
            Breadcrumbs.RemoveAt(0);
    }

    /// <summary>
    /// Merge this scope onto an outgoing error item. Per-call values already present on the
    /// item win over scope defaults (tags via <see cref="Dictionary{TKey,TValue}"/> insert-if-absent,
    /// user only when the item has none). Breadcrumbs are appended in order.
    /// </summary>
    public void ApplyToError(ErrorItem item)
    {
        foreach (var kv in Tags)
            item.Tags.TryAdd(kv.Key, kv.Value);

        if (Contexts.Count > 0)
        {
            item.Contexts ??= new Dictionary<string, object?>();
            foreach (var kv in Contexts)
                item.Contexts.TryAdd(kv.Key, kv.Value); // per-call block name wins
        }
        if (item.Contexts is { Count: 0 })
            item.Contexts = null; // omit-when-empty

        if (Extra.Count > 0)
        {
            item.Extra ??= new Dictionary<string, object?>();
            foreach (var kv in Extra)
                item.Extra.TryAdd(kv.Key, kv.Value); // per-call key wins
        }
        if (item.Extra is { Count: 0 })
            item.Extra = null; // omit-when-empty

        if (User is not null && item.User is null)
            item.User = new UserInfo { Id = User.Id, Email = User.Email, Username = User.Username };

        foreach (var crumb in Breadcrumbs)
            item.Breadcrumbs.Add(ToWire(crumb));
    }

    /// <summary>Merge this scope's tags/contexts/extra onto an outgoing analytics event.
    /// Per-call values already on the item win (tags/extra by key, contexts by block name).
    /// Empty results are normalized to null so they are omitted from the wire.</summary>
    public void ApplyToEvent(EventItem item)
    {
        if (Tags.Count > 0)
        {
            item.Tags ??= new Dictionary<string, object?>();
            foreach (var kv in Tags)
                item.Tags.TryAdd(kv.Key, kv.Value);
        }
        if (Contexts.Count > 0)
        {
            item.Contexts ??= new Dictionary<string, object?>();
            foreach (var kv in Contexts)
                item.Contexts.TryAdd(kv.Key, kv.Value);
        }
        if (Extra.Count > 0)
        {
            item.Extra ??= new Dictionary<string, object?>();
            foreach (var kv in Extra)
                item.Extra.TryAdd(kv.Key, kv.Value);
        }
        if (item.Tags is { Count: 0 }) item.Tags = null;
        if (item.Contexts is { Count: 0 }) item.Contexts = null;
        if (item.Extra is { Count: 0 }) item.Extra = null;
    }

    private static BreadcrumbWire ToWire(Breadcrumb b) => new()
    {
        Type = string.IsNullOrEmpty(b.Type) ? "default" : b.Type,
        Category = b.Category,
        Message = b.Message,
        Level = b.Level,
        Timestamp = b.Timestamp.ToString("O"),
        Data = b.Data is null ? new Dictionary<string, object?>() : new Dictionary<string, object?>(b.Data),
    };

    /// <summary>A deep-enough copy: independent tag/context/extra maps and breadcrumb list.</summary>
    public Scope Clone()
    {
        var copy = new Scope { User = User };
        foreach (var kv in Tags) copy.Tags[kv.Key] = kv.Value;
        foreach (var kv in Contexts) copy.Contexts[kv.Key] = kv.Value;
        foreach (var kv in Extra) copy.Extra[kv.Key] = kv.Value;
        copy.Breadcrumbs.AddRange(Breadcrumbs);
        return copy;
    }

    public void Clear()
    {
        User = null;
        Tags.Clear();
        Contexts.Clear();
        Extra.Clear();
        Breadcrumbs.Clear();
    }
}

/// <summary>
/// Ambient scope hub. <see cref="Current"/> is the async-local scope when one is pushed,
/// otherwise the process-wide <see cref="Global"/>. <see cref="PushScope"/> layers an
/// isolated clone for the duration of a <c>using</c> block (per-request isolation).
/// </summary>
internal static class ScopeManager
{
    private static readonly AsyncLocal<Scope?> _current = new();

    /// <summary>Process-wide defaults set once at/after init.</summary>
    public static Scope Global { get; } = new();

    /// <summary>The active scope: the pushed async-local scope, or the global one.</summary>
    public static Scope Current => _current.Value ?? Global;

    /// <summary>
    /// Push an isolated clone of the current scope and make it active until the returned
    /// handle is disposed, restoring the previous scope.
    /// </summary>
    public static IDisposable PushScope()
    {
        var previous = _current.Value;
        _current.Value = Current.Clone();
        return new ScopeHandle(previous);
    }

    /// <summary>Test hook: reset the process-global scope and clear any async-local override.</summary>
    internal static void ResetForTests()
    {
        Global.Clear();
        _current.Value = null;
    }

    private sealed class ScopeHandle : IDisposable
    {
        private readonly Scope? _previous;
        private bool _disposed;

        public ScopeHandle(Scope? previous) => _previous = previous;

        public void Dispose()
        {
            if (_disposed) return;
            _disposed = true;
            _current.Value = _previous;
        }
    }
}
