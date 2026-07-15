"""Minimal server-side Sauron example (SDK 0.3.0 surface).

One simulated request that demonstrates:

* a **per-request scope** (``with sauron.scope():``) that sets a user + a tag,
  isolated from any other concurrent request;
* an **``add_breadcrumb``** recorded just before a deliberately-captured
  exception (the breadcrumb, scoped user, and tag ride along on the error);
* a product-analytics **event** and an **identify**;
* one **``track_transaction``** timing the whole request;
* an explicit **flush + close** before exit.

The DSN is read from the ``SAURON_DSN`` environment variable:

* set   -> events are dispatched to that ingest;
* unset -> the SDK runs in a disabled **no-op** mode: every call above is a
  harmless no-op and the program still exits 0 (a convenient smoke test).

A non-empty but malformed DSN raises :class:`sauron.DsnError`.

Run::

    pip install -e ../../sdks/python
    SAURON_DSN="https://pk_live_xxx@ingest.sauron.example/1" python main.py
"""

from __future__ import annotations

import os
import time

import sauron

DISTINCT_ID = "u_demo_1"


def handle_request() -> None:
    """Simulate one server request handled under its own isolated scope."""
    start = time.perf_counter()

    # Everything set inside this block belongs to this request only and is
    # dropped when the block exits, so concurrent requests never leak into it.
    with sauron.scope():
        sauron.set_user({"id": DISTINCT_ID, "email": "demo@example.com"})
        sauron.set_tag("route", "/checkout")

        # Breadcrumbs form a trail that attaches to the next captured error.
        sauron.add_breadcrumb(
            category="cart",
            message="checkout started",
            level="info",
            data={"cart_value": 42.5},
        )

        # Product-analytics event. distinct_id is required by the wire contract.
        sauron.track(
            "checkout_completed",
            distinct_id=DISTINCT_ID,
            properties={"cart_value": 42.5, "currency": "USD"},
        )

        # Deliberately fail and report it. The scoped user + tag and the
        # breadcrumb above are stamped onto the error item automatically.
        try:
            raise ValueError("deliberate failure for the example")
        except ValueError:
            event_id = sauron.capture_exception()
            print(f"captured exception, event id: {event_id}")

        # Time the whole request as one performance transaction.
        duration_ms = (time.perf_counter() - start) * 1000
        sauron.track_transaction(
            "POST /checkout",
            op="http.server",
            duration_ms=duration_ms,
            status="ok",
            http_method="POST",
            http_status=200,
            url="/checkout",
        )


def main() -> int:
    dsn = os.environ.get("SAURON_DSN")
    sauron.init(dsn, environment="development", debug=True)

    # Attach traits to the person behind this distinct_id (process-wide).
    sauron.identify(
        DISTINCT_ID, traits={"plan": "pro", "source": "python-example"}
    )

    handle_request()

    # Drain the buffer synchronously, then stop the background thread. (atexit
    # also flushes, but short-lived processes should shut down explicitly.)
    sauron.flush()
    sauron.close()
    print("done: flushed and closed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
