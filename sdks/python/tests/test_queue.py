"""Bounded in-memory queue + opt-in disk persistence (task B7).

- Default: an in-memory ring bounded by ``max_bytes`` (drop-oldest).
- Opt-in ``offline_path``: persist each item FIFO to disk, reload on init, and
  delete on successful send (at-least-once across restarts).
"""

import json
import os

from sauron._queue import BoundedQueue
from sauron._transport import Transport


def _item(n, blob="x" * 100):
    return {"type": "event", "name": f"e{n}", "distinct_id": "u", "blob": blob}


def _item_size(item):
    return len(json.dumps(item, default=str).encode("utf-8"))


# -- in-memory byte bound -------------------------------------------------


def test_drop_oldest_over_cap_keeps_bytes_bounded():
    size = _item_size(_item(0))
    cap = size * 2 + 10  # room for ~2 items
    q = BoundedQueue(max_bytes=cap)
    for n in range(5):
        q.push(_item(n))
    names = [i["name"] for i in q.snapshot_items()]
    assert q.bytes <= cap
    # Oldest were dropped; the newest survives.
    assert names[-1] == "e4"
    assert "e0" not in names
    assert "e1" not in names


def test_len_and_bytes_track_contents():
    q = BoundedQueue(max_bytes=1_000_000)
    assert len(q) == 0 and q.bytes == 0
    q.push(_item(0))
    q.push(_item(1))
    assert len(q) == 2
    assert q.bytes == _item_size(_item(0)) + _item_size(_item(1))


def test_drain_empties_memory_and_preserves_order():
    q = BoundedQueue(max_bytes=1_000_000)
    q.push(_item(0))
    q.push(_item(1))
    entries = q.drain()
    assert [e.item["name"] for e in entries] == ["e0", "e1"]
    assert len(q) == 0 and q.bytes == 0


# -- opt-in disk persistence ----------------------------------------------


def test_memory_only_writes_nothing_to_disk(tmp_path):
    q = BoundedQueue(max_bytes=1_000_000)  # no offline_path
    q.push(_item(0))
    assert os.listdir(str(tmp_path)) == []


def test_disk_roundtrip_reloads_fifo(tmp_path):
    d = str(tmp_path)
    q = BoundedQueue(offline_path=d)
    q.push(_item(1))
    q.push(_item(2))
    q.push(_item(3))
    # A fresh instance over the same dir recovers the pending items in order.
    fresh = BoundedQueue(offline_path=d)
    names = [i["name"] for i in fresh.snapshot_items()]
    assert names == ["e1", "e2", "e3"]
    assert len(fresh) == 3


def test_confirm_deletes_persisted_files(tmp_path):
    d = str(tmp_path)
    q = BoundedQueue(offline_path=d)
    q.push(_item(1))
    q.push(_item(2))
    assert len(os.listdir(d)) == 2
    entries = q.drain()
    # Drain removes from memory but keeps the files until confirmed.
    assert len(os.listdir(d)) == 2
    q.confirm(entries)
    assert os.listdir(d) == []
    # And a fresh instance recovers nothing.
    assert BoundedQueue(offline_path=d).snapshot_items() == []


def test_over_cap_eviction_deletes_disk_file(tmp_path):
    d = str(tmp_path)
    size = _item_size(_item(0))
    q = BoundedQueue(max_bytes=size * 2 + 10, offline_path=d)
    for n in range(5):
        q.push(_item(n))
    # Only the survivors remain persisted.
    assert len(os.listdir(d)) == len(q)
    recovered = [i["name"] for i in BoundedQueue(offline_path=d).snapshot_items()]
    assert recovered == [i["name"] for i in q.snapshot_items()]


def test_clear_removes_memory_and_disk(tmp_path):
    d = str(tmp_path)
    q = BoundedQueue(offline_path=d)
    q.push(_item(1))
    q.clear()
    assert len(q) == 0
    assert os.listdir(d) == []


# -- transport integration ------------------------------------------------


class _StubDsn:
    envelope_url = "https://localhost:8081/api/1/envelope"
    public_key = "pk_test"


def _make_envelope(items):
    return {
        "header": {"sdk": {"name": "sauron-python", "version": "0.1.0"}},
        "context": {},
        "items": items,
    }


def test_transport_deletes_offline_files_on_successful_send(tmp_path):
    d = str(tmp_path)

    def ok_sender(url, headers, body):
        return 200

    t = Transport(
        _StubDsn(),
        _make_envelope,
        sender=ok_sender,
        flush_interval=3600,
        offline_path=d,
    )
    t.capture(_item(1))
    assert len(os.listdir(d)) == 1  # persisted immediately on capture
    t.flush()
    assert os.listdir(d) == []  # removed after delivery
    t.close(timeout=2)


def test_transport_recovers_offline_batch_across_restart(tmp_path):
    d = str(tmp_path)

    def fail_sender(url, headers, body):
        return 503

    # First process: send fails, so the item stays on disk.
    t1 = Transport(
        _StubDsn(),
        _make_envelope,
        sender=fail_sender,
        flush_interval=3600,
        offline_path=d,
        max_retries=0,
        sleep=lambda d: None,
    )
    t1.capture(_item(7))
    t1.flush()
    t1.close(timeout=2)
    assert len(os.listdir(d)) == 1  # still pending on disk

    # Second process: reloads the pending item and delivers it.
    delivered = []

    def ok_sender(url, headers, body):
        delivered.append(json.loads(body.decode("utf-8")))
        return 200

    t2 = Transport(
        _StubDsn(),
        _make_envelope,
        sender=ok_sender,
        flush_interval=3600,
        offline_path=d,
    )
    t2.flush()
    t2.close(timeout=2)
    assert len(delivered) == 1
    assert delivered[0]["items"][0]["name"] == "e7"
    assert os.listdir(d) == []
