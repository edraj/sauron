"""Sauron server-side Python SDK.

Public API::

    import sauron
    sauron.init(dsn="https://pk@host/1")
    sauron.track("event", distinct_id="u_1", properties={...})
    sauron.identify("u_1", traits={...})
    sauron.capture_exception()          # reads the active exception
    sauron.capture_message("hello")
    sauron.flush()
    sauron.close()
"""

from __future__ import annotations

import atexit
from typing import Any, Callable, Dict, Mapping, Optional, Sequence

from ._client import SDK_NAME, SDK_VERSION, Client
from ._dsn import Dsn, DsnError, parse_dsn
from ._scope import (
    Scope,
    build_breadcrumb,
    configure_scope,
    get_current_scope,
    get_global_scope,
    pop_scope,
    push_scope,
    scope,
)

__all__ = [
    "init",
    "track",
    "track_transaction",
    "capture_exception",
    "capture_message",
    "identify",
    "add_breadcrumb",
    "set_user",
    "set_tag",
    "set_tags",
    "set_context",
    "set_extra",
    "configure_scope",
    "scope",
    "push_scope",
    "pop_scope",
    "get_current_scope",
    "get_global_scope",
    "flush",
    "close",
    "get_client",
    "Client",
    "Scope",
    "Dsn",
    "DsnError",
    "parse_dsn",
    "SDK_NAME",
    "SDK_VERSION",
]

# The process-wide active client. ``None`` until ``init`` succeeds.
_client: Optional[Client] = None

# Register the atexit flush exactly once per process (idempotent across inits).
_atexit_registered = False


def _atexit_flush() -> None:
    """Flush + close the active client at interpreter shutdown (best-effort)."""
    if _client is not None:
        try:
            _client.close()
        except Exception:
            pass


def init(
    dsn: Optional[str] = None,
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
    sender: Optional[Any] = None,
) -> Optional[Client]:
    """Initialize the global Sauron client.

    A missing/empty ``dsn`` puts the SDK into a disabled no-op mode (logs, does
    not raise) so production code can ship without a DSN configured. A non-empty
    but malformed DSN raises :class:`DsnError`.

    Args:
        gzip_threshold_bytes: compress the request body with gzip (and set
            ``Content-Encoding: gzip``) once it exceeds this size. Default 1024.
        max_queue_bytes: byte budget for the in-memory pending queue; once
            exceeded the oldest items are dropped. Default 1 MiB.
        offline_path: opt-in directory for disk-persisting pending items FIFO
            (reloaded on init, deleted on delivery). Default ``None`` (off).
        auto_capture_unhandled: opt-in (default ``False``) — install
            ``sys.excepthook``/``threading.excepthook`` hooks that capture
            uncaught exceptions with ``mechanism.handled=False`` and then
            delegate to the previous hook (default crash/exit behavior preserved).
        sender: optional HTTP sender ``(url, headers, body) -> status`` used in
            place of the built-in ``urllib`` transport (mainly for tests).

    Returns:
        The created :class:`Client`, or ``None`` when disabled.
    """
    global _client, _atexit_registered

    if not dsn:
        if debug:
            import sys

            print("[sauron] no DSN configured; SDK disabled", file=sys.stderr)
        _client = None
        return None

    _client = Client(
        dsn,
        environment=environment,
        release=release,
        sample_rate=sample_rate,
        flush_interval=flush_interval,
        max_batch=max_batch,
        max_breadcrumbs=max_breadcrumbs,
        tags=tags,
        contexts=contexts,
        extra=extra,
        gzip_threshold_bytes=gzip_threshold_bytes,
        max_queue_bytes=max_queue_bytes,
        offline_path=offline_path,
        before_send=before_send,
        before_breadcrumb=before_breadcrumb,
        auto_capture_unhandled=auto_capture_unhandled,
        debug=debug,
        sender=sender,
    )
    # Graceful shutdown: flush the buffer at interpreter exit. Registered once.
    if not _atexit_registered:
        atexit.register(_atexit_flush)
        _atexit_registered = True
    return _client


