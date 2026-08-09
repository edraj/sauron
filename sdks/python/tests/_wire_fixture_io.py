"""Shared writer for ``sdks/wire-fixtures/<sdk>.json``.

Those envelopes are what the backend's
``cargo test -p sauron-core --test sdk_wire_conformance`` feeds through the REAL
``serde`` deserializer, so nothing here may reshape an item.

What IS pinned:

1. The intrinsically dynamic fields (``timestamp``, ``event_id``, ...).
2. Everything the **toolchain** supplies rather than the SDK — stack-frame
   identity strings (which under pytest are the test module's own names) and the
   host/interpreter values in ``context.os`` / ``.runtime`` / ``.device``.
   Without (2) a fixture was rewritten by a Python upgrade with no wire change at
   all, which makes a CI diff gate noisy and leaves a tracked file dirty after a
   plain test run.

What is deliberately NOT normalized is the part that proves something: item
shape, key set, nullability, and the frame COUNT.
"""

from __future__ import annotations

import json
import os
from typing import Any

TIMESTAMP = "2026-07-12T10:30:00.123Z"
EVENT_ID = "0123456789abcdef0123456789abcdef"
SESSION_ID = "sess_fixture"
DEVICE_ID = "3f2504e0-4f89-41d3-9a0c-0305e82c3301"
WORKFLOW_ID = "wf_fixture"

_STRING_SUBS = {
    "timestamp": TIMESTAMP,
    "sent_at": TIMESTAMP,
    "event_id": EVENT_ID,
    "session_id": SESSION_ID,
    "device_id": DEVICE_ID,
    "workflow_id": WORKFLOW_ID,
    "raw_stacktrace": "<normalized>",
    "build_id": "<normalized>",
    "isolate_dso_base": "<normalized>",
}

# Stack-frame identity: where the test happened to run, not what the SDK emits.
_FRAME_IDENTITY = {
    "function": "<fn>",
    "module": "<module>",
    "filename": "<file>",
    "abs_path": "<file>",
}

# "<parent>.<key>" paths carrying host- or interpreter-derived values.
# ``context.device`` / ``.os`` / ``.runtime`` are free-form ``serde_json::Value``
# on the wire, so their contents prove nothing — while ``runtime.version`` is the
# CPython version and ``os.name`` is the host platform. ``runtime.name`` is left
# alone deliberately: it is an SDK constant, not a host value.
_HOST_DERIVED = frozenset(
    {
        "os.name",
        "os.version",
        "runtime.version",
        "device.family",
        "device.model",
        "device.arch",
    }
)


def _normalize(node: Any, key: str = "", parent_key: str = "") -> Any:
    if isinstance(node, dict):
        return {k: _normalize(v, k, key) for k, v in node.items()}
    if isinstance(node, list):
        # List children keep the container's key AND its parent, so a frame
        # inside ``stacktrace: [...]`` is still seen as living under it.
        return [_normalize(v, key, parent_key) for v in node]
    if isinstance(node, str):
        if f"{parent_key}.{key}" in _HOST_DERIVED:
            return "<host>"
        if key in _FRAME_IDENTITY:
            return _FRAME_IDENTITY[key]
        if key in _STRING_SUBS:
            return _STRING_SUBS[key]
        return node
    if isinstance(node, bool):
        return node
    if isinstance(node, int):
        if key == "lineno":
            return 42
        if key == "colno":
            return 13
    # ``None`` falls through untouched: nullability is part of what the fixture
    # proves and must never be papered over with a placeholder.
    return node


def fixture_path(sdk: str) -> str:
    # tests/ -> python/ -> sdks/
    sdks = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    return os.path.join(sdks, "wire-fixtures", f"{sdk}.json")


def write_wire_fixture(sdk: str, envelope: Any) -> str:
    path = fixture_path(sdk)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(json.dumps(_normalize(envelope), indent=2))
        fh.write("\n")
    return path
