"""DSN parsing.

A DSN looks like ``https://<public_key>@<host>/<project_id>``. The public key
is a non-secret, write-only credential (it identifies the project for ingest);
it carries no password component.
"""

from __future__ import annotations

from urllib.parse import urlsplit


class DsnError(ValueError):
    """Raised when a DSN string is malformed or unusable."""

    def __init__(self, message: str) -> None:
        super().__init__(f"[sauron] invalid DSN: {message}")


class Dsn:
    """A parsed DSN plus the derived ingest endpoint.

    Attributes:
        raw: the original DSN string (embedded verbatim into the envelope header).
        public_key: the non-secret write key (travels in ``X-Sauron-Key``).
        host: ``host:port`` — includes the port when present.
        hostname: the host without a port.
        protocol: ``http`` or ``https`` (no trailing colon).
        project_id: the path segment.
        envelope_url: the ``POST`` target for the transport.
    """

    __slots__ = (
        "raw",
        "public_key",
        "host",
        "hostname",
        "protocol",
        "project_id",
        "envelope_url",
    )

    def __init__(
        self,
        raw: str,
        public_key: str,
        host: str,
        hostname: str,
        protocol: str,
        project_id: str,
    ) -> None:
        self.raw = raw
        self.public_key = public_key
        self.host = host
        self.hostname = hostname
        self.protocol = protocol
        self.project_id = project_id
        self.envelope_url = f"{protocol}://{host}/api/{project_id}/envelope"

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return (
            f"Dsn(host={self.host!r}, project_id={self.project_id!r}, "
            f"protocol={self.protocol!r})"
        )


def parse_dsn(dsn: str) -> Dsn:
    """Parse and validate a DSN, deriving the envelope URL.

    Raises:
        DsnError: if the DSN is empty, unparseable, missing its public key,
            carries a secret, or is missing the host or project id.
    """
    if not isinstance(dsn, str) or dsn == "":
        raise DsnError("DSN must be a non-empty string")

    try:
        parts = urlsplit(dsn)
    except Exception as exc:  # pragma: no cover - urlsplit rarely raises
        raise DsnError(f'could not parse "{dsn}"') from exc

    protocol = parts.scheme
    if protocol not in ("http", "https"):
        raise DsnError(f'unsupported protocol "{protocol}"')

    public_key = parts.username
    if not public_key:
        raise DsnError('missing public key (the "user" part of the URL)')

    if parts.password:
        raise DsnError("DSN must not contain a secret (password component)")

    hostname = parts.hostname
    if not hostname:
        raise DsnError("missing host")

    # ``host`` includes the port when present; rebuild it since urlsplit's
    # ``netloc`` also carries the userinfo we do not want in the URL.
    host = hostname
    if parts.port is not None:
        host = f"{hostname}:{parts.port}"

    project_id = parts.path.strip("/")
    if not project_id:
        raise DsnError("missing project id (the path segment)")

    return Dsn(
        raw=dsn,
        public_key=public_key,
        host=host,
        hostname=hostname,
        protocol=protocol,
        project_id=project_id,
    )
