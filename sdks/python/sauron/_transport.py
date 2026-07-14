"""Buffered background HTTP transport.

An in-memory buffer is drained by a daemon thread every ``flush_interval``
seconds, or immediately once ``max_batch`` items accumulate. ``flush()`` drains
and sends synchronously on the calling thread; ``close()`` flushes and stops the
worker. Each flush builds exactly one envelope from the buffered items and POSTs
it via the injected ``sender`` (default: stdlib ``urllib``).
"""

from __future__ import annotations

import json
import threading
import time
import urllib.error
import urllib.request
from typing import Any, Callable, Dict, List, Optional

# A sender takes (url, headers, body_bytes) and returns the HTTP status code.
Sender = Callable[[str, Dict[str, str], bytes], int]


def urllib_sender(url: str, headers: Dict[str, str], body: bytes) -> int:
    """Default HTTP sender built on stdlib ``urllib.request``."""
    req = urllib.request.Request(url, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status
    except urllib.error.HTTPError as exc:
        # A non-2xx response still carries a status code we want to act on.
        return exc.code


class Transport:
    """A thread-safe, batching envelope sender."""

    def __init__(
        self,
        dsn: Any,
        make_envelope: Callable[[List[Dict[str, Any]]], Dict[str, Any]],
        sender: Optional[Sender] = None,
        flush_interval: float = 5.0,
        max_batch: int = 30,
        logger: Optional[Callable[..., None]] = None,
        on_disable: Optional[Callable[[], None]] = None,
        max_retries: int = 3,
        retry_base: float = 0.1,
    ) -> None:
        self._dsn = dsn
        self._make_envelope = make_envelope
        self._sender: Sender = sender or urllib_sender
        self._flush_interval = flush_interval
        self._max_batch = max_batch
        self._log = logger or (lambda *a, **k: None)
        self._on_disable = on_disable
        self._max_retries = max_retries
        self._retry_base = retry_base

        self._buffer: List[Dict[str, Any]] = []
        self._cond = threading.Condition()
        self._stop = False
        self._disabled = False
        self._started = False
        self._thread = threading.Thread(
            target=self._run, name="sauron-transport", daemon=True
        )

    # -- lifecycle ---------------------------------------------------------

    def start(self) -> None:
        if self._started:
            return
        self._started = True
        self._thread.start()

    def disable(self) -> None:
        """Stop accepting and sending (called on a hard auth failure)."""
        with self._cond:
            self._disabled = True
            self._buffer.clear()
            self._cond.notify_all()
        if self._on_disable is not None:
            self._on_disable()

    def close(self, timeout: Optional[float] = None) -> None:
        with self._cond:
            self._stop = True
            self._cond.notify_all()
        if self._started and self._thread.is_alive():
            self._thread.join(timeout)
        # Send anything still buffered on the caller's thread.
        self._flush_once()

    # -- producer side -----------------------------------------------------

    def capture(self, item: Dict[str, Any]) -> None:
        """Enqueue one envelope item. Non-blocking."""
        with self._cond:
            if self._disabled or self._stop:
                return
            self._buffer.append(item)
            if len(self._buffer) >= self._max_batch:
                self._cond.notify_all()

    def flush(self, timeout: Optional[float] = None) -> bool:
        """Send all buffered items synchronously. Returns True when done."""
        self._flush_once()
        return True

    # -- worker ------------------------------------------------------------

    def _run(self) -> None:
        while True:
            with self._cond:
                # Wake on: flush_interval elapsed, a full batch, or stop.
                if not self._should_wake():
                    self._cond.wait(self._flush_interval)
                stop = self._stop
            self._flush_once()
            if stop:
                return

    def _should_wake(self) -> bool:
        # Caller must hold the condition lock.
        return self._stop or len(self._buffer) >= self._max_batch

    def _drain(self) -> List[Dict[str, Any]]:
        with self._cond:
            if not self._buffer:
                return []
            items = self._buffer
            self._buffer = []
            return items

    def _flush_once(self) -> None:
        items = self._drain()
        if items:
            self._send(items)

    # -- send + retry ------------------------------------------------------

    def _send(self, items: List[Dict[str, Any]]) -> None:
        if self._disabled:
            return
        envelope = self._make_envelope(items)
        try:
            body = json.dumps(envelope, default=str).encode("utf-8")
        except (TypeError, ValueError) as exc:
            self._log("failed to serialize envelope, dropping", exc)
            return

        url = self._dsn.envelope_url
        headers = {
            "X-Sauron-Key": self._dsn.public_key,
            "Content-Type": "application/json",
        }

        delay = self._retry_base
        for attempt in range(self._max_retries + 1):
            status: Optional[int]
            try:
                status = self._sender(url, headers, body)
            except Exception as exc:  # network error — transient
                self._log("send failed", exc)
                status = None

            if status is not None and 200 <= status < 300:
                return

            if status in (401, 403):
                # Bad key — stop retrying and disable the client for good.
                self._log("auth rejected (status=%s), disabling" % status)
                self.disable()
                return

            # Transient (429 / 5xx / network error): retry with backoff.
            if attempt < self._max_retries:
                time.sleep(delay)
                delay *= 2
            else:
                self._log(
                    "dropping %d item(s) after %d attempts (last status=%s)"
                    % (len(items), self._max_retries + 1, status)
                )
