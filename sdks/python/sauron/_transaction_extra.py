"""The size cap on a transaction's developer-supplied ``extra``.

Its own module rather than a private helper inside ``_client.py`` so the limit
and its behaviour are directly testable — the failure this guards against (an
oversized payload taking a whole batched envelope down with it) is not visible
from the outside once it happens.
"""

from __future__ import annotations

import json
from typing import Any, Dict

#: Largest serialized ``extra`` a single transaction may carry, in bytes.
#:
#: Transactions are the highest-volume signal and they ship in BATCHED
#: envelopes, so one oversized payload does not fail alone — ingest rejects the
#: whole envelope past ``INGEST_MAX_BODY_BYTES`` (1 MiB by default) and every
#: unrelated span batched with it is lost. Since the motivating use of
#: transaction ``extra`` is request and response bodies, that is not a remote
#: hazard.
#:
#: Kept identical across all five SDKs. If it moves, it moves everywhere.
MAX_TRANSACTION_EXTRA_BYTES = 16 * 1024


def cap_transaction_extra(
    extra: Dict[str, Any],
    max_bytes: int = MAX_TRANSACTION_EXTRA_BYTES,
) -> Dict[str, Any]:
    """Cap a transaction's ``extra``, substituting a marker when too large.

    Replaces the WHOLE map rather than trimming keys: a half-written JSON value
    is worse than an honest marker, and per-key trimming would make the result
    depend on key iteration order, which differs across the five SDKs. The
    marker is deliberately readable on the dashboard — ``_truncated`` says data
    was dropped rather than silently serving a short object that looks
    complete.

    A value that cannot be serialized at all (a cycle, a custom object) becomes
    the same marker with ``_bytes: -1``, because the alternative is raising
    from inside ``track_transaction`` — and an SDK that crashes the app it is
    measuring is worse than one that drops a payload.
    """
    try:
        # ``default=str`` is deliberately NOT used: it would make an
        # unserializable payload silently succeed here and then fail at the
        # real encode in ``_transport``, which is the one place a failure is
        # invisible. Measure with the same strictness the wire will apply.
        encoded = json.dumps(extra, separators=(",", ":"))
    except (TypeError, ValueError, RecursionError):
        return {"_truncated": True, "_bytes": -1}
    # UTF-8 byte length, not ``len(encoded)``: the latter counts characters,
    # undercounting every non-ASCII byte — which is exactly what a response
    # body full of user text is made of.
    size = len(encoded.encode("utf-8"))
    if size <= max_bytes:
        return extra
    return {"_truncated": True, "_bytes": size}
