"""Minimal server-side Sauron example.

Reads the DSN from the SAURON_DSN environment variable, identifies a user,
tracks an event, captures a deliberate exception, then flushes and closes.

Run:

    pip install -e ../../sdks/python
    SAURON_DSN="https://pk_live_xxx@ingest.sauron.example/1" python main.py
"""

from __future__ import annotations

import os
import sys

import sauron

DISTINCT_ID = "u_demo_1"


def main() -> int:
    dsn = os.environ.get("SAURON_DSN")
    if not dsn:
        print(
            "SAURON_DSN is not set. Export it, e.g.:\n"
            '  SAURON_DSN="https://pk_live_xxx@ingest.sauron.example/1" python main.py',
            file=sys.stderr,
        )
        return 1

    # A missing/empty DSN would disable the SDK (no-op mode); a malformed
    # non-empty DSN raises sauron.DsnError.
    sauron.init(dsn, environment="development", debug=True)

    # Attach traits to the person behind this distinct_id.
    sauron.identify(DISTINCT_ID, traits={"plan": "pro", "source": "python-example"})

    # Product-analytics event. distinct_id is required by the wire contract.
    sauron.track(
        "checkout_completed",
        distinct_id=DISTINCT_ID,
        properties={"cart_value": 42.5, "currency": "USD"},
    )

    # Deliberately trigger an error and report it.
    try:
        raise ValueError("deliberate failure for the example")
    except ValueError:
        event_id = sauron.capture_exception(
            tags={"module": "python-example"},
        )
        print(f"captured exception, event id: {event_id}")

    # Drain the buffer synchronously, then stop the background thread.
    sauron.flush()
    sauron.close()
    print("done: flushed and closed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
