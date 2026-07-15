"""Optional gzip compression for the request body.

The ingest accepts ``Content-Encoding: gzip`` (see the wire contract's module
docs). Compress only when the body is large enough that the CPU/size trade-off
pays off — small payloads are sent verbatim so the common single-item flush
stays cheap.
"""

from __future__ import annotations

import gzip
from typing import Dict, Tuple

# Match the js/flutter SDKs: only compress bodies larger than this many bytes.
DEFAULT_GZIP_THRESHOLD_BYTES = 1024


def maybe_gzip(body: bytes, threshold: int) -> Tuple[bytes, Dict[str, str]]:
    """Gzip ``body`` when it exceeds ``threshold`` bytes.

    Returns ``(body, headers)``. Above the threshold the returned body is the
    gzip-compressed payload and ``headers`` carries ``Content-Encoding: gzip``;
    at or below the threshold the body is returned unchanged with no extra
    headers.
    """
    if len(body) > threshold:
        return gzip.compress(body), {"Content-Encoding": "gzip"}
    return body, {}
