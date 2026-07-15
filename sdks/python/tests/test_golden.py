"""Golden-envelope fixture — the standing guard against wire-shape drift.

Mirrors the shared golden in ``backend/crates/sauron-core/src/envelope.rs``
(the ``GOLDEN`` const) and the other SDKs' fixtures (`sdks/js/test/envelope.test.ts`,
`sdks/flutter/test/envelope_test.dart`). It exercises the *reconciled* error
shape — an error item carrying **breadcrumbs + tags + user + fingerprint** — plus
an event, an identify, and a transaction item.

Two levels of assertion:

* **Shape lock** — the canonical item dicts serialize byte-for-byte to the
  golden literal when assembled by :meth:`Client._make_envelope` (the Python
  analogue of the JS ``buildEnvelope`` golden).
* **Behavioral** — the live client, driven through its public API with a scoped
  user/tags/breadcrumb and a fingerprint override, emits exactly that shape
  (dynamic ``event_id`` / timestamps / stacktrace normalized).
"""

import json
import unittest

from sauron._client import SDK_NAME, SDK_VERSION, Client
from sauron._scope import get_current_scope, reset_scopes

from ._fake import FakeSender

DSN = "https://pk_test@localhost:8081/1"

# Fixed stand-ins for the intrinsically dynamic fields (uuid + wall clock), so
# the live envelope can be normalized to the byte-exact golden.
TS = "2026-07-12T10:30:00.123Z"
EVENT_ID = "0123456789abcdef0123456789abcdef"

_FRAME_KEYS = {
    "function", "module", "filename", "abs_path", "lineno", "colno", "in_app",
}

GOLDEN_BREADCRUMB = {
    "type": "navigation",
    "category": "history",
    "message": "went to /settings",
    "level": "info",
    "timestamp": TS,
    "data": {"from": "/", "to": "/settings"},
}

# The reconciled, server-shaped error item: breadcrumbs + tags + user +
# fingerprint, exactly as the Python client builds it (key set + order).
GOLDEN_ERROR = {
    "type": "error",
    "event_id": EVENT_ID,
    "level": "error",
    "timestamp": TS,
    "exception": {
        "type": "TypeError",
        "value": "x is not callable",
        "mechanism": {"type": "generic", "handled": True},
        "stacktrace": [
            {
                "function": "handler",
                "module": None,
                "filename": "app.py",
                "abs_path": "/srv/app.py",
                "lineno": 42,
                "colno": None,
                "in_app": True,
            }
        ],
    },
    "message": None,
    "breadcrumbs": [GOLDEN_BREADCRUMB],
    "tags": {"area": "billing", "tier": "pro"},
    "fingerprint": ["billing", "TypeError"],
    "user": {"id": "u_123", "email": "a@b.co"},
    "session_id": None,
    "screen": None,
}

GOLDEN_EVENT = {
    "type": "event",
    "name": "checkout_completed",
    "distinct_id": "u_123",
    "properties": {"cart_value": 42.5},
    "timestamp": TS,
    "session_id": None,
    "screen": None,
}

GOLDEN_IDENTIFY = {
    "type": "identify",
    "distinct_id": "u_123",
    "anonymous_id": None,
    "traits": {"plan": "pro"},
    "timestamp": TS,
}

GOLDEN_TRANSACTION = {
    "type": "transaction",
    "name": "GET /api/users",
    "op": "http",
    "duration_ms": 128.4,
    "status": "ok",
    "http_method": "GET",
    "http_status": 200,
    "url": "/api/users",
    "distinct_id": "u_123",
    "session_id": None,
    "timestamp": TS,
}

GOLDEN_ITEMS = [GOLDEN_ERROR, GOLDEN_EVENT, GOLDEN_IDENTIFY, GOLDEN_TRANSACTION]


def _normalize(item):
    """Deep-copy an emitted item and pin its dynamic fields to the golden."""
    it = json.loads(json.dumps(item))  # deep copy + proves JSON-serializable.
    if "timestamp" in it:
        it["timestamp"] = TS
    if "event_id" in it:
        it["event_id"] = EVENT_ID
    if it.get("breadcrumbs"):
        it["breadcrumbs"] = [{**b, "timestamp": TS} for b in it["breadcrumbs"]]
    return it


