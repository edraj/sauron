import { describe, it, expect } from 'vitest';

import { parseDsn, DsnError } from '../src/dsn.js';

describe('parseDsn', () => {
  it('parses a valid https DSN and derives the envelope URL', () => {
    const dsn = parseDsn('https://pub_key_123@ingest.sauron.dev/42');
    expect(dsn.publicKey).toBe('pub_key_123');
    expect(dsn.host).toBe('ingest.sauron.dev');
    expect(dsn.hostname).toBe('ingest.sauron.dev');
    expect(dsn.protocol).toBe('https');
    expect(dsn.projectId).toBe('42');
    expect(dsn.envelopeUrl).toBe('https://ingest.sauron.dev/api/42/envelope');
    expect(dsn.raw).toBe('https://pub_key_123@ingest.sauron.dev/42');
  });

  it('keeps the port in host and the endpoint', () => {
    const dsn = parseDsn('http://key@localhost:8080/7');
    expect(dsn.protocol).toBe('http');
    expect(dsn.host).toBe('localhost:8080');
    expect(dsn.hostname).toBe('localhost');
    expect(dsn.envelopeUrl).toBe('http://localhost:8080/api/7/envelope');
  });

  it('rejects an empty string', () => {
    expect(() => parseDsn('')).toThrow(DsnError);
  });

  it('rejects an unparseable value', () => {
    expect(() => parseDsn('not a url')).toThrow(DsnError);
  });

  it('rejects an unsupported protocol', () => {
    expect(() => parseDsn('ftp://key@host/1')).toThrow(/unsupported protocol/);
  });

  it('rejects a missing public key', () => {
    expect(() => parseDsn('https://ingest.sauron.dev/42')).toThrow(/missing public key/);
  });

  it('rejects a DSN carrying a secret (password component)', () => {
    expect(() => parseDsn('https://key:secret@host/1')).toThrow(/must not contain a secret/);
  });

  it('rejects a missing project id', () => {
    expect(() => parseDsn('https://key@host')).toThrow(/missing project id/);
  });
});
