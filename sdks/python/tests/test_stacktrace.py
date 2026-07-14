import unittest

from sauron._stacktrace import (
    exception_type_name,
    extract_stacktrace,
)


def _inner():
    raise ValueError("boom")


def _outer():
    _inner()


class TestStacktrace(unittest.TestCase):
    def _capture(self):
        try:
            _outer()
        except ValueError as exc:
            return exc
        self.fail("expected ValueError")

    def test_crash_frame_is_last(self):
        exc = self._capture()
        frames = extract_stacktrace(exc)
        self.assertGreaterEqual(len(frames), 2)
        # Frames are call-site -> crash. The raising frame (_inner) is last.
        self.assertEqual(frames[-1]["function"], "_inner")
        # The call site (_outer) precedes it.
        functions = [f["function"] for f in frames]
        self.assertIn("_outer", functions)
        self.assertLess(functions.index("_outer"), functions.index("_inner"))

    def test_frame_shape(self):
        exc = self._capture()
        frame = extract_stacktrace(exc)[-1]
        self.assertEqual(
            set(frame.keys()),
            {
                "function",
                "module",
                "filename",
                "abs_path",
                "lineno",
                "colno",
                "in_app",
            },
        )
        self.assertEqual(frame["filename"], "test_stacktrace.py")
        self.assertTrue(frame["abs_path"].endswith("test_stacktrace.py"))
        self.assertIsInstance(frame["lineno"], int)

    def test_in_app_true_for_test_code(self):
        exc = self._capture()
        frame = extract_stacktrace(exc)[-1]
        # This test file is app code, not stdlib / site-packages.
        self.assertTrue(frame["in_app"])

    def test_no_traceback_returns_empty(self):
        self.assertEqual(extract_stacktrace(ValueError("detached")), [])

    def test_exception_type_name_builtin(self):
        self.assertEqual(exception_type_name(ValueError("x")), "ValueError")


if __name__ == "__main__":
    unittest.main()
