"""Compile-checks the exact API shapes the README documents.

Not a behaviour test — it exists because a README example is the one piece of
this SDK that nothing else exercises. Signatures drift, the docs keep telling
people to write code that raises ``TypeError``, and no suite notices.
"""

import unittest

import sauron
from sauron._transaction_extra import (
    MAX_TRANSACTION_EXTRA_BYTES,
    cap_transaction_extra,
)


class ReadmeShapeTests(unittest.TestCase):
    def test_track_transaction_accepts_the_documented_shape(self):
        # No init(): track_transaction is a no-op without a client, which is
        # exactly what makes this a pure signature check.
        sauron.track_transaction(
            "POST /orders",
            op="http",
            duration_ms=842.5,
            status="ok",
            http_method="POST",
            http_status=201,
            url="https://api.example.com/orders",
            distinct_id="u_1",
            tags={"api": "orders", "tier": "premium"},
            extra={
                "request": "{}",
                "response": "{}",
                "response_bytes": 2,
                "request_headers": ["content-type"],
            },
        )
        sauron.track_transaction(
            "SELECT orders",
            op="db",
            duration_ms=12.0,
            status="ok",
            tags={"db": "postgres", "table": "orders"},
            extra={"statement": "SELECT 1", "row_count": 20, "params": ("u_1",)},
        )

    def test_documented_cap_constant_is_importable(self):
        self.assertEqual(MAX_TRANSACTION_EXTRA_BYTES, 16 * 1024)
        capped = cap_transaction_extra(
            {"a": "x" * (MAX_TRANSACTION_EXTRA_BYTES + 1)}
        )
        self.assertTrue(capped["_truncated"])


if __name__ == "__main__":
    unittest.main()
