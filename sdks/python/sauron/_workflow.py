"""Workflow types and pure normalization helpers.

Holds **no module-level state** — unlike the browser SDK's `workflow.ts`
(a single module-level `let current`, correct only for a one-user, one-tab
browser), the active workflow here lives on the per-request
:class:`sauron._scope.Scope` (see ``_scope.py``: the ``workflow`` field,
cloned on push) which is itself isolated per ``asyncio`` task / thread /
copied context via a ``contextvars.ContextVar``. A module-level workflow
global would leak one concurrent request's workflow into another's errors.

See ``Client.start_workflow`` / ``end_workflow`` / ``cancel_workflow`` in
``_client.py`` for the mutators, and ``sauron.get_workflow`` for the getter.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from enum import Enum
from typing import Optional

# Cap on a workflow name, after trimming.
WORKFLOW_NAME_MAX = 120
# Cap on a cancel reason, after trimming.
WORKFLOW_REASON_MAX = 120


class WorkflowStatus(str, Enum):
    """The exact six wire-contract status strings returned by the three
    workflow mutators — never a seventh. Subclasses ``str`` so callers can
    compare against the literal wire strings (``result.status == "ok"``).
    """

    OK = "ok"
    ALREADY_ACTIVE = "already_active"
    NOT_ACTIVE = "not_active"
    NAME_MISMATCH = "name_mismatch"
    INVALID_NAME = "invalid_name"
    DISABLED = "disabled"


@dataclass(frozen=True)
class WorkflowResult:
    """The return value of every ``start_workflow``/``end_workflow``/
    ``cancel_workflow`` call. ``workflow_id`` is set only for ``ok``.
    """

    status: WorkflowStatus
    workflow_id: Optional[str] = None


@dataclass
class ActiveWorkflow:
    """A currently-active workflow, held as a single unit on
    :attr:`sauron._scope.Scope.workflow` — ``workflow_id`` and ``name`` are
    always read/written together (never as two independent nullable fields),
    so a stamped item can never carry a name with no id or vice versa.
    """

    workflow_id: str
    name: str
    started_at: datetime


def normalize_workflow_name(name: object) -> Optional[str]:
    """Return the trimmed name, or ``None`` when invalid.

    Order matters: trim, then reject if empty, then reject if over the cap —
    all checks run on the *trimmed* value. Never truncates.
    """
    if not isinstance(name, str):
        return None
    trimmed = name.strip()
    if not trimmed or len(trimmed) > WORKFLOW_NAME_MAX:
        return None
    return trimmed


def normalize_reason(reason: object) -> str:
    """Normalize a cancel reason: default ``"user"``, else trim and cap."""
    if not isinstance(reason, str) or not reason.strip():
        return "user"
    return reason.strip()[:WORKFLOW_REASON_MAX]
