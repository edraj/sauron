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

from typing import Any, Mapping, Optional

from ._client import SDK_NAME, SDK_VERSION, Client
from ._dsn import Dsn, DsnError, parse_dsn

__all__ = [
    "init",
    "track",
    "capture_exception",
    "capture_message",
    "identify",
    "flush",
    "close",
    "get_client",
    "Client",
    "Dsn",
    "DsnError",
    "parse_dsn",
    "SDK_NAME",
    "SDK_VERSION",
]

# The process-wide active client. ``None`` until ``init`` succeeds.
_client: Optional[Client] = None


def init(
    dsn: Optional[str] = None,
    *,
    environment: str = "production",
    release: Optional[str] = None,
    sample_rate: float = 1.0,
    flush_interval: float = 5.0,
    max_batch: int = 30,
    debug: bool = False,
    sender: Optional[Any] = None,
) -> Optional[Client]:
    """Initialize the global Sauron client.

    A missing/empty ``dsn`` puts the SDK into a disabled no-op mode (logs, does
    not raise) so production code can ship without a DSN configured. A non-empty
    but malformed DSN raises :class:`DsnError`.

    Args:
        sender: optional HTTP sender ``(url, headers, body) -> status`` used in
            place of the built-in ``urllib`` transport (mainly for tests).

    Returns:
        The created :class:`Client`, or ``None`` when disabled.
    """
    global _client

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
        debug=debug,
        sender=sender,
    )
    return _client


def get_client() -> Optional[Client]:
    """Return the active global client, or ``None`` when disabled."""
    return _client


def track(
    event: str,
    distinct_id: str,
    properties: Optional[Mapping[str, Any]] = None,
) -> None:
    if _client is not None:
        _client.track(event, distinct_id, properties)


def capture_exception(
    error: Optional[BaseException] = None,
    *,
    user: Optional[Mapping[str, Any]] = None,
    level: str = "error",
    tags: Optional[Mapping[str, Any]] = None,
) -> Optional[str]:
    if _client is not None:
        return _client.capture_exception(
            error, user=user, level=level, tags=tags
        )
    return None


def capture_message(message: str, level: str = "info") -> Optional[str]:
    if _client is not None:
        return _client.capture_message(message, level)
    return None


def identify(
    distinct_id: str,
    traits: Optional[Mapping[str, Any]] = None,
) -> None:
    if _client is not None:
        _client.identify(distinct_id, traits)


def flush(timeout: Optional[float] = None) -> bool:
    if _client is not None:
        return _client.flush(timeout)
    return True


def close(timeout: Optional[float] = None) -> None:
    global _client
    if _client is not None:
        _client.close(timeout)
        _client = None
