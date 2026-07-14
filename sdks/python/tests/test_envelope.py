import unittest

from sauron._client import SDK_NAME, SDK_VERSION, Client

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"


class TestEnvelopeWireContract(unittest.TestCase):
    def setUp(self):
        self.sender = FakeSender(status=200)
        # Large flush_interval so only explicit flush() sends (deterministic).
        self.client = Client(
            DSN,
            environment="production",
            release="svc@1.2.3",
            flush_interval=3600,
            max_batch=1000,
            sender=self.sender,
        )

    def tearDown(self):
        self.client.close(timeout=2)

    # -- transport-level assertions ---------------------------------------

    def test_post_url_and_key_header(self):
        self.client.track("signed_up", "u_1")
        self.client.flush()
        self.assertEqual(len(self.sender.calls), 1)
        call = self.sender.calls[0]
        self.assertEqual(call["url"], "https://localhost:8081/api/1/envelope")
        self.assertEqual(call["headers"]["X-Sauron-Key"], "pk_test")
        self.assertEqual(
            call["headers"]["Content-Type"], "application/json"
        )

    def test_envelope_header_and_context(self):
        self.client.track("signed_up", "u_1")
        self.client.flush()
        env = self.sender.envelopes[0]
        self.assertEqual(env["header"]["sdk"], {
            "name": SDK_NAME,
            "version": SDK_VERSION,
        })
        self.assertEqual(SDK_NAME, "sauron-python")
        self.assertEqual(SDK_VERSION, "0.1.0")
        self.assertEqual(env["header"]["dsn"], DSN)
        self.assertEqual(env["header"]["environment"], "production")
        self.assertEqual(env["header"]["release"], "svc@1.2.3")
        self.assertIn("sent_at", env["header"])

        ctx = env["context"]
        self.assertIn("device_id", ctx["device"])
        self.assertEqual(ctx["runtime"]["name"], "python")
        self.assertIsNotNone(ctx["runtime"]["version"])
        self.assertIsNone(ctx["user"])
        self.assertEqual(ctx["app"], {})

    # -- item shapes ------------------------------------------------------

    def test_event_item_shape(self):
        self.client.track(
            "checkout_completed", "u_123", {"cart_value": 42.5}
        )
        self.client.flush()
        item = self.sender.items[0]
        self.assertEqual(item["type"], "event")
        self.assertEqual(item["name"], "checkout_completed")
        self.assertEqual(item["distinct_id"], "u_123")
        self.assertEqual(item["properties"], {"cart_value": 42.5})
        self.assertIn("timestamp", item)
        self.assertIsNone(item["session_id"])
        self.assertIsNone(item["screen"])

    def test_identify_item_shape(self):
        self.client.identify("u_123", {"plan": "pro"})
        self.client.flush()
        item = self.sender.items[0]
        self.assertEqual(item["type"], "identify")
        self.assertEqual(item["distinct_id"], "u_123")
        self.assertIsNone(item["anonymous_id"])
        self.assertEqual(item["traits"], {"plan": "pro"})
        self.assertIn("timestamp", item)

    def test_capture_message_item_shape(self):
        self.client.capture_message("worker started", level="info")
        self.client.flush()
        item = self.sender.items[0]
        self.assertEqual(item["type"], "error")
        self.assertEqual(item["level"], "info")
        self.assertEqual(item["message"], "worker started")
        self.assertIsNone(item["exception"])
        self.assertEqual(item["breadcrumbs"], [])
        self.assertEqual(item["tags"], {})
        self.assertIsNone(item["fingerprint"])
        self.assertIn("event_id", item)

    def test_capture_exception_item_shape(self):
        try:
            raise TypeError("x is not callable")
        except TypeError as exc:
            self.client.capture_exception(
                exc,
                user={"id": "u_9", "email": "a@b.co"},
                tags={"area": "billing"},
            )
        self.client.flush()
        item = self.sender.items[0]
        self.assertEqual(item["type"], "error")
        self.assertEqual(item["level"], "error")
        exc = item["exception"]
        self.assertEqual(exc["type"], "TypeError")
        self.assertEqual(exc["value"], "x is not callable")
        self.assertEqual(
            exc["mechanism"], {"type": "generic", "handled": True}
        )
        self.assertGreaterEqual(len(exc["stacktrace"]), 1)
        # Crashing frame is last and is app code.
        self.assertTrue(exc["stacktrace"][-1]["in_app"])
        self.assertEqual(item["user"], {
            "id": "u_9",
            "email": "a@b.co",
            "username": None,
        })
        self.assertEqual(item["tags"], {"area": "billing"})

    def test_capture_exception_reads_active_exception(self):
        try:
            raise RuntimeError("bare")
        except RuntimeError:
            event_id = self.client.capture_exception()
        self.assertIsNotNone(event_id)
        self.client.flush()
        item = self.sender.items[0]
        self.assertEqual(item["exception"]["type"], "RuntimeError")
        self.assertEqual(item["exception"]["value"], "bare")


class TestSampling(unittest.TestCase):
    def test_sample_rate_zero_drops_errors(self):
        sender = FakeSender()
        client = Client(
            DSN, sample_rate=0.0, flush_interval=3600, sender=sender
        )
        try:
            raise ValueError("nope")
        except ValueError as exc:
            client.capture_exception(exc)
        client.flush()
        self.assertEqual(len(sender.items), 0)
        client.close(timeout=2)


if __name__ == "__main__":
    unittest.main()
