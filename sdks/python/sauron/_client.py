"""The Sauron client: owns config, context, and the transport, and turns the
public API calls into wire-contract envelope items."""

from __future__ import annotations

import platform
import random
import sys
import uuid
from datetime import datetime, timezone
from typing import Any, Callable, Dict, List, Mapping, Optional, Sequence

from ._autocapture import install_excepthook
from ._dsn import Dsn, parse_dsn
from ._scope import (
    build_breadcrumb,
    get_current_scope,
    get_global_scope,
    set_max_breadcrumbs,
)
from ._stacktrace import exception_type_name, extract_stacktrace
from ._transport import Sender, Transport

SDK_NAME = "sauron-python"
SDK_VERSION = "0.3.0"

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
        max_breadcrumbs: int = 100,
        tags: Optional[Mapping[str, Any]] = None,
        contexts: Optional[Mapping[str, Any]] = None,
        extra: Optional[Mapping[str, Any]] = None,
        gzip_threshold_bytes: int = 1024,
        max_queue_bytes: int = 1_048_576,
        offline_path: Optional[str] = None,
        before_send: Optional[Callable[..., Optional[Dict[str, Any]]]] = None,
        before_breadcrumb: Optional[
            Callable[[Dict[str, Any]], Optional[Dict[str, Any]]]
        ] = None,
        auto_capture_unhandled: bool = False,
        debug: bool = False,
        sender: Optional[Sender] = None,
    ) -> None:
        self._log = _make_logger(debug)
        self.dsn: Dsn = parse_dsn(dsn)
        self.environment = environment
        self.release = release
        self.sample_rate = sample_rate
        self._before_send = before_send
        self._before_breadcrumb = before_breadcrumb
        self.enabled = True
        # Uninstaller for the opt-in uncaught-exception hooks (``None`` = off).
        self._uninstall_autocapture: Optional[Callable[[], None]] = None

        # The active breadcrumb ring size lives on the scope; clones inherit it.
        set_max_breadcrumbs(max_breadcrumbs)

        # Seed the process-wide scope with init-time metadata defaults so every
        # error/message/track picks them up (runtime setters last-write-wins).
        gscope = get_global_scope()
        if tags:
            gscope.set_tags(tags)
        if contexts:
            for _name, _block in contexts.items():
                gscope.set_context(_name, _block)
        if extra:
            for _key, _value in extra.items():
                gscope.set_extra(_key, _value)

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
            gzip_threshold_bytes=gzip_threshold_bytes,
            max_queue_bytes=max_queue_bytes,
            offline_path=offline_path,
        )
        self._transport.start()

        # Opt-in: capture uncaught exceptions with ``mechanism.handled=False``,
        # chaining (never replacing) the prior hooks so default crash/exit
        # behavior is preserved.
        if auto_capture_unhandled:
            self._uninstall_autocapture = install_excepthook(self)

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

    def _dispatch(
        self, item: Dict[str, Any], hint: Optional[Any] = None
    ) -> None:
        """The single outbound chokepoint: run ``before_send`` then enqueue.

        Applies to every item type (error/event/identify/transaction). A hook
        returning ``None`` drops the item; a returned object replaces it. A
        hook that raises drops the item rather than crashing the caller.
        """
        if self._before_send is not None:
            try:
                item = self._before_send(item, hint)
            except Exception as exc:
                self._log("before_send raised, dropping item", exc)
                return
            if item is None:
                return
        self._transport.capture(item)

    # -- scope / breadcrumbs ----------------------------------------------

    def add_breadcrumb(
        self,
        *,
        type: Optional[str] = None,
        category: Optional[str] = None,
        message: Optional[str] = None,
        level: Optional[str] = None,
        data: Optional[Mapping[str, Any]] = None,
    ) -> None:
        """Record a breadcrumb on the active scope (bounded ring).

        Runs the ``before_breadcrumb`` hook first (if configured); a ``None``
        return drops the crumb.
        """
        if not self.enabled:
            return
        crumb = build_breadcrumb(
            type=type,
            category=category,
            message=message,
            level=level,
            data=data,
        )
        if self._before_breadcrumb is not None:
            try:
                result = self._before_breadcrumb(crumb)
            except Exception as exc:  # a hook must never crash the caller
                self._log("before_breadcrumb raised, dropping crumb", exc)
                return
            if result is None:
                return
            crumb = result
        get_current_scope().add_breadcrumb(crumb)

    # -- public API --------------------------------------------------------

    def track(
        self,
        event: str,
        distinct_id: str,
        properties: Optional[Mapping[str, Any]] = None,
        *,
        tags: Optional[Mapping[str, Any]] = None,
        contexts: Optional[Mapping[str, Any]] = None,
        extra: Optional[Mapping[str, Any]] = None,
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
        # Per-call metadata attached only when non-empty; the scope merge then
        # folds in defaults and omits empty blocks (never emit {}).
        if tags:
            item["tags"] = dict(tags)
        if contexts:
            item["contexts"] = dict(contexts)
        if extra:
            item["extra"] = dict(extra)
        get_current_scope().apply_to_event(item)
        self._dispatch(item)

    def capture_exception(
        self,
        error: Optional[BaseException] = None,
        *,
        user: Optional[Mapping[str, Any]] = None,
        level: str = "error",
        tags: Optional[Mapping[str, Any]] = None,
        contexts: Optional[Mapping[str, Any]] = None,
        extra: Optional[Mapping[str, Any]] = None,
        fingerprint: Optional[Sequence[str]] = None,
        mechanism: Optional[Mapping[str, Any]] = None,
    ) -> Optional[str]:
        """Capture an exception as a wire-contract error item.

        ``fingerprint`` is an optional client-supplied grouping override (a list
        of strings, honored verbatim by the backend). ``mechanism`` overrides the
        default ``{"type": "generic", "handled": True}`` — the auto-capture hooks
        pass ``handled=False`` for uncaught crashes.
        """
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
                "mechanism": dict(mechanism)
                if mechanism
                else {"type": "generic", "handled": True},
                "stacktrace": extract_stacktrace(error),
            },
            "message": None,
            "breadcrumbs": [],
            "tags": dict(tags) if tags else {},
            "fingerprint": list(fingerprint) if fingerprint else None,
            "user": self._normalize_user(user),
            "session_id": None,
            "screen": None,
        }
        # Per-call metadata: attach only when non-empty so the scope merge in
        # apply_to_error can omit empty blocks (never emit {}).
        if contexts:
            item["contexts"] = dict(contexts)
        if extra:
            item["extra"] = dict(extra)
        # Merge the active scope (breadcrumbs/tags/user/contexts/extra); per-call
        # user/tags/contexts/extra already on the item take precedence.
        get_current_scope().apply_to_error(item)
        self._dispatch(item)
        return event_id

    def capture_message(
        self,
        message: str,
        level: str = "info",
        *,
        tags: Optional[Mapping[str, Any]] = None,
        contexts: Optional[Mapping[str, Any]] = None,
        extra: Optional[Mapping[str, Any]] = None,
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
            "tags": dict(tags) if tags else {},
            "fingerprint": None,
            "user": None,
            "session_id": None,
            "screen": None,
        }
        if contexts:
            item["contexts"] = dict(contexts)
        if extra:
            item["extra"] = dict(extra)
        get_current_scope().apply_to_error(item)
        self._dispatch(item)
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
        self._dispatch(item)

    def track_transaction(
        self,
        name: str,
        *,
        op: str = "custom",
        duration_ms: float,
        status: Optional[str] = None,
        http_method: Optional[str] = None,
        http_status: Optional[int] = None,
        url: Optional[str] = None,
        distinct_id: Optional[str] = None,
    ) -> None:
        """Emit a performance transaction (one timed operation).

        ``op`` defaults to ``"custom"``. ``distinct_id`` falls back to the
        active scope's user id when omitted.
        """
        if not self.enabled:
            return
        if distinct_id is None:
            user = get_current_scope().user
            if user:
                distinct_id = user.get("id")
        item = {
            "type": "transaction",
            "name": name,
            "op": op or "custom",
            "duration_ms": float(duration_ms),
            "status": status,
            "http_method": http_method,
            "http_status": http_status,
            "url": url,
            "distinct_id": distinct_id,
            "session_id": None,
            "timestamp": _now_iso(),
        }
        self._dispatch(item)

    def flush(self, timeout: Optional[float] = None) -> bool:
        return self._transport.flush(timeout)

    def close(self, timeout: Optional[float] = None) -> None:
        if self._uninstall_autocapture is not None:
            self._uninstall_autocapture()
            self._uninstall_autocapture = None
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
