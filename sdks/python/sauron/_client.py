"""The Sauron client: owns config, context, and the transport, and turns the
public API calls into wire-contract envelope items."""

from __future__ import annotations

import platform
import random
import sys
import uuid
from datetime import datetime, timezone
from typing import Any, Dict, List, Mapping, Optional

from ._dsn import Dsn, parse_dsn
from ._stacktrace import exception_type_name, extract_stacktrace
from ._transport import Sender, Transport

SDK_NAME = "sauron-python"
SDK_VERSION = "0.1.0"

_VALID_LEVELS = frozenset({"debug", "info", "warning", "error", "fatal"})


def _now_iso() -> str:
    """Current UTC time as an RFC3339 / ISO-8601 string with a ``Z`` suffix."""
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _make_logger(debug: bool):
    def log(*args: Any, **kwargs: Any) -> None:
        if debug:
            print("[sauron]", *args, file=sys.stderr)

    return log


class Client:
    """A configured Sauron client. Construct via :func:`sauron.init`."""

    def __init__(
        self,
        dsn: str,
        *,
        environment: str = "production",
        release: Optional[str] = None,
        sample_rate: float = 1.0,
        flush_interval: float = 5.0,
        max_batch: int = 30,
        debug: bool = False,
        sender: Optional[Sender] = None,
    ) -> None:
        self._log = _make_logger(debug)
        self.dsn: Dsn = parse_dsn(dsn)
        self.environment = environment
        self.release = release
        self.sample_rate = sample_rate
        self.enabled = True

        # A stable per-process id for this server instance.
        self._device_id = str(uuid.uuid4())

        self._transport = Transport(
            dsn=self.dsn,
            make_envelope=self._make_envelope,
            sender=sender,
            flush_interval=flush_interval,
            max_batch=max_batch,
            logger=self._log,
            on_disable=self._on_disable,
        )
        self._transport.start()
        self._log(
            "initialized", self.dsn.host, "project", self.dsn.project_id
        )

    # -- context / envelope ------------------------------------------------

    def _make_envelope(self, items: List[Dict[str, Any]]) -> Dict[str, Any]:
        header = {
            "dsn": self.dsn.raw,
            "sdk": {"name": SDK_NAME, "version": SDK_VERSION},
            "sent_at": _now_iso(),
            "environment": self.environment,
            "release": self.release,
        }
        context = {
            "device": {"device_id": self._device_id},
            "os": {"name": platform.system() or None, "version": None},
            "app": {},
            "runtime": {"name": "python", "version": platform.python_version()},
            "user": None,
        }
        return {"header": header, "context": context, "items": items}

    def _on_disable(self) -> None:
        self.enabled = False
        self._log("client disabled")

    # -- public API --------------------------------------------------------

    def track(
        self,
        event: str,
        distinct_id: str,
        properties: Optional[Mapping[str, Any]] = None,
    ) -> None:
        if not self.enabled:
            return
        if not distinct_id:
            self._log("track() requires a distinct_id; dropping event", event)
            return
        item = {
            "type": "event",
            "name": event,
            "distinct_id": distinct_id,
            "properties": dict(properties) if properties else {},
            "timestamp": _now_iso(),
            "session_id": None,
            "screen": None,
        }
        self._transport.capture(item)

    def capture_exception(
        self,
        error: Optional[BaseException] = None,
        *,
        user: Optional[Mapping[str, Any]] = None,
        level: str = "error",
        tags: Optional[Mapping[str, Any]] = None,
    ) -> Optional[str]:
        if not self.enabled:
            return None

        if error is None:
            error = sys.exc_info()[1]
        if error is None:
            self._log("capture_exception() called with no active exception")
            return None

        # Sampling applies to errors only.
        if random.random() >= self.sample_rate:
            self._log("dropped error by sample_rate")
            return None

        level = level if level in _VALID_LEVELS else "error"
        event_id = uuid.uuid4().hex
        item = {
            "type": "error",
            "event_id": event_id,
            "level": level,
            "timestamp": _now_iso(),
            "exception": {
                "type": exception_type_name(error),
                "value": str(error) if str(error) else None,
                "mechanism": {"type": "generic", "handled": True},
                "stacktrace": extract_stacktrace(error),
            },
            "message": None,
            "breadcrumbs": [],
            "tags": dict(tags) if tags else {},
            "fingerprint": None,
            "user": self._normalize_user(user),
            "session_id": None,
            "screen": None,
        }
        self._transport.capture(item)
        return event_id

    def capture_message(
        self, message: str, level: str = "info"
    ) -> Optional[str]:
        if not self.enabled:
            return None
        level = level if level in _VALID_LEVELS else "info"
        event_id = uuid.uuid4().hex
        item = {
            "type": "error",
            "event_id": event_id,
            "level": level,
            "timestamp": _now_iso(),
            "exception": None,
            "message": message,
            "breadcrumbs": [],
            "tags": {},
            "fingerprint": None,
            "user": None,
            "session_id": None,
            "screen": None,
        }
        self._transport.capture(item)
        return event_id

    def identify(
        self,
        distinct_id: str,
        traits: Optional[Mapping[str, Any]] = None,
    ) -> None:
        if not self.enabled:
            return
        if not distinct_id:
            self._log("identify() requires a distinct_id; dropping")
            return
        item = {
            "type": "identify",
            "distinct_id": distinct_id,
            "anonymous_id": None,
            "traits": dict(traits) if traits else {},
            "timestamp": _now_iso(),
        }
        self._transport.capture(item)

    def flush(self, timeout: Optional[float] = None) -> bool:
        return self._transport.flush(timeout)

    def close(self, timeout: Optional[float] = None) -> None:
        self._transport.close(timeout)
        self.enabled = False

    # -- helpers -----------------------------------------------------------

    @staticmethod
    def _normalize_user(
        user: Optional[Mapping[str, Any]]
    ) -> Optional[Dict[str, Any]]:
        if not user:
            return None
        return {
            "id": user.get("id"),
            "email": user.get("email"),
            "username": user.get("username"),
        }
