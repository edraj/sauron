"""A bounded, thread-safe outbound queue with opt-in disk persistence.

Default behavior is an in-memory FIFO ring bounded by a byte budget
(``max_bytes``): once the pending payload exceeds the budget the oldest entries
are dropped. This gives a server a practical "offline" story — survive a
transient ingest outage without unbounded memory growth.

When ``offline_path`` is set, every pending item is also written to a file under
that directory (one file per item, FIFO-ordered by filename). A fresh instance
over the same directory reloads the pending items on construction, and
:meth:`confirm` deletes the files once a batch has been delivered — giving
at-least-once delivery across process restarts. Disk persistence is off by
default (ephemeral containers shouldn't be forced into filesystem assumptions).
"""

from __future__ import annotations

import json
import os
import threading
import time
from collections import deque
from typing import Any, Deque, Dict, List, Mapping, Optional

# 1 MiB: matches the js/flutter default queue budget.
DEFAULT_MAX_QUEUE_BYTES = 1_048_576

_SUFFIX = ".json"


class QueueEntry:
    """One pending item plus its on-disk bookkeeping."""

    __slots__ = ("item", "size", "filename")

    def __init__(
        self, item: Dict[str, Any], size: int, filename: Optional[str]
    ) -> None:
        self.item = item
        self.size = size
        self.filename = filename


class BoundedQueue:
    """A byte-bounded FIFO of envelope items with optional disk persistence."""

    def __init__(
        self,
        *,
        max_bytes: int = DEFAULT_MAX_QUEUE_BYTES,
        offline_path: Optional[str] = None,
    ) -> None:
        self._max_bytes = max_bytes
        self._offline_path = offline_path
        self._entries: Deque[QueueEntry] = deque()
        self._bytes = 0
        self._seq = 0
        self._lock = threading.Lock()

        if self._offline_path:
            os.makedirs(self._offline_path, exist_ok=True)
            self._reload()

    # -- producer ----------------------------------------------------------

    def push(self, item: Mapping[str, Any]) -> None:
        """Append an item, persisting it (if enabled) and enforcing the cap."""
        item = dict(item)
        payload = _encode(item)
        with self._lock:
            filename = self._persist(payload)
            entry = QueueEntry(item=item, size=len(payload), filename=filename)
            self._entries.append(entry)
            self._bytes += entry.size
            self._evict_over_cap()

    # -- consumer ----------------------------------------------------------

    def drain(self) -> List[QueueEntry]:
        """Remove and return every pending entry (memory only).

        On-disk files are retained until :meth:`confirm` — so a send that fails
        (or a crash mid-flight) leaves them for the next process to recover.
        """
        with self._lock:
            entries = list(self._entries)
            self._entries.clear()
            self._bytes = 0
            return entries

    def confirm(self, entries: List[QueueEntry]) -> None:
        """Delete the persisted files backing ``entries`` (after delivery)."""
        if not self._offline_path:
            return
        for entry in entries:
            self._remove_file(entry.filename)

    def clear(self) -> None:
        """Drop everything from memory and disk (e.g. on a hard disable)."""
        with self._lock:
            entries = list(self._entries)
            self._entries.clear()
            self._bytes = 0
        for entry in entries:
            self._remove_file(entry.filename)

    # -- introspection -----------------------------------------------------

    @property
    def bytes(self) -> int:
        return self._bytes

    def __len__(self) -> int:
        return len(self._entries)

    def snapshot_items(self) -> List[Dict[str, Any]]:
        """The pending items in FIFO order (test/inspection aid)."""
        with self._lock:
            return [dict(e.item) for e in self._entries]

    # -- internals ---------------------------------------------------------

    def _evict_over_cap(self) -> None:
        # Caller holds the lock. Drop oldest until within budget, but always
        # keep at least the newest entry (a lone oversized item still ships).
        while self._bytes > self._max_bytes and len(self._entries) > 1:
            victim = self._entries.popleft()
            self._bytes -= victim.size
            self._remove_file(victim.filename)

    def _persist(self, payload: bytes) -> Optional[str]:
        # Caller holds the lock.
        if not self._offline_path:
            return None
        # A time-then-sequence prefix keeps files FIFO-sortable across restarts.
        self._seq += 1
        filename = "%020d_%08d%s" % (time.time_ns(), self._seq, _SUFFIX)
        path = os.path.join(self._offline_path, filename)
        try:
            with open(path, "wb") as fh:
                fh.write(payload)
        except OSError:
            # Persistence is best-effort; the in-memory copy still ships.
            return None
        return filename

    def _remove_file(self, filename: Optional[str]) -> None:
        if not filename or not self._offline_path:
            return
        try:
            os.remove(os.path.join(self._offline_path, filename))
        except FileNotFoundError:
            pass
        except OSError:
            pass

    def _reload(self) -> None:
        # Caller is __init__ (single-threaded). Recover pending items FIFO.
        try:
            names = sorted(
                n for n in os.listdir(self._offline_path) if n.endswith(_SUFFIX)
            )
        except OSError:
            return
        for name in names:
            path = os.path.join(self._offline_path, name)
            try:
                with open(path, "rb") as fh:
                    payload = fh.read()
                item = json.loads(payload.decode("utf-8"))
            except (OSError, ValueError):
                # A corrupt/partial file can't be recovered — discard it.
                self._remove_file(name)
                continue
            entry = QueueEntry(item=item, size=len(payload), filename=name)
            self._entries.append(entry)
            self._bytes += entry.size
        # Reloaded files may already exceed the budget (e.g. a smaller cap on
        # restart); trim to fit.
        self._evict_over_cap()


def _encode(item: Mapping[str, Any]) -> bytes:
    return json.dumps(item, default=str).encode("utf-8")
