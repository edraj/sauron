"""The cap on a transaction's ``extra``.

Worth its own suite because the failure it prevents is invisible from the
outside: transactions ship in BATCHED envelopes, and ingest rejects the whole
envelope past ``INGEST_MAX_BODY_BYTES``. One oversized response body does not
lose one span — it loses every unrelated span batched alongside it, with a 400
the transport drops without retrying.
"""

import unittest

from sauron._transaction_extra import (
    MAX_TRANSACTION_EXTRA_BYTES,
    cap_transaction_extra,
)


class TransactionExtraCapTests(unittest.TestCase):
    def test_small_payload_passes_through_unchanged(self):
        extra = {"request": '{"page":1}', "retries": 2}
        self.assertIs(cap_transaction_extra(extra), extra)

    def test_oversized_payload_becomes_a_marker(self):
        capped = cap_transaction_extra(
            {"response": "x" * (MAX_TRANSACTION_EXTRA_BYTES + 1)}
        )
        self.assertTrue(capped["_truncated"])
        self.assertGreater(capped["_bytes"], MAX_TRANSACTION_EXTRA_BYTES)
        # The whole map goes, not just the offending key.
        self.assertNotIn("response", capped)

    def test_measures_utf8_bytes_not_characters(self):
        # Under the cap by character count, over it by bytes. Measured wrong,
        # the envelope is ~2x the size the SDK believed it was sending.
        capped = cap_transaction_extra(
            {"body": "é" * (MAX_TRANSACTION_EXTRA_BYTES - 100)}
        )
        self.assertTrue(capped["_truncated"])

    def test_unserializable_payload_is_marked_not_raised(self):
        # An SDK that crashes the app it is measuring is worse than one that
        # drops a payload.
        class Opaque:
            pass

        capped = cap_transaction_extra({"model": Opaque()})
        self.assertTrue(capped["_truncated"])
        self.assertEqual(capped["_bytes"], -1)

    def test_cycle_is_marked_not_raised(self):
        cyclic: dict = {}
        cyclic["self"] = cyclic
        capped = cap_transaction_extra(cyclic)
        self.assertTrue(capped["_truncated"])
        self.assertEqual(capped["_bytes"], -1)

    def test_limit_matches_every_other_sdk(self):
        self.assertEqual(MAX_TRANSACTION_EXTRA_BYTES, 16 * 1024)


class TrackTransactionMetadataTests(unittest.TestCase):
    """The omit-when-empty rule, at the item-building layer."""

    def setUp(self):
        import sauron

        self.dispatched = []

        class _Recorder(sauron.Client):
            # Signature mirrors `Client._dispatch` exactly, `hint` included —
            # a narrower override would pass here and break the moment a
            # caller supplied one.
            def _dispatch(_self, item, hint=None):  # noqa: N805
                self.dispatched.append(item)

        self.client = _Recorder(dsn="https://pk_test@localhost:8081/1")

    def test_absent_when_not_supplied(self):
        self.client.track_transaction("/x", duration_ms=1.0)
        item = self.dispatched[-1]
        self.assertNotIn("tags", item)
        self.assertNotIn("extra", item)

    def test_absent_when_supplied_but_empty(self):
        self.client.track_transaction("/x", duration_ms=1.0, tags={}, extra={})
        item = self.dispatched[-1]
        self.assertNotIn("tags", item)
        self.assertNotIn("extra", item)

    def test_extra_is_capped_on_the_way_onto_the_item(self):
        self.client.track_transaction(
            "/x",
            duration_ms=1.0,
            extra={"b": "y" * MAX_TRANSACTION_EXTRA_BYTES},
        )
        self.assertTrue(self.dispatched[-1]["extra"]["_truncated"])

    def test_caller_maps_are_copied_not_aliased(self):
        # The item is QUEUED, not sent inline, so a caller mutating their own
        # dict after the call would otherwise change what ships.
        tags = {"tier": "free"}
        self.client.track_transaction("/x", duration_ms=1.0, tags=tags)
        tags["tier"] = "premium"
        self.assertEqual(self.dispatched[-1]["tags"], {"tier": "free"})


if __name__ == "__main__":
    unittest.main()
