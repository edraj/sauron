import unittest

from sauron._client import Client
from sauron._transport import Transport

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"


class _StubDsn:
    envelope_url = "https://localhost:8081/api/1/envelope"
    public_key = "pk_test"


def _make_envelope(items):
    return {"header": {"sdk": {"name": "sauron-python", "version": "0.1.0"}},
            "context": {}, "items": items}


class TestTransportBatching(unittest.TestCase):
    def test_flush_sends_all_buffered_items_in_one_envelope(self):
        sender = FakeSender()
        t = Transport(
            _StubDsn(),
            _make_envelope,
            sender=sender,
            flush_interval=3600,
            max_batch=1000,
        )
        for i in range(5):
            t.capture({"type": "event", "name": f"e{i}", "distinct_id": "u"})
        # Nothing sent until flush.
        self.assertEqual(len(sender.calls), 0)
        t.flush()
        # A single envelope carrying all five items.
        self.assertEqual(len(sender.calls), 1)
        self.assertEqual(len(sender.items), 5)
        t.close(timeout=2)

    def test_empty_flush_sends_nothing(self):
        sender = FakeSender()
        t = Transport(_StubDsn(), _make_envelope, sender=sender,
                      flush_interval=3600)
        t.flush()
        self.assertEqual(len(sender.calls), 0)
        t.close(timeout=2)

    def test_max_batch_triggers_background_flush(self):
        sender = FakeSender()
        t = Transport(
            _StubDsn(),
            _make_envelope,
            sender=sender,
            flush_interval=3600,
            max_batch=3,
        )
        t.start()
        for i in range(3):
            t.capture({"type": "event", "name": f"e{i}", "distinct_id": "u"})
        # The worker wakes on the full batch; give it a moment.
        deadline_ok = False
        import time

        for _ in range(50):
            if len(sender.items) >= 3:
                deadline_ok = True
                break
            time.sleep(0.02)
        self.assertTrue(deadline_ok, "background flush did not fire on max_batch")
        t.close(timeout=2)

    def test_auth_failure_disables_and_stops_sending(self):
        disabled = {"v": False}
        sender = FakeSender(status=401)
        t = Transport(
            _StubDsn(),
            _make_envelope,
            sender=sender,
            flush_interval=3600,
            max_retries=0,
            on_disable=lambda: disabled.__setitem__("v", True),
        )
        t.capture({"type": "event", "name": "e", "distinct_id": "u"})
        t.flush()
        self.assertTrue(disabled["v"])
        # After disable, further captures are dropped.
        t.capture({"type": "event", "name": "e2", "distinct_id": "u"})
        t.flush()
        self.assertEqual(len(sender.calls), 1)
        t.close(timeout=2)

    def test_transient_failure_retries_then_drops(self):
        sender = FakeSender(status=503)
        t = Transport(
            _StubDsn(),
            _make_envelope,
            sender=sender,
            flush_interval=3600,
            max_retries=2,
            retry_base=0.0,
        )
        t.capture({"type": "event", "name": "e", "distinct_id": "u"})
        t.flush()
        # 1 initial attempt + 2 retries = 3 POSTs, then dropped.
        self.assertEqual(len(sender.calls), 3)
        t.close(timeout=2)


class TestClientInjectedSender(unittest.TestCase):
    def test_client_uses_injected_sender_no_network(self):
        sender = FakeSender()
        client = Client(DSN, flush_interval=3600, sender=sender)
        client.track("e", "u_1")
        client.flush()
        self.assertEqual(len(sender.calls), 1)
        self.assertEqual(
            sender.calls[0]["url"], "https://localhost:8081/api/1/envelope"
        )
        client.close(timeout=2)


if __name__ == "__main__":
    unittest.main()
