using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;

namespace Sauron;

/// <summary>One queued, serialized envelope awaiting delivery, plus its FIFO id and backing file.</summary>
internal sealed class QueueEntry
{
    public long Id { get; }
    public byte[] Payload { get; }

    /// <summary>Backing file when disk persistence is on; null for in-memory entries.</summary>
    public string? File { get; }

    public QueueEntry(long id, byte[] payload, string? file)
    {
        Id = id;
        Payload = payload;
        File = file;
    }
}

/// <summary>
/// A bounded, FIFO buffer of pending envelopes — the transient-outage store. It is byte-capped
/// (<c>maxBytes</c>): pushing past the cap drops the oldest entries so memory can't grow without
/// bound. When an <c>offlineDir</c> is supplied, entries are also persisted to disk (one file each,
/// named by a zero-padded sequence for FIFO ordering), reloaded on construction, and deleted once
/// acknowledged — giving at-least-once delivery across process restarts.
/// </summary>
internal sealed class BoundedQueue
{
    private const string Ext = ".env";

    private readonly int _maxBytes;
    private readonly string? _dir;
    private readonly object _gate = new();
    private readonly LinkedList<QueueEntry> _entries = new();
    private long _bytes;
    private long _seq;

    public BoundedQueue(int maxBytes, string? offlineDir)
    {
        _maxBytes = maxBytes < 0 ? 0 : maxBytes;
        _dir = string.IsNullOrEmpty(offlineDir) ? null : offlineDir;

        if (_dir is not null)
        {
            try { Directory.CreateDirectory(_dir); }
            catch { _dir = null; } // if the dir is unusable, fall back to memory-only
            if (_dir is not null)
                LoadFromDisk();
        }
    }

    /// <summary>Total bytes currently retained.</summary>
    public long Bytes { get { lock (_gate) { return _bytes; } } }

    /// <summary>Number of entries currently retained.</summary>
    public int Count { get { lock (_gate) { return _entries.Count; } } }

    /// <summary>Append an envelope payload, persisting it and evicting the oldest over the cap.</summary>
    public void Push(byte[] payload)
    {
        if (payload is null)
            return;

        lock (_gate)
        {
            long id = ++_seq;
            string? file = null;
            if (_dir is not null)
            {
                file = Path.Combine(_dir, id.ToString("D20") + Ext);
                try { System.IO.File.WriteAllBytes(file, payload); }
                catch { file = null; }
            }

            _entries.AddLast(new QueueEntry(id, payload, file));
            _bytes += payload.Length;
            EnforceCap();
        }
    }

    /// <summary>A FIFO snapshot of the current entries (for a drain pass); does not mutate the queue.</summary>
    public IReadOnlyList<QueueEntry> Snapshot()
    {
        lock (_gate) { return _entries.ToList(); }
    }

    /// <summary>Remove a delivered (or permanently-dropped) entry and delete its backing file.</summary>
    public void Ack(QueueEntry entry)
    {
        if (entry is null)
            return;

        lock (_gate)
        {
            var node = _entries.Find(entry);
            if (node is not null)
            {
                _entries.Remove(node);
                _bytes -= entry.Payload.Length;
                if (_bytes < 0) _bytes = 0;
            }
            DeleteFile(entry);
        }
    }

    /// <summary>Remove and return every entry (FIFO), deleting all backing files.</summary>
    public IReadOnlyList<QueueEntry> Drain()
    {
        lock (_gate)
        {
            var all = _entries.ToList();
            foreach (var e in all)
                DeleteFile(e);
            _entries.Clear();
            _bytes = 0;
            return all;
        }
    }

    private void EnforceCap()
    {
        while (_bytes > _maxBytes && _entries.First is { } first)
        {
            _entries.RemoveFirst();
            _bytes -= first.Value.Payload.Length;
            if (_bytes < 0) _bytes = 0;
            DeleteFile(first.Value);
        }
    }

    private static void DeleteFile(QueueEntry entry)
    {
        if (entry.File is null)
            return;
        try { if (System.IO.File.Exists(entry.File)) System.IO.File.Delete(entry.File); }
        catch { /* best-effort */ }
    }

    private void LoadFromDisk()
    {
        if (_dir is null)
            return;

        string[] files;
        try { files = Directory.GetFiles(_dir, "*" + Ext); }
        catch { return; }

        Array.Sort(files, StringComparer.Ordinal); // zero-padded ids sort FIFO

        foreach (var f in files)
        {
            byte[] payload;
            try { payload = System.IO.File.ReadAllBytes(f); }
            catch { continue; }

            long id = ParseId(Path.GetFileNameWithoutExtension(f)) ?? (_seq + 1);
            if (id > _seq) _seq = id;

            _entries.AddLast(new QueueEntry(id, payload, f));
            _bytes += payload.Length;
        }

        EnforceCap();
    }

    private static long? ParseId(string name)
        => long.TryParse(name, out var v) ? v : (long?)null;
}
