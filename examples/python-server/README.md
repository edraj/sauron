# Sauron Python server example

A tiny, copy-pasteable server-side example that uses the [Sauron Python
SDK](../../sdks/python) (v0.3.0) to demonstrate the full server surface:
per-request scope, breadcrumbs, an event, an identify, a captured exception,
and a performance transaction.

## Run

```bash
# from this directory
pip install -e ../../sdks/python

SAURON_DSN="https://pk_live_xxx@ingest.sauron.example/1" python main.py
```

The DSN is read from the `SAURON_DSN` environment variable. Use the public key
and project id from your Sauron project's ingest DSN.

With `SAURON_DSN` **unset** the SDK runs in a disabled **no-op** mode: every
call below is a harmless no-op and the program still exits `0` — handy as a
smoke test. A non-empty but malformed DSN raises `sauron.DsnError`.

```bash
# smoke test: no DSN, exits 0
python main.py
```

## What it does

1. `sauron.init(dsn, environment="development", debug=True)` — start the client
   (or enter no-op mode when the DSN is missing).
2. `sauron.identify(...)` — attach traits to a `distinct_id`.
3. `with sauron.scope():` — a **per-request scope** whose user/tags are isolated
   to this request (concurrent requests never leak into each other):
   - `sauron.set_user({"id": ..., "email": ...})` and
     `sauron.set_tag("route", "/checkout")` — scoped context.
   - `sauron.add_breadcrumb(category="cart", message="checkout started", ...)` —
     a breadcrumb recorded **before** the error, which rides along on it.
   - `sauron.track("checkout_completed", distinct_id=..., properties={...})` — a
     product-analytics event.
   - `try/except` around a deliberate `ValueError`, reported with
     `sauron.capture_exception()` — the scoped user, tag, and breadcrumb are
     stamped onto the error item automatically.
   - `sauron.track_transaction("POST /checkout", op="http.server", duration_ms=..., http_status=200, ...)`
     — one performance transaction timing the request.
4. `sauron.flush()` then `sauron.close()` — drain the buffer and stop the
   background thread on shutdown. (`atexit` also flushes, but short-lived
   processes should shut down explicitly.)

## Requirements

The SDK is stdlib-only (no runtime dependencies), so there is nothing else to
install beyond the SDK itself. See [`requirements.txt`](./requirements.txt).
