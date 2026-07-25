"""Retry / backoff policy alignment (task B6).

Shared policy table:
- retry on 408 / 429 / 5xx and network errors
- 413 is NOT retried unchanged: the envelope is split, and a lone item that
  still will not fit is dropped
- honor ``Retry-After`` on 429
- DROP (no retry) on 400 / 401 / 403 / 404
- give up after ``max_retries`` retries (default 3) and drop the batch
- back-off is capped at 30s

Determinism: an injected ``sleep`` records delays instead of blocking, and a
scripted sender returns a queued sequence of responses (int status, a
``(status, headers)`` tuple, or an ``Exception`` to raise as a network error).
"""

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


class ScriptedSender:
    """Returns a queued sequence of responses; the last one repeats."""

    def __init__(self, responses):
        self.responses = list(responses)
        self.calls = 0

    def __call__(self, url, headers, body):
        idx = min(self.calls, len(self.responses) - 1)
        self.calls += 1
        r = self.responses[idx]
        if isinstance(r, BaseException):
            raise r
        return r


def _transport(sender, **kwargs):
    slept = []
    kwargs.setdefault("flush_interval", 3600)
    kwargs.setdefault("retry_base", 0.0)
    t = Transport(
        _StubDsn(),
        _make_envelope,
        sender=sender,
        sleep=lambda d: slept.append(d),
        **kwargs,
    )
    t._slept = slept
    return t


def _drive(sender, **kwargs):
    t = _transport(sender, **kwargs)
    t.capture({"type": "event", "name": "e", "distinct_id": "u"})
    t.flush()
    t.close(timeout=2)
    return t


# -- retry set ------------------------------------------------------------


def test_429_then_200_two_sends_and_honors_retry_after():
    sender = ScriptedSender([(429, {"Retry-After": "2"}), 200])
    t = _drive(sender, max_retries=3)
    assert sender.calls == 2
    # The single backoff honored Retry-After exactly.
    assert t._slept == [2.0]


def test_retry_after_http_date_is_honored():
    from email.utils import formatdate

    when = formatdate(timeval=None, usegmt=True)  # ~now → ~0s
    sender = ScriptedSender([(429, {"Retry-After": when}), 200])
    t = _drive(sender, max_retries=3)
    assert sender.calls == 2
    assert len(t._slept) == 1
    assert 0.0 <= t._slept[0] <= 30.0


def test_500_retries_then_drops():
    sender = ScriptedSender([500, 500, 500, 500, 500])
    t = _drive(sender, max_retries=3)
    # 1 initial + 3 retries = 4 sends, then dropped.
    assert sender.calls == 4
    assert len(t._slept) == 3


def test_network_error_retries_then_succeeds():
    sender = ScriptedSender([ConnectionError("boom"), 200])
    t = _drive(sender, max_retries=3)
    assert sender.calls == 2


def test_408_429_are_retryable():
    for status in (408, 429):
        sender = ScriptedSender([status, 200])
        _drive(sender, max_retries=3)
        assert sender.calls == 2, status


def test_413_is_not_retried_with_the_same_body():
    # The body is exactly what the server rejected, so an identical retry can
    # only fail identically. A lone item that doesn't fit is dropped instead.
    sender = ScriptedSender([413, 200])
    _drive(sender, max_retries=3)
    assert sender.calls == 1


# -- drop set -------------------------------------------------------------


def test_400_drops_without_retry():
    sender = ScriptedSender([400])
    _drive(sender, max_retries=3)
    assert sender.calls == 1


def test_404_drops_without_retry():
    sender = ScriptedSender([404])
    _drive(sender, max_retries=3)
    assert sender.calls == 1


def test_401_disables_and_drops():
    disabled = {"v": False}
    sender = ScriptedSender([401])
    t = _transport(
        sender,
        max_retries=3,
        on_disable=lambda: disabled.__setitem__("v", True),
    )
    t.capture({"type": "event", "name": "e", "distinct_id": "u"})
    t.flush()
    assert sender.calls == 1
    assert disabled["v"] is True
    t.close(timeout=2)


def test_403_disables_and_drops():
    disabled = {"v": False}
    sender = ScriptedSender([403])
    t = _transport(
        sender,
        max_retries=3,
        on_disable=lambda: disabled.__setitem__("v", True),
    )
    t.capture({"type": "event", "name": "e", "distinct_id": "u"})
    t.flush()
    assert sender.calls == 1
    assert disabled["v"] is True
    t.close(timeout=2)


def test_backoff_is_capped_at_30s():
    # A large retry_base would explode without the cap; assert every delay
    # honors the 30s ceiling.
    sender = ScriptedSender([500, 500, 500, 200])
    t = _drive(sender, max_retries=3, retry_base=1000.0)
    assert sender.calls == 4
    assert all(d <= 30.0 for d in t._slept)


# -- envelope item cap ----------------------------------------------------
# More than 1000 items in one envelope is a non-retryable 400 server-side, and
# a rejection deletes the persisted copies — so an uncapped drain could destroy
# a whole offline backlog in a single request.


class SizeRecordingSender(ScriptedSender):
    """Records how many items each request carried."""

    def __init__(self, responses):
        super().__init__(responses)
        self.sizes = []

    def __call__(self, url, headers, body):
        import gzip
        import json

        raw = body
        if isinstance(raw, (bytes, bytearray)) and raw[:2] == b"\x1f\x8b":
            raw = gzip.decompress(raw)
        try:
            self.sizes.append(len(json.loads(raw)["items"]))
        except Exception:
            pass
        return super().__call__(url, headers, body)


def test_backlog_larger_than_cap_is_split_across_envelopes():
    from sauron import _transport as tmod

    sender = SizeRecordingSender([200])
    t = _transport(sender, max_batch=100_000, max_queue_bytes=64 * 1024 * 1024)
    for i in range(2500):
        t.capture({"type": "event", "name": "e%d" % i, "distinct_id": "u"})
    t.flush()
    t.close(timeout=5)

    assert sender.sizes, "no request was made"
    assert max(sender.sizes) <= tmod.MAX_ITEMS_PER_ENVELOPE
    assert sum(sender.sizes) == 2500
