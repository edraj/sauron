"""Captures the envelope this SDK **actually posts** into
``sdks/wire-fixtures/python.json``, where the backend's
``cargo test -p sauron-core --test sdk_wire_conformance`` feeds it through the
real ``serde`` deserializer.

``test_golden.py`` compares against a literal authored *in this repo*, which is
exactly how the ``js`` SDK shipped ``exception.type: null`` — wire-invalid
against a non-``Option`` ``String`` — while passing every test on both sides.
The envelope is all-or-nothing, so that one item 400s the whole batch and every
SDK drops a 400 without retrying.

Python's ``capture_message`` shape (omit ``exception``, set ``message``) is the
one the other SDKs are being moved to, so this fixture is also the reference.
"""

from __future__ import annotations

import unittest

from sauron._client import Client
from sauron._scope import get_current_scope, reset_scopes

from ._fake import FakeSender
from ._wire_fixture_io import write_wire_fixture

DSN = "https://pk_test@localhost:8081/1"


class TestWireFixture(unittest.TestCase):
    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)
        self.client = Client(
            DSN,
            release="svc@1.4.2",
            flush_interval=3600,
            max_batch=1000,
            sender=self.sender,
        )

    def tearDown(self):
        self.client.close(timeout=2)
        reset_scopes()

    def test_posted_envelope_is_written_as_the_wire_fixture(self):
        scope = get_current_scope()
        scope.set_user({"id": "u_123", "email": "a@b.co"})
        scope.set_tag("env", "prod")
        self.client.add_breadcrumb(
            type="navigation",
            category="history",
            message="went to /settings",
            level="info",
            data={"from": "/", "to": "/settings"},
        )

        self.client.identify("u_123", {"plan": "pro"})
        self.client.track("checkout_completed", "u_123", {"cart_value": 42.5})
        try:
            raise TypeError("x is not callable")
        except TypeError as exc:
            self.client.capture_exception(exc)
        self.client.capture_message(
            "payment provider returned a soft decline", "warning"
        )
        self.client.track_transaction(
            "GET /api/users",
            op="http",
            duration_ms=128.4,
            status="ok",
            http_method="GET",
            http_status=200,
            url="/api/users",
            distinct_id="u_123",
            # Exercised in the fixture so the backend's ``serde`` deserializer
            # sees real values in these two fields, not just their absence.
            tags={"tier": "premium"},
            extra={"request": '{"page":1}', "response": '{"users":[]}'},
        )
        # A SECOND transaction with neither field set — the omit-when-empty
        # rule is the half a fixture with only the populated case cannot see,
        # and it is the half that guarantees an app not using this feature
        # ships identical bytes.
        self.client.track_transaction(
            "/checkout", op="navigation", duration_ms=42.0
        )
        self.assertTrue(self.client.flush(timeout=5))

        self.assertEqual(len(self.sender.envelopes), 1)
        envelope = self.sender.envelopes[0]
        types = [i["type"] for i in envelope["items"]]
        for required in ("error", "event", "identify", "transaction"):
            self.assertIn(required, types)
        self.assertEqual(types.count("error"), 2)  # exception + message

        # Both error items must carry their text where the backend reads it, and
        # any exception block must carry a real type string (non-``Option`` on
        # the wire).
        for item in envelope["items"]:
            if item["type"] != "error":
                continue
            exception = item.get("exception")
            if exception is not None:
                self.assertIsInstance(exception.get("type"), str)
                self.assertNotEqual(exception["type"], "")
            text = item.get("message") or (exception or {}).get("value")
            self.assertTrue(text)

        write_wire_fixture("python", envelope)


if __name__ == "__main__":
    unittest.main()
