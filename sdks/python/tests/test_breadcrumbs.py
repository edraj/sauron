import unittest

import sauron
from sauron._scope import get_global_scope, reset_scopes, scope

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"


class TestBreadcrumbs(unittest.TestCase):
    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)

    def tearDown(self):
        sauron.close(timeout=2)
        reset_scopes()

    def _init(self, **kwargs):
        return sauron.init(
            DSN, flush_interval=3600, max_batch=1000, sender=self.sender,
            **kwargs,
        )

    def test_breadcrumb_attaches_to_captured_error(self):
        self._init()
        sauron.add_breadcrumb(category="ui", message="clicked login")
        sauron.add_breadcrumb(type="http", message="GET /me")
        try:
            raise ValueError("boom")
        except ValueError as exc:
            sauron.capture_exception(exc)
        sauron.flush()
        item = self.sender.items[0]
        crumbs = item["breadcrumbs"]
        self.assertEqual(len(crumbs), 2)
        self.assertEqual(crumbs[0]["message"], "clicked login")
        self.assertEqual(crumbs[0]["category"], "ui")
        self.assertEqual(crumbs[1]["type"], "http")

    def test_breadcrumb_stamps_iso_timestamp(self):
        self._init()
        sauron.add_breadcrumb(message="x")
        sauron.capture_message("done")
        sauron.flush()
        crumb = self.sender.items[0]["breadcrumbs"][0]
        self.assertIn("timestamp", crumb)
        self.assertTrue(crumb["timestamp"].endswith("Z"))
        self.assertIn("T", crumb["timestamp"])

    def test_before_breadcrumb_can_drop(self):
        self._init(before_breadcrumb=lambda c: None if c.get("category") == "noise" else c)
        sauron.add_breadcrumb(category="noise", message="drop me")
        sauron.add_breadcrumb(category="ui", message="keep me")
        sauron.capture_message("done")
        sauron.flush()
        crumbs = self.sender.items[0]["breadcrumbs"]
        self.assertEqual(len(crumbs), 1)
        self.assertEqual(crumbs[0]["message"], "keep me")

    def test_before_breadcrumb_can_mutate(self):
        def redact(crumb):
            if crumb.get("message"):
                crumb["message"] = "[redacted]"
            return crumb

        self._init(before_breadcrumb=redact)
        sauron.add_breadcrumb(message="secret")
        sauron.capture_message("done")
        sauron.flush()
        crumb = self.sender.items[0]["breadcrumbs"][0]
        self.assertEqual(crumb["message"], "[redacted]")

    def test_scope_setters_land_on_error(self):
        self._init()
        sauron.set_user({"id": "u_42", "email": "z@z.co"})
        sauron.set_tag("area", "checkout")
        sauron.set_tags({"tier": "pro"})
        sauron.capture_message("hi")
        sauron.flush()
        item = self.sender.items[0]
        self.assertEqual(item["user"], {"id": "u_42", "email": "z@z.co"})
        self.assertEqual(item["tags"], {"area": "checkout", "tier": "pro"})

    def test_scoped_block_isolates_breadcrumbs(self):
        self._init()
        sauron.add_breadcrumb(message="global-crumb")
        with scope():
            sauron.add_breadcrumb(message="req-crumb")
            sauron.capture_message("inside")
        sauron.capture_message("outside")
        sauron.flush()
        inside = self.sender.items[0]
        outside = self.sender.items[1]
        self.assertEqual(
            [c["message"] for c in inside["breadcrumbs"]],
            ["global-crumb", "req-crumb"],
        )
        # The request crumb must not leak back onto the global scope.
        self.assertEqual(
            [c["message"] for c in outside["breadcrumbs"]], ["global-crumb"]
        )

    def test_max_breadcrumbs_option_bounds_ring(self):
        self._init(max_breadcrumbs=2)
        for i in range(4):
            sauron.add_breadcrumb(message=str(i))
        sauron.capture_message("done")
        sauron.flush()
        crumbs = self.sender.items[0]["breadcrumbs"]
        self.assertEqual([c["message"] for c in crumbs], ["2", "3"])

    def test_add_breadcrumb_before_init_is_safe(self):
        # No client yet: must not raise, and may seed the global scope.
        sauron.add_breadcrumb(message="early")
        self.assertEqual(
            [c["message"] for c in get_global_scope().breadcrumbs], ["early"]
        )


if __name__ == "__main__":
    unittest.main()
