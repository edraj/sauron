"""Opt-in auto-capture of uncaught exceptions + graceful atexit shutdown.

Off by default: ``init`` installs no process hooks unless
``auto_capture_unhandled=True``. When enabled the installed ``sys.excepthook``
captures the crash with ``mechanism.handled = False``, flushes, then delegates
to the previous hook so the interpreter's default crash/exit behavior is
preserved.
"""

import sys
import threading
import unittest

import sauron
from sauron._scope import reset_scopes

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"


class TestAutoCapture(unittest.TestCase):
    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)
        self._orig_excepthook = sys.excepthook
        self._orig_threading_hook = getattr(threading, "excepthook", None)

    def tearDown(self):
        sauron.close(timeout=2)
        # Restore process hooks regardless of what the SDK did.
        sys.excepthook = self._orig_excepthook
        if self._orig_threading_hook is not None:
            threading.excepthook = self._orig_threading_hook
        reset_scopes()

    def _init(self, **kwargs):
        return sauron.init(
            DSN,
            flush_interval=3600,
            max_batch=1000,
            sender=self.sender,
            **kwargs,
        )

    # -- opt-in gating -----------------------------------------------------

    def test_off_by_default_installs_no_hook(self):
        before = sys.excepthook
        self._init()
        self.assertIs(sys.excepthook, before)

    def test_enabled_installs_hook(self):
        before = sys.excepthook
        self._init(auto_capture_unhandled=True)
        self.assertIsNot(sys.excepthook, before)

    def test_close_uninstalls_hook(self):
        before = sys.excepthook
        self._init(auto_capture_unhandled=True)
        self.assertIsNot(sys.excepthook, before)
        sauron.close(timeout=2)
        self.assertIs(sys.excepthook, before)

    # -- capture semantics -------------------------------------------------

    def test_uncaught_capture_marks_handled_false_and_chains(self):
        chained = []
        # Install a spy as the "previous" hook so we don't hit the real
        # interpreter default (which would print a traceback to stderr).
        sys.excepthook = lambda *args: chained.append(args)
        self._init(auto_capture_unhandled=True)

        captured_exc = None
        try:
            raise ValueError("kaboom")
        except ValueError as exc:
            captured_exc = exc  # the name ``exc`` is cleared after the block.
            sys.excepthook(type(exc), exc, exc.__traceback__)

        # The crash was captured and flushed synchronously.
        self.assertEqual(len(self.sender.items), 1)
        item = self.sender.items[0]
        self.assertEqual(item["type"], "error")
        self.assertEqual(item["exception"]["type"], "ValueError")
        self.assertEqual(item["exception"]["value"], "kaboom")
        self.assertIs(item["exception"]["mechanism"]["handled"], False)

        # Default crash behavior preserved: the previous hook still ran.
        self.assertEqual(len(chained), 1)
        self.assertIs(chained[0][1], captured_exc)

    def test_keyboard_interrupt_is_not_captured_but_chains(self):
        chained = []
        sys.excepthook = lambda *args: chained.append(args)
        self._init(auto_capture_unhandled=True)

        exc = KeyboardInterrupt()
        sys.excepthook(KeyboardInterrupt, exc, None)

        # Ctrl-C is not an application error: never captured, but still chained.
        self.assertEqual(len(self.sender.items), 0)
        self.assertEqual(len(chained), 1)

    def test_threading_excepthook_captures_handled_false(self):
        if not hasattr(threading, "excepthook"):
            self.skipTest("threading.excepthook requires Python 3.8+")
        chained = []
        # Spy as the previous hook so we chain to it (not pytest's) and stay
        # hermetic — the installed hook must still delegate downstream.
        threading.excepthook = lambda args: chained.append(args)
        self._init(auto_capture_unhandled=True)

        try:
            raise RuntimeError("thread boom")
        except RuntimeError as exc:
            args = threading.ExceptHookArgs(
                (type(exc), exc, exc.__traceback__, threading.current_thread())
            )
            threading.excepthook(args)

        self.assertEqual(len(self.sender.items), 1)
        item = self.sender.items[0]
        self.assertEqual(item["exception"]["type"], "RuntimeError")
        self.assertIs(item["exception"]["mechanism"]["handled"], False)
        self.assertEqual(len(chained), 1)

    # -- graceful shutdown (atexit) ---------------------------------------

    def test_atexit_flush_sends_pending_items(self):
        # The atexit handler must flush anything still buffered at exit.
        self._init()
        sauron.capture_message("goodbye")
        # Huge flush_interval means nothing has been sent yet.
        self.assertEqual(len(self.sender.items), 0)
        sauron._atexit_flush()
        self.assertGreaterEqual(len(self.sender.items), 1)

    def test_atexit_registered_only_once_across_inits(self):
        import atexit
        from unittest import mock

        sauron._atexit_registered = False  # reset for a deterministic count.
        with mock.patch.object(atexit, "register") as reg:
            self._init()
            sauron.close(timeout=2)
            self._init()  # second init must not re-register.
        registrations = [
            c for c in reg.call_args_list if c.args and c.args[0] is sauron._atexit_flush
        ]
        self.assertEqual(len(registrations), 1)


if __name__ == "__main__":
    unittest.main()
