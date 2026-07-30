"""Buffered background HTTP transport.

An in-memory buffer is drained by a daemon thread every ``flush_interval``
seconds, or immediately once ``max_batch`` items accumulate. ``flush()`` drains
and sends synchronously on the calling thread; ``close()`` flushes and stops the
worker. Each flush builds exactly one envelope from the buffered items and POSTs
it via the injected ``sender`` (default: stdlib ``urllib``).
"""

from __future__ import annotations

import json
import random
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
from typing import Any, Callable, Dict, List, Mapping, Optional, Tuple

from ._gzip import DEFAULT_GZIP_THRESHOLD_BYTES, maybe_gzip
from ._queue import DEFAULT_MAX_QUEUE_BYTES, BoundedQueue, QueueEntry

# A sender takes (url, headers, body_bytes) and returns either the HTTP status
# code, or a ``(status, response_headers)`` tuple so the transport can read
# ``Retry-After``. Returning ``None`` (or raising) is treated as a network error.
Sender = Callable[[str, Dict[str, str], bytes], Any]

# Backoff never waits longer than this, even honoring a large ``Retry-After``.
MAX_BACKOFF_SECONDS = 30.0

# Non-2xx statuses we retry (in addition to network errors and any 5xx).
# 413 is absent on purpose: the body is what the server rejected, so resending
# it unchanged can only fail the same way. It is handled by shrinking instead.
_RETRYABLE_STATUSES = frozenset({408, 429})

# Ceiling on items per envelope, matching the server's own limit. Exceeding it
# is a non-retryable 400, so an unbounded envelope could never be delivered.
MAX_ITEMS_PER_ENVELOPE = 1000

# Outcomes of one send attempt, which the caller needs to tell apart:
# a rejected envelope must have its persisted copies deleted (they would poison
# every future process), while a transient failure must keep them for retry.
_DELIVERED = "delivered"
_REJECTED = "rejected"
_TRANSIENT = "transient"


