"""Shared test helper: a fake HTTP sender that records POSTs instead of
touching the network."""

from __future__ import annotations

import json
import threading
from typing import Any, Dict, List


class FakeSender:
    """Drop-in replacement for the transport's HTTP sender.

    Records every ``(url, headers, body)`` POST and returns a configurable
    status code. Never performs any network I/O.
    """

    def __init__(self, status: int = 200) -> None:
        self.status = status
        self.calls: List[Dict[str, Any]] = []
        self._lock = threading.Lock()

    def __call__(self, url: str, headers: Dict[str, str], body: bytes) -> int:
        with self._lock:
            self.calls.append(
                {
                    "url": url,
                    "headers": headers,
                    "body": body,
                    "json": json.loads(body.decode("utf-8")),
                }
            )
        return self.status

    # -- convenience accessors --------------------------------------------

    @property
    def envelopes(self) -> List[Dict[str, Any]]:
        return [c["json"] for c in self.calls]

    @property
    def items(self) -> List[Dict[str, Any]]:
        out: List[Dict[str, Any]] = []
        for env in self.envelopes:
            out.extend(env.get("items", []))
        return out
