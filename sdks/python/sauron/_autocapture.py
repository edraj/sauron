"""Opt-in capture of *uncaught* exceptions.

Off by default (the ``auto_capture_unhandled`` init flag). When enabled,
:func:`install_excepthook` wraps ``sys.excepthook`` — and ``threading.excepthook``
on Python 3.8+ — so a crash that reaches the top level is captured with
``mechanism.handled = False`` and flushed **before** delegating to the previously
installed hook. Delegating last preserves the interpreter's default crash
behavior (print the traceback, non-zero exit); the SDK never swallows the error.

``KeyboardInterrupt`` (Ctrl-C) is deliberately *not* captured — it is a user
signal, not an application fault — but the previous hook still runs so exit
semantics are unchanged.
"""

from __future__ import annotations

import sys
import threading
from typing import Any, Callable

# The mechanism stamped onto errors that came from an uncaught handler. Mirrors
# the browser SDK's ``onunhandledrejection`` marker: ``handled = False``.
_SYS_MECHANISM = {"type": "excepthook", "handled": False}
_THREAD_MECHANISM = {"type": "threading.excepthook", "handled": False}


def _should_capture(exc_type: type) -> bool:
    """Skip user-initiated interrupts; capture genuine faults."""
    return not (
        exc_type is None or issubclass(exc_type, (KeyboardInterrupt,))
    )


def install_excepthook(client: Any) -> Callable[[], None]:
    """Install uncaught-exception hooks that report through ``client``.

    Returns an idempotent uninstaller that restores the previous hooks (only if
    ours are still the active ones — so nested installs unwind cleanly).
    """
    previous_sys_hook = sys.excepthook
    previous_thread_hook = getattr(threading, "excepthook", None)

    def sys_hook(exc_type, exc, tb):
        if _should_capture(exc_type):
            try:
                client.capture_exception(
                    exc, level="fatal", mechanism=_SYS_MECHANISM
                )
                client.flush()
            except Exception:  # a reporting failure must never mask the crash
                pass
        # Preserve default crash/exit behavior — delegate to the prior hook.
        previous_sys_hook(exc_type, exc, tb)

    def thread_hook(args):
        exc_type = args.exc_type
        exc = args.exc_value
        if exc is not None and _should_capture(exc_type):
            try:
                client.capture_exception(
                    exc, level="fatal", mechanism=_THREAD_MECHANISM
                )
                client.flush()
            except Exception:
                pass
        if previous_thread_hook is not None:
            previous_thread_hook(args)

    sys.excepthook = sys_hook
    if previous_thread_hook is not None:
        threading.excepthook = thread_hook

    def uninstall() -> None:
        if sys.excepthook is sys_hook:
            sys.excepthook = previous_sys_hook
        if (
            previous_thread_hook is not None
            and getattr(threading, "excepthook", None) is thread_hook
        ):
            threading.excepthook = previous_thread_hook

    return uninstall