def urllib_sender(
    url: str, headers: Dict[str, str], body: bytes
) -> Tuple[int, Dict[str, str]]:
    """Default HTTP sender built on stdlib ``urllib.request``.

    Returns ``(status, response_headers)`` so the retry policy can honor a
    ``Retry-After`` header on 429s.
    """
    req = urllib.request.Request(url, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, dict(resp.headers)
    except urllib.error.HTTPError as exc:
        # A non-2xx response still carries a status + headers we act on.
        return exc.code, dict(exc.headers or {})


def _normalize_result(result: Any) -> Tuple[Optional[int], Dict[str, str]]:
    """Coerce a sender return into ``(status, headers)``.

    Accepts a bare status ``int`` (legacy senders) or a ``(status, headers)``
    tuple. ``None`` → ``(None, {})`` (network error).
    """
    if result is None:
        return None, {}
    if isinstance(result, tuple):
        status = result[0]
        headers = result[1] if len(result) > 1 else {}
        return status, dict(headers or {})
    return result, {}


def _header(headers: Mapping[str, str], name: str) -> Optional[str]:
    """Case-insensitive header lookup."""
    if not headers:
        return None
    target = name.lower()
    for key, value in headers.items():
        if str(key).lower() == target:
            return value
    return None


def _parse_retry_after(value: Optional[str]) -> Optional[float]:
    """Parse a ``Retry-After`` value (delta-seconds or HTTP-date) → seconds."""
    if value is None:
        return None
    text = str(value).strip()
    if not text:
        return None
    # delta-seconds form.
    try:
        return max(0.0, float(text))
    except ValueError:
        pass
    # HTTP-date form.
    try:
        when = parsedate_to_datetime(text)
    except (TypeError, ValueError):
        return None
    if when is None:
        return None
    if when.tzinfo is None:
        when = when.replace(tzinfo=timezone.utc)
    delta = (when - datetime.now(timezone.utc)).total_seconds()
    return max(0.0, delta)


def _is_retryable(status: Optional[int]) -> bool:
    """Transient failures worth retrying: network errors, 408/413/429, any 5xx."""
    if status is None:
        return True
    if status in _RETRYABLE_STATUSES:
        return True
    return 500 <= status <= 599


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
        gzip_threshold_bytes: int = DEFAULT_GZIP_THRESHOLD_BYTES,
        max_queue_bytes: int = DEFAULT_MAX_QUEUE_BYTES,
        offline_path: Optional[str] = None,
        sleep: Optional[Callable[[float], None]] = None,
        rand: Optional[Callable[[], float]] = None,
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
        self._gzip_threshold = gzip_threshold_bytes
        # Injectable for deterministic tests; default to the real clock/RNG.
        self._sleep = sleep or time.sleep
        self._rand = rand or random.random

        # Pending items live in a byte-bounded queue (optionally disk-backed).
        self._queue = BoundedQueue(
            max_bytes=max_queue_bytes, offline_path=offline_path
        )
        self._cond = threading.Condition()
        self._stop = False
        self._disabled = False
        self._started = False
        self._thread = threading.Thread(
            target=self._run, name="sauron-transport", daemon=True
        )

    # -- lifecycle ---------------------------------------------------------

    @property
    def disabled(self) -> bool:
        """Whether :meth:`disable` has run (permanent for this transport's
        lifetime — there is no way back short of constructing a new one).

        Read without the condition lock, matching the existing unlocked read
        of ``self._disabled`` in :meth:`_send`: a plain ``bool`` attribute
        read/write is already atomic under the GIL, and the only writer
        (:meth:`disable`) only ever flips it one-way, so a caller can never
        observe a torn or stale-then-reverted value.
        """
        return self._disabled

    def start(self) -> None:
        if self._started:
            return
        self._started = True
        self._thread.start()

    def disable(self) -> None:
        """Stop accepting and sending (called on a hard auth failure)."""
        with self._cond:
            self._disabled = True
            self._queue.clear()
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
            self._queue.push(item)
            if len(self._queue) >= self._max_batch:
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
        return self._stop or len(self._queue) >= self._max_batch

    def _drain(self) -> List[QueueEntry]:
        with self._cond:
            return self._queue.drain(MAX_ITEMS_PER_ENVELOPE)

    def _flush_once(self) -> None:
        # Loop so a backlog larger than one envelope still drains fully rather
        # than trickling out one envelope per tick.
        while True:
            entries = self._drain()
            if not entries:
                return
            outcome = self._send([e.item for e in entries])
            if outcome == _DELIVERED:
                # Delivered — drop the persisted copies (if any).
                self._queue.confirm(entries)
                continue
            if outcome == _REJECTED:
                # The server refused this envelope and would refuse it again;
                # the payload itself is the problem. Delete the persisted copies
                # too, otherwise every future process reloads the same poison
                # from disk, resends it, and is rejected forever.
                self._queue.confirm(entries)
            # Transient: the in-memory copies are gone, but any persisted files
            # remain so the next process can recover them (at-least-once).
            return

    # -- send + retry ------------------------------------------------------

    def _send(self, items: List[Dict[str, Any]]) -> str:
        """POST one envelope, applying the shared retry policy.

        Returns one of :data:`_DELIVERED`, :data:`_REJECTED` (the server will
        never accept this payload) or :data:`_TRANSIENT` (worth another try
        later). The caller uses that to decide whether the persisted copies
        should be deleted or kept.
        """
        if self._disabled:
            return _REJECTED
        envelope = self._make_envelope(items)
        try:
            body = json.dumps(envelope, default=str).encode("utf-8")
        except (TypeError, ValueError) as exc:
            # Unserializable payload: retrying cannot help.
            self._log("failed to serialize envelope, dropping", exc)
            return _REJECTED

        body, gzip_headers = maybe_gzip(body, self._gzip_threshold)

        url = self._dsn.envelope_url
        headers = {
            "X-Sauron-Key": self._dsn.public_key,
            "Content-Type": "application/json",
        }
        headers.update(gzip_headers)

        attempt = 0
        while True:
            try:
                result: Any = self._sender(url, headers, body)
            except Exception as exc:  # network error — transient
                self._log("send failed", exc)
                result = None
            status, resp_headers = _normalize_result(result)

            # Delivered.
            if status is not None and 200 <= status < 300:
                return _DELIVERED

            # Bad key — stop retrying and disable the client for good.
            if status in (401, 403):
                self._log("auth rejected (status=%s), disabling" % status)
                self.disable()
                return _REJECTED

            # Too large. Resending the same bytes cannot succeed, so halve the
            # envelope and hand the remainder back for a later, smaller flush.
            if status == 413:
                if len(items) > 1:
                    half = max(1, len(items) // 2)
                    self._log(
                        "envelope too large (413); retrying with %d of %d item(s)"
                        % (half, len(items))
                    )
                    first = self._send(items[:half])
                    if first != _DELIVERED:
                        return first
                    return self._send(items[half:])
                self._log("single item rejected as too large (413); dropping it")
                return _REJECTED

            # Non-retryable (400/404 and other client errors): drop.
            if not _is_retryable(status):
                self._log(
                    "dropping %d item(s) on non-retryable status %s"
                    % (len(items), status)
                )
                return _REJECTED

            # Transient: retry with backoff until we run out of retries.
            if attempt >= self._max_retries:
                self._log(
                    "giving up on %d item(s) after %d attempts (last status=%s)"
                    % (len(items), self._max_retries + 1, status)
                )
                return _TRANSIENT

            self._sleep(self._backoff_delay(attempt, status, resp_headers))
            attempt += 1

    def _backoff_delay(
        self,
        attempt: int,
        status: Optional[int],
        resp_headers: Mapping[str, str],
    ) -> float:
        """Seconds to wait before the next retry (capped at 30s).

        A 429 with a ``Retry-After`` header is honored verbatim (capped);
        otherwise use exponential backoff with full jitter over ``retry_base``.
        """
        if status == 429:
            retry_after = _parse_retry_after(_header(resp_headers, "Retry-After"))
            if retry_after is not None:
                return min(retry_after, MAX_BACKOFF_SECONDS)
        base = self._retry_base * (2 ** attempt)
        jittered = self._rand() * base if base > 0 else 0.0
        return min(jittered, MAX_BACKOFF_SECONDS)
