"""Scope: global process-wide defaults plus per-request/per-task isolation.

A naive global mutable scope would leak one request's user/tags/breadcrumbs into
a concurrent request. The active scope is stored in a :class:`contextvars.ContextVar`
so each ``asyncio`` task / thread / copied context sees its own layer over the
global scope. ``push_scope``/``pop_scope`` (and the ``scope()`` context manager)
clone the current scope so mutations never touch the parent.

Reads merge child-over-parent because each child is a *clone* of its parent at
push time; :meth:`Scope.apply_to_error` then stamps the scope state onto an
outgoing error item (tags, user, breadcrumbs, and — wire-tolerantly — contexts
and extra when present).
"""

from __future__ import annotations

from contextlib import contextmanager
from contextvars import ContextVar
from datetime import datetime, timezone
from typing import Any, Callable, Dict, Iterator, List, Mapping, Optional

# The default breadcrumb ring size (aligned with the Flutter SDK).
DEFAULT_MAX_BREADCRUMBS = 100


def _now_iso() -> str:
    """Current UTC time as an RFC3339 / ISO-8601 string with a ``Z`` suffix."""
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def build_breadcrumb(
    *,
    type: Optional[str] = None,
    category: Optional[str] = None,
    message: Optional[str] = None,
    level: Optional[str] = None,
    data: Optional[Mapping[str, Any]] = None,
) -> Dict[str, Any]:
    """Construct a wire-shaped breadcrumb dict, stamping an ISO ``timestamp``.

    Matches ``envelope.rs::Breadcrumb``: ``{type, category, message, level,
    timestamp, data}``.
    """
    return {
        "type": type or "default",
        "category": category,
        "message": message,
        "level": level,
        "timestamp": _now_iso(),
        "data": dict(data) if data else {},
    }


