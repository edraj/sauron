/**
 * DSN parsing.
 *
 * A DSN looks like `https://<public_key>@<host>/<project_id>`. The public key
 * is a non-secret, write-only credential.
 */

export interface Dsn {
  /** The raw DSN string (embedded verbatim into the envelope header). */
  raw: string;
  publicKey: string;
  /** `host:port` — used to build the endpoint. */
  host: string;
  /** Hostname without port. */
  hostname: string;
  /** `https` or `http` (no trailing colon). */
  protocol: string;
  projectId: string;
  /** `POST` target: `{protocol}://{host}/api/{project_id}/envelope`. */
  envelopeUrl: string;
}

export class DsnError extends Error {
  constructor(message: string) {
    super(`[sauron] invalid DSN: ${message}`);
    this.name = 'DsnError';
  }
}

/** Parse and validate a DSN, deriving the transport URL. */
export function parseDsn(dsn: string): Dsn {
  if (typeof dsn !== 'string' || dsn.length === 0) {
    throw new DsnError('DSN must be a non-empty string');
  }

  let url: URL;
  try {
    url = new URL(dsn);
  } catch {
    throw new DsnError(`could not parse "${dsn}"`);
  }

  const protocol = url.protocol.replace(/:$/, '');
  if (protocol !== 'http' && protocol !== 'https') {
    throw new DsnError(`unsupported protocol "${protocol}"`);
  }

  const publicKey = url.username;
  if (!publicKey) {
    throw new DsnError('missing public key (the "user" part of the URL)');
  }
  if (url.password) {
    throw new DsnError('DSN must not contain a secret (password component)');
  }

  const host = url.host; // includes port when present
  const hostname = url.hostname;
  if (!host) {
    throw new DsnError('missing host');
  }

  const projectId = url.pathname.replace(/^\/+/, '').replace(/\/+$/, '');
  if (!projectId) {
    throw new DsnError('missing project id (the path segment)');
  }

  const envelopeUrl = `${protocol}://${host}/api/${projectId}/envelope`;

  return {
    raw: dsn,
    publicKey,
    host,
    hostname,
    protocol,
    projectId,
    envelopeUrl,
  };
}
