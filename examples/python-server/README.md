# Sauron Python server example

A tiny, copy-pasteable server-side example that uses the [Sauron Python
SDK](../../sdks/python) to identify a user, track an event, and capture an
exception.

## Run

```bash
# from this directory
pip install -e ../../sdks/python

SAURON_DSN="https://pk_live_xxx@ingest.sauron.example/1" python main.py
```

The DSN is read from the `SAURON_DSN` environment variable. Use the public key
and project id from your Sauron project's ingest DSN.

## What it does

1. `sauron.init(dsn, environment="development", debug=True)` — start the client.
2. `sauron.identify(...)` — attach traits to a `distinct_id`.
3. `sauron.track("checkout_completed", distinct_id=..., properties={...})` — send
   a product-analytics event.
4. `try/except` around a deliberate `ValueError`, reported with
   `sauron.capture_exception(...)`.
5. `sauron.flush()` then `sauron.close()` — drain the buffer and stop the
   background thread on shutdown.

## Requirements

The SDK is stdlib-only (no runtime dependencies), so there is nothing else to
install beyond the SDK itself. See [`requirements.txt`](./requirements.txt).
