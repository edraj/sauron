using System;
using System.IO;
using System.Text;
using System.Threading.Tasks;
using Xunit;

namespace Sauron.Tests;

public class QueueTests
{
    private static byte[] Bytes(int n) => Encoding.UTF8.GetBytes(new string('x', n));

    private static string TempDir() => Path.Combine(Path.GetTempPath(), "sauron-q-" + Guid.NewGuid().ToString("N"));

    // ---- BoundedQueue unit ---------------------------------------------

    [Fact]
    public void OverCap_DropsOldest_KeepsBytesUnderMax()
    {
        var q = new BoundedQueue(maxBytes: 300, offlineDir: null);
        q.Push(Bytes(100)); // A
        q.Push(Bytes(100)); // B
        q.Push(Bytes(100)); // C -> 300 (== cap, retained)
        q.Push(Bytes(100)); // D -> 400 > 300, drop oldest (A)

        Assert.True(q.Bytes <= 300);
        var items = q.Drain();
        Assert.Equal(3, items.Count); // A was dropped
    }

    [Fact]
    public void SingleItemLargerThanCap_IsDropped()
    {
        var q = new BoundedQueue(maxBytes: 50, offlineDir: null);
        q.Push(Bytes(100));

        Assert.True(q.Bytes <= 50);
        Assert.Equal(0, q.Count);
    }

    [Fact]
    public void Drain_ReturnsFifoOrder_AndClears()
    {
        var q = new BoundedQueue(1_000_000, null);
        q.Push(Encoding.UTF8.GetBytes("first"));
        q.Push(Encoding.UTF8.GetBytes("second"));

        var items = q.Drain();
        Assert.Equal("first", Encoding.UTF8.GetString(items[0].Payload));
        Assert.Equal("second", Encoding.UTF8.GetString(items[1].Payload));
        Assert.Equal(0, q.Count);
    }

    // ---- Disk persistence ----------------------------------------------

    [Fact]
    public void Disk_PersistAndReload_RoundTripsFifo()
    {
        var dir = TempDir();
        try
        {
            var q1 = new BoundedQueue(1_000_000, dir);
            q1.Push(Encoding.UTF8.GetBytes("first"));
            q1.Push(Encoding.UTF8.GetBytes("second"));

            // fresh instance simulates a process restart
            var q2 = new BoundedQueue(1_000_000, dir);
            var items = q2.Drain();

            Assert.Equal(2, items.Count);
            Assert.Equal("first", Encoding.UTF8.GetString(items[0].Payload));
            Assert.Equal("second", Encoding.UTF8.GetString(items[1].Payload));
            Assert.Empty(Directory.GetFiles(dir)); // drained -> removed from disk
        }
        finally
        {
            if (Directory.Exists(dir)) Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void Disk_Ack_RemovesFile()
    {
        var dir = TempDir();
        try
        {
            var q = new BoundedQueue(1_000_000, dir);
            q.Push(Encoding.UTF8.GetBytes("payload"));
            Assert.Single(Directory.GetFiles(dir));

            var entry = q.Snapshot()[0];
            q.Ack(entry);

            Assert.Empty(Directory.GetFiles(dir));
            Assert.Equal(0, q.Count);
        }
        finally
        {
            if (Directory.Exists(dir)) Directory.Delete(dir, recursive: true);
        }
    }

    // ---- Transport integration -----------------------------------------

    [Fact]
    public void OfflineDir_RetainsUndeliveredEnvelope_ThenDrainsOnRecovery()
    {
        var dir = TempDir();
        try
        {
            // Ingest is down: persistent 500. The envelope is retained (and on disk).
            var down = new ScriptedHandler(
                ScriptedHandler.Status(500),
                ScriptedHandler.Status(500),
                ScriptedHandler.Status(500),
                ScriptedHandler.Status(500),
                ScriptedHandler.Status(500));
            var c1 = new SauronClient(new SauronOptions
            {
                Dsn = "https://pub123@example.com/42",
                HttpMessageHandler = down,
                FlushInterval = TimeSpan.FromHours(1),
                MaxBatch = 1000,
                OfflineDir = dir,
                DelayHook = _ => Task.CompletedTask,
            });

            c1.Track("evt", "u1");
            c1.Flush();

            Assert.Single(Directory.GetFiles(dir)); // undelivered, persisted

            // Restart with a healthy ingest: reload from disk and deliver.
            var up = new CapturingHandler();
            var c2 = new SauronClient(new SauronOptions
            {
                Dsn = "https://pub123@example.com/42",
                HttpMessageHandler = up,
                FlushInterval = TimeSpan.FromHours(1),
                MaxBatch = 1000,
                OfflineDir = dir,
                DelayHook = _ => Task.CompletedTask,
            });

            c2.Flush(); // buffer empty, but the disk queue was reloaded

            Assert.Equal(1, up.RequestCount);
            Assert.Empty(Directory.GetFiles(dir)); // delivered -> removed
        }
        finally
        {
            if (Directory.Exists(dir)) Directory.Delete(dir, recursive: true);
        }
    }
}