class Scope:
    """A single layer of ambient signal context.

    Holds a fallback ``user``, ``tags``, free-form ``contexts`` and ``extra``
    blocks, and a bounded breadcrumb ring. Cloned on push so nested scopes never
    mutate their parent.
    """

    __slots__ = (
        "user",
        "tags",
        "contexts",
        "extra",
        "breadcrumbs",
        "max_breadcrumbs",
        "_parent",
    )

    def __init__(self, max_breadcrumbs: int = DEFAULT_MAX_BREADCRUMBS) -> None:
        self.user: Optional[Dict[str, Any]] = None
        self.tags: Dict[str, Any] = {}
        self.contexts: Dict[str, Any] = {}
        self.extra: Dict[str, Any] = {}
        self.breadcrumbs: List[Dict[str, Any]] = []
        self.max_breadcrumbs = max_breadcrumbs
        # Set on push so pop can restore it; ``None`` at the global scope.
        self._parent: Optional["Scope"] = None

    # -- mutators ----------------------------------------------------------

    def set_user(self, user: Optional[Mapping[str, Any]]) -> "Scope":
        """Set (or clear, with ``None``) the fallback user for this scope."""
        self.user = dict(user) if user else None
        return self

    def set_tag(self, key: str, value: Any) -> "Scope":
        self.tags[key] = value
        return self

    def set_tags(self, tags: Mapping[str, Any]) -> "Scope":
        self.tags.update(tags)
        return self

    def set_context(self, key: str, value: Any) -> "Scope":
        self.contexts[key] = value
        return self

    def set_extra(self, key: str, value: Any) -> "Scope":
        self.extra[key] = value
        return self

    def add_breadcrumb(self, crumb: Mapping[str, Any]) -> "Scope":
        """Append a (pre-built) breadcrumb, dropping the oldest past the cap."""
        self.breadcrumbs.append(dict(crumb))
        # Ring buffer: push then trim the front so the newest N survive.
        overflow = len(self.breadcrumbs) - self.max_breadcrumbs
        if overflow > 0:
            del self.breadcrumbs[:overflow]
        return self

    def clear(self) -> None:
        self.user = None
        self.tags = {}
        self.contexts = {}
        self.extra = {}
        self.breadcrumbs = []

    # -- read --------------------------------------------------------------

    def clone(self) -> "Scope":
        """A deep-enough copy: mutating the clone never touches the original."""
        c = Scope(self.max_breadcrumbs)
        c.user = dict(self.user) if self.user else None
        c.tags = dict(self.tags)
        c.contexts = dict(self.contexts)
        c.extra = dict(self.extra)
        c.breadcrumbs = [dict(b) for b in self.breadcrumbs]
        return c

    def apply_to_error(self, item: Dict[str, Any]) -> None:
        """Stamp this scope's state onto an outgoing error item in place.

        Per-call values already present on ``item`` win over scope values.
        """
        # Tags: scope first, per-call item tags override.
        merged_tags: Dict[str, Any] = dict(self.tags)
        merged_tags.update(item.get("tags") or {})
        item["tags"] = merged_tags

        # User: a per-call user (already set) wins; otherwise the scope user.
        if not item.get("user") and self.user:
            item["user"] = dict(self.user)

        # Breadcrumbs: the scope trail (capped), unless already populated.
        if not item.get("breadcrumbs"):
            trail = self.breadcrumbs[-self.max_breadcrumbs :]
            item["breadcrumbs"] = [dict(b) for b in trail]

        # Contexts / extra: free-form blocks; the ingest tolerantly ignores
        # unknown keys, so only attach them when non-empty.
        if self.contexts:
            merged_ctx = dict(self.contexts)
            merged_ctx.update(item.get("contexts") or {})
            item["contexts"] = merged_ctx
        if self.extra:
            merged_extra = dict(self.extra)
            merged_extra.update(item.get("extra") or {})
            item["extra"] = merged_extra

    def apply_to_event(self, item: Dict[str, Any]) -> None:
        """Stamp scope tags/contexts/extra onto an analytics event item in place.

        Mirrors the tags/contexts/extra half of :meth:`apply_to_error` (no user,
        breadcrumbs, or fingerprint): scope values first, per-call values already
        on ``item`` override (tags/extra by key, contexts by block name). Empty
        results are omitted rather than emitted as ``{}``.
        """
        merged_tags: Dict[str, Any] = dict(self.tags)
        merged_tags.update(item.get("tags") or {})
        if merged_tags:
            item["tags"] = merged_tags
        else:
            item.pop("tags", None)

        merged_ctx: Dict[str, Any] = dict(self.contexts)
        merged_ctx.update(item.get("contexts") or {})
        if merged_ctx:
            item["contexts"] = merged_ctx
        else:
            item.pop("contexts", None)

        merged_extra: Dict[str, Any] = dict(self.extra)
        merged_extra.update(item.get("extra") or {})
        if merged_extra:
            item["extra"] = merged_extra
        else:
            item.pop("extra", None)


# -- module-level scope hub ------------------------------------------------

_global = Scope()
_current: ContextVar[Optional[Scope]] = ContextVar(
    "sauron_scope", default=None
)


def get_global_scope() -> Scope:
    """The process-wide scope holding defaults (release/env/global tags/user)."""
    return _global


def get_current_scope() -> Scope:
    """The active scope: the pushed context scope, or the global one."""
    return _current.get() or _global


def push_scope() -> Scope:
    """Clone the current scope and make the clone active; returns the clone."""
    parent = _current.get()
    child = get_current_scope().clone()
    child._parent = parent
    _current.set(child)
    return child


def pop_scope() -> None:
    """Restore the parent of the active scope (no-op at the global scope)."""
    current = _current.get()
    if current is not None:
        _current.set(current._parent)


@contextmanager
def scope() -> Iterator[Scope]:
    """Run a block under a fresh child scope, auto-popped on exit."""
    child = push_scope()
    try:
        yield child
    finally:
        pop_scope()


def configure_scope(callback: Callable[[Scope], Any]) -> None:
    """Mutate the active scope via a callback (e.g. set global defaults)."""
    callback(get_current_scope())


def set_max_breadcrumbs(max_breadcrumbs: int) -> None:
    """Adjust the global scope's breadcrumb ring size (clones inherit it)."""
    _global.max_breadcrumbs = max_breadcrumbs


def reset_scopes() -> None:
    """Test aid: clear the global scope and reset the active-scope var."""
    _global.clear()
    _global.max_breadcrumbs = DEFAULT_MAX_BREADCRUMBS
    _current.set(None)