class TestGoldenShapeLock(unittest.TestCase):
    """The canonical items assemble byte-for-byte to the golden envelope."""

    def setUp(self):
        self.sender = FakeSender(status=200)
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

    def test_make_envelope_preserves_golden_items_byte_for_byte(self):
        built = self.client._make_envelope(GOLDEN_ITEMS)
        self.assertEqual(list(built.keys()), ["header", "context", "items"])
        self.assertEqual(built["items"], GOLDEN_ITEMS)
        # Byte parity of the reconciled item shapes.
        self.assertEqual(
            json.dumps(built["items"], sort_keys=True),
            json.dumps(GOLDEN_ITEMS, sort_keys=True),
        )

    def test_header_carries_reconciled_sdk_identity(self):
        built = self.client._make_envelope(GOLDEN_ITEMS)
        self.assertEqual(
            built["header"]["sdk"],
            {"name": "sauron-python", "version": "0.3.0"},
        )
        self.assertEqual(SDK_NAME, "sauron-python")
        self.assertEqual(SDK_VERSION, "0.3.0")

    def test_item_type_discriminators(self):
        types = [i["type"] for i in GOLDEN_ITEMS]
        self.assertEqual(types, ["error", "event", "identify", "transaction"])

    def test_wire_fields_are_snake_case(self):
        # No camelCase leakage on any item; the snake_case wire keys are present.
        self.assertIn("event_id", GOLDEN_ERROR)
        self.assertIn("fingerprint", GOLDEN_ERROR)
        self.assertIn("distinct_id", GOLDEN_EVENT)
        self.assertIn("distinct_id", GOLDEN_IDENTIFY)
        for key in ("duration_ms", "http_method", "http_status", "distinct_id"):
            self.assertIn(key, GOLDEN_TRANSACTION)
        blob = json.dumps(GOLDEN_ITEMS)
        for camel in ("distinctId", "eventId", "httpStatus", "durationMs"):
            self.assertNotIn(camel, blob)


class TestGoldenClientEmitsShape(unittest.TestCase):
    """The live client, driven via its public API, emits the golden shape."""

    def setUp(self):
        reset_scopes()
        self.sender = FakeSender(status=200)
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
        reset_scopes()

    def _emit_golden(self):
        # Scoped user, tags, and a breadcrumb — the reconciliation surface.
        client = self.client
        get_current_scope().set_user({"id": "u_123", "email": "a@b.co"})
        get_current_scope().set_tag("area", "billing")
        get_current_scope().set_tags({"tier": "pro"})
        client.add_breadcrumb(
            type="navigation",
            category="history",
            message="went to /settings",
            level="info",
            data={"from": "/", "to": "/settings"},
        )
        try:
            raise TypeError("x is not callable")
        except TypeError as exc:
            client.capture_exception(exc, fingerprint=["billing", "TypeError"])
        client.track("checkout_completed", "u_123", {"cart_value": 42.5})
        client.identify("u_123", {"plan": "pro"})
        client.track_transaction(
            "GET /api/users",
            op="http",
            duration_ms=128.4,
            status="ok",
            http_method="GET",
            http_status=200,
            url="/api/users",
            distinct_id="u_123",
        )
        client.flush()
        return self.sender.items

    def test_emitted_items_match_golden(self):
        items = self._emit_golden()
        self.assertEqual(len(items), 4)
        self.assertEqual(
            [i["type"] for i in items],
            ["error", "event", "identify", "transaction"],
        )

        error = items[0]
        # The live stacktrace is real: assert its shape, then pin it to the
        # golden's synthetic frame so the rest compares byte-for-byte.
        stack = error["exception"]["stacktrace"]
        self.assertGreaterEqual(len(stack), 1)
        self.assertTrue(stack[-1]["in_app"])  # crashing frame is app code.
        self.assertEqual(set(stack[0].keys()), _FRAME_KEYS)

        norm_error = _normalize(error)
        norm_error["exception"]["stacktrace"] = GOLDEN_ERROR["exception"][
            "stacktrace"
        ]
        self.assertEqual(norm_error, GOLDEN_ERROR)

        self.assertEqual(_normalize(items[1]), GOLDEN_EVENT)
        self.assertEqual(_normalize(items[2]), GOLDEN_IDENTIFY)
        self.assertEqual(_normalize(items[3]), GOLDEN_TRANSACTION)

    def test_reconciled_fields_present_on_error(self):
        error = self._emit_golden()[0]
        # The four reconciliation fields the servers previously omitted.
        self.assertEqual(
            [c["message"] for c in error["breadcrumbs"]], ["went to /settings"]
        )
        self.assertEqual(error["tags"], {"area": "billing", "tier": "pro"})
        self.assertEqual(error["user"], {"id": "u_123", "email": "a@b.co"})
        self.assertEqual(error["fingerprint"], ["billing", "TypeError"])


if __name__ == "__main__":
    unittest.main()