def get_client() -> Optional[Client]:
    """Return the active global client, or ``None`` when disabled."""
    return _client


def track(
    event: str,
    distinct_id: str,
    properties: Optional[Mapping[str, Any]] = None,
    *,
    tags: Optional[Mapping[str, Any]] = None,
    contexts: Optional[Mapping[str, Any]] = None,
    extra: Optional[Mapping[str, Any]] = None,
) -> None:
    if _client is not None:
        _client.track(
            event,
            distinct_id,
            properties,
            tags=tags,
            contexts=contexts,
            extra=extra,
        )


def capture_exception(
    error: Optional[BaseException] = None,
    *,
    user: Optional[Mapping[str, Any]] = None,
    level: str = "error",
    tags: Optional[Mapping[str, Any]] = None,
    contexts: Optional[Mapping[str, Any]] = None,
    extra: Optional[Mapping[str, Any]] = None,
    fingerprint: Optional[Sequence[str]] = None,
) -> Optional[str]:
    if _client is not None:
        return _client.capture_exception(
            error,
            user=user,
            level=level,
            tags=tags,
            contexts=contexts,
            extra=extra,
            fingerprint=fingerprint,
        )
    return None


def capture_message(
    message: str,
    level: str = "info",
    *,
    tags: Optional[Mapping[str, Any]] = None,
    contexts: Optional[Mapping[str, Any]] = None,
    extra: Optional[Mapping[str, Any]] = None,
) -> Optional[str]:
    if _client is not None:
        return _client.capture_message(
            message, level, tags=tags, contexts=contexts, extra=extra
        )
    return None


def identify(
    distinct_id: str,
    traits: Optional[Mapping[str, Any]] = None,
) -> None:
    if _client is not None:
        _client.identify(distinct_id, traits)


def track_transaction(
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
    """Emit a performance transaction. No-op before ``init`` / when disabled."""
    if _client is not None:
        _client.track_transaction(
            name,
            op=op,
            duration_ms=duration_ms,
            status=status,
            http_method=http_method,
            http_status=http_status,
            url=url,
            distinct_id=distinct_id,
        )


# -- scope + breadcrumbs (operate on the active scope) ---------------------


def add_breadcrumb(
    *,
    type: Optional[str] = None,
    category: Optional[str] = None,
    message: Optional[str] = None,
    level: Optional[str] = None,
    data: Optional[Mapping[str, Any]] = None,
) -> None:
    """Record a breadcrumb on the active scope.

    When a client is initialized the ``before_breadcrumb`` hook runs first;
    before init this seeds the global scope (no hook, never raises).
    """
    if _client is not None:
        _client.add_breadcrumb(
            type=type,
            category=category,
            message=message,
            level=level,
            data=data,
        )
    else:
        get_current_scope().add_breadcrumb(
            build_breadcrumb(
                type=type,
                category=category,
                message=message,
                level=level,
                data=data,
            )
        )


def set_user(user: Optional[Mapping[str, Any]]) -> None:
    """Set (or clear) the active scope's fallback user."""
    get_current_scope().set_user(user)


def set_tag(key: str, value: Any) -> None:
    get_current_scope().set_tag(key, value)


def set_tags(tags: Mapping[str, Any]) -> None:
    get_current_scope().set_tags(tags)


def set_context(key: str, value: Any) -> None:
    get_current_scope().set_context(key, value)


def set_extra(key: str, value: Any) -> None:
    get_current_scope().set_extra(key, value)


def flush(timeout: Optional[float] = None) -> bool:
    if _client is not None:
        return _client.flush(timeout)
    return True


def close(timeout: Optional[float] = None) -> None:
    global _client
    if _client is not None:
        _client.close(timeout)
        _client = None
