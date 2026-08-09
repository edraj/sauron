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
from ._workflow import (
    ActiveWorkflow,
    WorkflowResult,
    WorkflowStatus,
    normalize_reason,
    normalize_workflow_name,
)

SDK_NAME = "sauron-python"
SDK_VERSION = "1.4.0"

# Item types eligible for workflow stamping in ``_dispatch`` — the single
# chokepoint every error/event/identify/transaction item passes through.
# ``identify`` is deliberately excluded: the server has no workflow columns
# for it (contract item 16).
_WORKFLOW_STAMPED_TYPES = frozenset({"event", "error", "transaction"})

# The three reserved workflow lifecycle event names. These are the ONLY
# events exempt from ``track()``'s "drop when ``distinct_id`` is falsy"
# guard — see ``_workflow_distinct_id`` for why an empty ``distinct_id`` is
# the correct wire value for an anonymous workflow run.
_WORKFLOW_LIFECYCLE_EVENTS = frozenset(
    {"$workflow_start", "$workflow_end", "$workflow_cancel"}
)

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
        self.release = release
        self.sample_rate = sample_rate
        self._before_send = before_send
        self._before_breadcrumb = before_breadcrumb
        # Explicit disable/close flag; see the ``enabled`` property below —
        # the public predicate also consults the transport's own auto-disable.
        self._enabled = True
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

    @property
    def enabled(self) -> bool:
        """Whether captures/tracks/workflow calls are currently accepted.

        ``False`` once :meth:`close` has run, **or** once the transport has
        permanently auto-disabled itself after the gateway rejected the
        ingest key with a ``401``/``403`` — whichever happens first. The
        latter can flip this to ``False`` mid-process without the app ever
        calling ``close()``, which is the whole point: a revoked/rotated key
        must stop `start_workflow`/`track`/etc. from believing they succeeded.

        Computed rather than stored so every existing read site (``track``,
        ``capture_exception``, ``capture_message``, ``identify``,
        ``track_transaction``, ``start_workflow``, ``_end_or_cancel_workflow``)
        picks up an auto-disable with no change of its own. The transport
        always exists by the time a ``Client`` is constructed (``__init__``
        builds and starts it unconditionally), so — unlike SDKs that can hold
        a client before its transport exists — there is no "no transport yet"
        case where a missing transport must be treated as enabled rather than
        disabled.
        """
        return self._enabled and not self._transport.disabled

    # -- context / envelope ------------------------------------------------

    def _make_envelope(self, items: List[Dict[str, Any]]) -> Dict[str, Any]:
        header = {
            "dsn": self.dsn.raw,
            "sdk": {"name": SDK_NAME, "version": SDK_VERSION},
            "sent_at": _now_iso(),
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
        self._enabled = False
        self._log("client disabled")

    def _dispatch(
        self, item: Dict[str, Any], hint: Optional[Any] = None
    ) -> None:
        """The single outbound chokepoint: stamp the workflow, run
        ``before_send``, then enqueue.

        Applies to every item type (error/event/identify/transaction) — which
        is exactly why this is where the active workflow (if any) is stamped,
        rather than at each item-construction site: ``track``, ``capture_exception``,
        ``capture_message`` (which builds its item inline) and ``track_transaction``
        all end here, so a future capture path cannot forget the stamp. Never
        stamped onto ``identify`` (no workflow columns for it server-side).

        A hook returning ``None`` drops the item; a returned object replaces
        it. A hook that raises drops the item rather than crashing the caller.
        """
        if item.get("type") in _WORKFLOW_STAMPED_TYPES:
            get_current_scope().apply_workflow(item)
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
        if not distinct_id and event not in _WORKFLOW_LIFECYCLE_EVENTS:
            # Ordinary events still require a distinct_id. The three reserved
            # workflow lifecycle events are exempt: an anonymous workflow run
            # must reach the server with an EMPTY distinct_id rather than be
            # dropped here (see ``_workflow_distinct_id``).
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

    # -- workflows -----------------------------------------------------------
    #
    # Named, explicitly-bounded spans of activity. State lives on the active
    # *scope* (``Scope.workflow``, see ``_scope.py``) rather than a module-level
    # global: this SDK serves concurrent requests, and a module global would
    # let one request's ``start_workflow`` stamp another request's errors, or
    # let one request's ``end_workflow`` be swallowed by another's
    # ``already_active``. ``Scope.workflow`` is isolated the same way
    # tags/user/breadcrumbs already are — via the ``contextvars.ContextVar``
    # scope stack in ``_scope.py`` — so push a scope per request
    # (``with sauron.scope():``) for that isolation to apply; operating
    # straight against the global scope shares one workflow across every
    # caller, exactly like an un-scoped ``set_tag`` would.

    def _workflow_distinct_id(self) -> str:
        """Person id for a workflow lifecycle event: the identified scope
        user, or **the empty string** when nobody is identified.

        The empty string is deliberate and load-bearing — do NOT "fix" this
        to a sentinel like a device id, ``"system"``, or ``anon_<id>``:

        * ``backend/crates/sauron-pipeline/src/process.rs:381`` and ``:440``
          (the ``bump_workflow`` / ``apply_workflow_lifecycle`` call sites)
          both do ``Some(distinct_id.as_str()).filter(|s| !s.is_empty())``,
          so an empty value lands as ``NULL`` on the ``workflows`` row.
        * ``backend/crates/sauron-db/src/repo.rs:3162`` aggregates
          ``COUNT(DISTINCT w.distinct_id) AS unique_users``, which skips
          ``NULL``s.

        So an anonymous workflow run correctly contributes *nothing* to
        ``unique_users``. Any synthetic id instead collapses every anonymous
        run in a process onto one fake user and corrupts that metric. The
        wire field itself is a required ``String``
        (``backend/crates/sauron-core/src/envelope.rs:226``), so it is still
        sent — just empty. ``track()``'s falsy-``distinct_id`` guard exempts
        the three reserved lifecycle event names for exactly this reason.
        """
        user = get_current_scope().user
        if user and user.get("id"):
            return str(user["id"])
        return ""

    def _emit_workflow_close(
        self,
        scope_obj: Any,
        active: ActiveWorkflow,
        event_name: str,
        reason: Optional[str] = None,
    ) -> None:
        """Emit the closing lifecycle event for ``active`` while it is
        STILL ``scope_obj.workflow`` (so ``track()`` — via ``_dispatch`` —
        stamps the item with it), then clear it.

        **This function never propagates an exception.** That is the whole
        contract of item 15(a): the clear runs in ``finally`` *and* the
        failure is swallowed here, so ``end_workflow``/``cancel_workflow``
        still return ``ok`` when the emit throws. Re-raising instead would
        let the caller's outer ``except`` convert a failure that already
        cleared ``scope_obj.workflow`` into ``disabled`` — reporting
        "nothing changed" to a caller whose workflow was in fact closed, and
        breaking the invariant that ``disabled`` from a catch only ever means
        "failed *before* touching state".

        Once a close has been decided, the close is unconditional locally:
        the workflow is over either way, and a lost lifecycle event is
        recoverable server-side (the ``bump_workflow`` upsert) while a
        workflow wedged permanently "active" in local state is not.
        """
        try:
            duration_ms = max(
                0.0,
                (datetime.now(timezone.utc) - active.started_at).total_seconds()
                * 1000.0,
            )
            properties: Dict[str, Any] = {
                "workflow_id": active.workflow_id,
                "workflow_name": active.name,
                "duration_ms": duration_ms,
            }
            if event_name == "$workflow_cancel":
                properties["reason"] = normalize_reason(reason)
            self.track(event_name, self._workflow_distinct_id(), properties)
        except Exception as exc:
            self._log(f"{event_name}: emitting the lifecycle event failed", exc)
        finally:
            scope_obj.workflow = None

    def start_workflow(self, name: str, *, force: bool = False) -> WorkflowResult:
        """Start a named workflow on the active scope.

        ``force=True`` supersedes an already-active workflow — emitting
        ``$workflow_cancel`` with ``reason="superseded"`` for it first — and
        otherwise an active workflow makes this a no-op returning
        ``already_active``. Never raises.
        """
        try:
            if not self.enabled:
                return WorkflowResult(WorkflowStatus.DISABLED)

            normalized = normalize_workflow_name(name)
            if normalized is None:
                self._log("start_workflow: invalid name", name)
                return WorkflowResult(WorkflowStatus.INVALID_NAME)

            scope_obj = get_current_scope()
            active = scope_obj.workflow
            if active is not None and not force:
                self._log(
                    f'start_workflow("{normalized}"): "{active.name}" is '
                    "already active; pass force=True to replace it"
                )
                return WorkflowResult(WorkflowStatus.ALREADY_ACTIVE)

            # Mint the replacement BEFORE any supersede close (contract
            # addendum 23). ``uuid4()``/``now()`` are the last things here
            # that can realistically throw; if they threw *after*
            # ``_emit_workflow_close`` had already put a real
            # ``$workflow_cancel(reason="superseded")`` on the wire and
            # cleared ``scope_obj.workflow``, the outer catch below would
            # return ``disabled`` — telling the caller "nothing changed"
            # when in fact their workflow had just been destroyed. Minting
            # first means a throw here happens with nothing yet mutated, so
            # ``disabled`` is then truthful. (Cost: ``started_at`` predates
            # the supersede emit by microseconds — immaterial, and the
            # contract prescribes this ordering.)
            workflow = ActiveWorkflow(
                workflow_id=str(uuid.uuid4()),
                name=normalized,
                started_at=datetime.now(timezone.utc),
            )

            if active is not None:
                # ``force`` is necessarily True here (the not-force case
                # returned above). Never raises — see ``_emit_workflow_close``.
                self._emit_workflow_close(
                    scope_obj, active, "$workflow_cancel", "superseded"
                )

            # Set state BEFORE emitting so ``$workflow_start`` is itself
            # stamped with the new workflow — backwards, the lifecycle event
            # would carry no workflow fields at all, silently.
            scope_obj.workflow = workflow
            try:
                self.track(
                    "$workflow_start",
                    self._workflow_distinct_id(),
                    {
                        "workflow_id": workflow.workflow_id,
                        "workflow_name": workflow.name,
                    },
                )
            except Exception as exc:
                # A failure here happens AFTER state is set: the workflow IS
                # live locally, and the server materializes the row from the
                # first stamped event anyway (``bump_workflow``'s upsert). A
                # lost ``$workflow_start`` is recoverable; a lost local id is
                # not — so this still returns ``ok``, not ``disabled``.
                self._log(
                    "start_workflow: emitting $workflow_start failed", exc
                )
            return WorkflowResult(WorkflowStatus.OK, workflow.workflow_id)
        except Exception as exc:
            # Only reachable from the pre-mutation prologue above (the
            # enabled check, name normalization, scope lookup, or minting the
            # replacement). Everything past that point either cannot raise or
            # is locally caught, so `disabled` here is always truthful:
            # nothing was mutated and nothing was emitted.
            self._log("start_workflow threw", exc)
            return WorkflowResult(WorkflowStatus.DISABLED)

    def _end_or_cancel_workflow(
        self,
        event_name: str,
        name: Optional[str] = None,
        reason: Optional[str] = None,
    ) -> WorkflowResult:
        try:
            if not self.enabled:
                return WorkflowResult(WorkflowStatus.DISABLED)

            scope_obj = get_current_scope()
            active = scope_obj.workflow
            if active is None:
                return WorkflowResult(WorkflowStatus.NOT_ACTIVE)

            if name is not None and normalize_workflow_name(name) != active.name:
                # A malformed explicit name (blank, > 120) also lands here as
                # a mismatch — ``invalid_name`` is reachable only from
                # ``start_workflow``.
                self._log(
                    f'{event_name}: "{name}" does not match active workflow '
                    f'"{active.name}"'
                )
                return WorkflowResult(WorkflowStatus.NAME_MISMATCH)

            workflow_id = active.workflow_id
            # Never raises, and always clears — so this always reports `ok`
            # once the close has been decided, per contract item 15(a).
            self._emit_workflow_close(scope_obj, active, event_name, reason)
            return WorkflowResult(WorkflowStatus.OK, workflow_id)
        except Exception as exc:
            # Only reachable from the pre-mutation prologue above (the
            # enabled check, scope lookup, or name comparison) — the close
            # itself swallows its own failures, so `disabled` here always
            # means "failed before touching state", never a half-close.
            self._log(f"{event_name} threw", exc)
            return WorkflowResult(WorkflowStatus.DISABLED)

    def end_workflow(self, name: Optional[str] = None) -> WorkflowResult:
        """End the active workflow (or the one named ``name``, if given).

        Emits ``$workflow_end`` with ``duration_ms`` and clears the scope's
        workflow. A no-op returning ``not_active``/``name_mismatch`` when the
        precondition fails. Never raises.
        """
        return self._end_or_cancel_workflow("$workflow_end", name)

    def cancel_workflow(
        self, name: Optional[str] = None, *, reason: Optional[str] = None
    ) -> WorkflowResult:
        """Cancel the active workflow (or the one named ``name``, if given).

        Emits ``$workflow_cancel`` with ``duration_ms`` and ``reason``
        (default ``"user"``, trimmed and capped at 120 chars) and clears the
        scope's workflow. Never raises.
        """
        return self._end_or_cancel_workflow("$workflow_cancel", name, reason)

    def flush(self, timeout: Optional[float] = None) -> bool:
        return self._transport.flush(timeout)

    def close(self, timeout: Optional[float] = None) -> None:
        if self._uninstall_autocapture is not None:
            self._uninstall_autocapture()
            self._uninstall_autocapture = None
        self._transport.close(timeout)
        self._enabled = False

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
