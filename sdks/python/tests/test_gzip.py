"""Gzip transport compression (task B5).

The request body is gzipped only when it exceeds ``gzip_threshold_bytes`` and,
when it is, the ``Content-Encoding: gzip`` header is set and the body round-trips
back to the original JSON.
"""

import gzip
import json

from sauron._gzip import maybe_gzip
from sauron._transport import Transport


class _StubDsn:
    envelope_url = "https://localhost:8081/api/1/envelope"
    public_key = "pk_test"


def _make_envelope(items):
    return {
        "header": {"sdk": {"name": "sauron-python", "version": "0.1.0"}},
        "context": {},
        "items": items,
    }


# -- maybe_gzip unit ------------------------------------------------------


def test_gzip_above_threshold_roundtrips():
    body = b'{"blob":"' + b"a" * 4000 + b'"}'
    out, headers = maybe_gzip(body, 1024)
    assert headers.get("Content-Encoding") == "gzip"
    assert out != body
    assert len(out) < len(body)
    assert gzip.decompress(out) == body


def test_passthrough_below_threshold():
    body = b'{"x":"short"}'
    out, headers = maybe_gzip(body, 1024)
    assert out == body
    assert headers == {}


def test_boundary_equal_is_passthrough():
    # "exceeds" means strictly greater than the threshold.
    body = b"a" * 1024
    out, headers = maybe_gzip(body, 1024)
    assert out == body
    assert headers == {}


# -- transport integration ------------------------------------------------


def test_transport_gzips_large_body_and_sets_header():
    captured = {}

    def sender(url, headers, body):
        captured["headers"] = dict(headers)
        captured["body"] = body
        return 200

    t = Transport(
        _StubDsn(),
        _make_envelope,
        sender=sender,
        flush_interval=3600,
        gzip_threshold_bytes=1024,
    )
    t.capture(
        {
            "type": "event",
            "name": "big",
            "distinct_id": "u",
            "properties": {"blob": "x" * 4000},
        }
    )
    t.flush()
    assert captured["headers"].get("Content-Encoding") == "gzip"
    decompressed = gzip.decompress(captured["body"])
    data = json.loads(decompressed.decode("utf-8"))
    assert data["items"][0]["properties"]["blob"] == "x" * 4000
    t.close(timeout=2)


def test_transport_leaves_small_body_uncompressed():
    captured = {}

    def sender(url, headers, body):
        captured["headers"] = dict(headers)
        captured["body"] = body
        return 200

    t = Transport(
        _StubDsn(),
        _make_envelope,
        sender=sender,
        flush_interval=3600,
        gzip_threshold_bytes=1024,
    )
    t.capture({"type": "event", "name": "small", "distinct_id": "u"})
    t.flush()
    assert "Content-Encoding" not in captured["headers"]
    # Body is plain JSON.
    json.loads(captured["body"].decode("utf-8"))
    t.close(timeout=2)
