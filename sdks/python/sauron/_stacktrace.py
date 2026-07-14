"""Stack-trace extraction from native Python exceptions.

Frames are emitted in the wire-contract order: **call site first, crashing
frame last**. ``traceback.extract_tb`` already yields frames oldest-to-newest
(the point where the exception was raised comes last), so no reordering is
needed.
"""

from __future__ import annotations

import os
import sys
import traceback
from typing import Any, Dict, List, Optional

# Path fragments that mark third-party / stdlib code (not the app's own).
_VENDOR_MARKERS = (
    os.sep + "site-packages" + os.sep,
    os.sep + "dist-packages" + os.sep,
)


def _is_in_app(abs_path: Optional[str]) -> Optional[bool]:
    """Heuristic: is this frame part of the application's own code?

    ``False`` for stdlib / installed-package frames, ``True`` otherwise, and
    ``None`` when there is no path to judge by.
    """
    if not abs_path:
        return None

    norm = os.path.normcase(os.path.abspath(abs_path))

    for marker in _VENDOR_MARKERS:
        if os.path.normcase(marker) in norm:
            return False

    # Frames living under the interpreter prefix are stdlib.
    for prefix in {sys.prefix, sys.base_prefix}:
        if not prefix:
            continue
        root = os.path.normcase(os.path.abspath(prefix))
        try:
            if os.path.commonpath([norm, root]) == root:
                return False
        except ValueError:
            # Different drives on Windows — not comparable, treat as app code.
            continue

    return True


def extract_stacktrace(exc: BaseException) -> List[Dict[str, Any]]:
    """Extract wire-contract frames from an exception's traceback.

    Returns an empty list when the exception has no traceback attached.
    """
    tb = getattr(exc, "__traceback__", None)
    if tb is None:
        return []

    frames: List[Dict[str, Any]] = []
    for fs in traceback.extract_tb(tb):
        abs_path = fs.filename
        frames.append(
            {
                "function": fs.name,
                "module": None,
                "filename": os.path.basename(abs_path) if abs_path else None,
                "abs_path": abs_path,
                "lineno": fs.lineno,
                # colno is available on FrameSummary in Python 3.11+.
                "colno": getattr(fs, "colno", None),
                "in_app": _is_in_app(abs_path),
            }
        )
    return frames


def exception_type_name(exc: BaseException) -> str:
    """The fully-qualified class name of an exception, e.g. ``ValueError`` or
    ``mypkg.errors.BoomError``."""
    cls = type(exc)
    module = cls.__module__
    if module in (None, "builtins", "__main__"):
        return cls.__qualname__
    return f"{module}.{cls.__qualname__}"
