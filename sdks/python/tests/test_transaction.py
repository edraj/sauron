import unittest

import sauron
from sauron._scope import reset_scopes

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"


class TestTrackTransaction(unittest.TestCase):
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

    def test_emits_transaction_item_with_fields(self):
        self._init()
        sauron.track_transaction(
            "GET /api/users",
            op="http",
            duration_ms=128.4,
            status="ok",
            http_method="GET",
            http_status=200,
            url="/api/users",
            distinct_id="u_1",
        )
        sauron.flush()
        item = self.sender.items[0]
        self.assertEqual(item["type"], "transaction")
        self.assertEqual(item["name"], "GET /api/users")
        self.assertEqual(item["op"], "http")
        self.assertEqual(item["duration_ms"], 128.4)
        self.assertEqual(item["status"], "ok")
        self.assertEqual(item["http_method"], "GET")
        self.assertEqual(item["http_status"], 200)
        self.assertEqual(item["url"], "/api/users")
        self.assertEqual(item["distinct_id"], "u_1")
        self.assertIn("timestamp", item)

    def test_op_defaults_to_custom(self):
        self._init()
        sauron.track_transaction("checkout", duration_ms=10)
        sauron.flush()
        self.assertEqual(self.sender.items[0]["op"], "custom")

    def test_duration_is_float(self):
        self._init()
        sauron.track_transaction("t", duration_ms=12)
        sauron.flush()
        duration = self.sender.items[0]["duration_ms"]
        self.assertIsInstance(duration, float)
        self.assertEqual(duration, 12.0)

    def test_distinct_id_falls_back_to_scoped_user(self):
        self._init()
        sauron.set_user({"id": "u_scoped"})
        sauron.track_transaction("job", op="custom", duration_ms=5)
        sauron.flush()
        self.assertEqual(self.sender.items[0]["distinct_id"], "u_scoped")

    def test_explicit_distinct_id_wins_over_scope(self):
        self._init()
        sauron.set_user({"id": "u_scoped"})
        sauron.track_transaction(
            "job", duration_ms=5, distinct_id="u_explicit"
        )
        sauron.flush()
        self.assertEqual(self.sender.items[0]["distinct_id"], "u_explicit")

    def test_distinct_id_is_none_without_user_or_arg(self):
        self._init()
        sauron.track_transaction("job", duration_ms=5)
        sauron.flush()
        self.assertIsNone(self.sender.items[0]["distinct_id"])

    def test_before_send_applies_to_transactions(self):
        self._init(
            before_send=lambda item, hint=None: None
            if item["type"] == "transaction"
            else item
        )
        sauron.track_transaction("job", duration_ms=5)
        sauron.flush()
        self.assertEqual(len(self.sender.items), 0)

    def test_no_op_before_init(self):
        # No init: must be a silent no-op, never raise.
        sauron.track_transaction("job", duration_ms=5)
        self.assertEqual(len(self.sender.items), 0)


if __name__ == "__main__":
    unittest.main()
