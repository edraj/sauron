import unittest

import sauron
from sauron._scope import reset_scopes

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"


class TestBeforeSend(unittest.TestCase):
    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)

    def tearDown(self):
        sauron.close(timeout=2)
        reset_scopes()

    def _init(self, before_send):
        return sauron.init(
            DSN,
            flush_interval=3600,
            max_batch=1000,
            sender=self.sender,
            before_send=before_send,
        )

    def test_returning_none_drops_error(self):
        self._init(lambda item, hint=None: None if item["type"] == "error" else item)
        sauron.track("kept", "u_1")
        try:
            raise ValueError("boom")
        except ValueError as exc:
            sauron.capture_exception(exc)
        sauron.flush()
        types = [i["type"] for i in self.sender.items]
        self.assertEqual(types, ["event"])

    def test_can_mutate_event_properties(self):
        def redact(item, hint=None):
            if item["type"] == "event":
                props = item.get("properties") or {}
                if "email" in props:
                    props["email"] = "[redacted]"
            return item

        self._init(redact)
        sauron.track("signed_up", "u_1", {"email": "a@b.co", "plan": "pro"})
        sauron.flush()
        props = self.sender.items[0]["properties"]
        self.assertEqual(props["email"], "[redacted]")
        self.assertEqual(props["plan"], "pro")

    def test_runs_on_every_item_type(self):
        seen = []

        def collect(item, hint=None):
            seen.append(item["type"])
            return item

        self._init(collect)
        sauron.track("evt", "u_1")
        sauron.identify("u_1", {"plan": "pro"})
        sauron.capture_message("hi")
        sauron.flush()
        self.assertEqual(sorted(seen), ["error", "event", "identify"])

    def test_returning_replacement_object_is_used(self):
        def replace(item, hint=None):
            if item["type"] == "identify":
                item["traits"] = {"scrubbed": True}
            return item

        self._init(replace)
        sauron.identify("u_1", {"secret": "x"})
        sauron.flush()
        self.assertEqual(
            self.sender.items[0]["traits"], {"scrubbed": True}
        )

    def test_before_send_exception_drops_item_safely(self):
        def boom(item, hint=None):
            raise RuntimeError("hook error")

        self._init(boom)
        sauron.track("evt", "u_1")
        sauron.flush()
        # A crashing hook must drop the item, not the process.
        self.assertEqual(len(self.sender.items), 0)


if __name__ == "__main__":
    unittest.main()
