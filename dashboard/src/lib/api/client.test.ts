import { describe, expect, it } from 'vitest';
import { AxiosError } from 'axios';
import { errorCode, errorMessage, normalizeError, unwrapBlobErrorBody } from './client';

function blobError(status: number, envelope: unknown): AxiosError {
  const blob = new Blob([JSON.stringify(envelope)], { type: 'application/json' });
  return new AxiosError(
    `Request failed with status code ${status}`,
    'ERR_BAD_REQUEST',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    {} as any,
    undefined,
    {
      status,
      statusText: 'Forbidden',
      data: blob,
      headers: {},
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      config: {} as any,
    },
  );
}

describe('unwrapBlobErrorBody', () => {
  it('turns a Blob-bodied 403 into the envelope its message lives in', async () => {
    const err = blobError(403, {
      error: { code: 'forbidden', message: 'no read access to app(s): abc' },
    });
    // Without the unwrap, `normalizeError` reads the Blob as an envelope, gets
    // `undefined`, and degrades to axios's generic string — which is exactly
    // the case the 403 was designed for: a user opens a shared /active-users
    // URL after a grant was revoked and the toast has to name the app.
    expect(normalizeError(err).message).toBe('Request failed with status code 403');

    await unwrapBlobErrorBody(err);
    expect(normalizeError(err).message).toBe('no read access to app(s): abc');
    expect(normalizeError(err).code).toBe('forbidden');
  });

  it('leaves a non-JSON Blob alone rather than throwing', async () => {
    const err = new AxiosError(
      'Request failed with status code 500',
      'ERR_BAD_RESPONSE',
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      {} as any,
      undefined,
      {
        status: 500,
        statusText: 'Server Error',
        data: new Blob(['day,active_total\r\n']),
        headers: {},
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        config: {} as any,
      },
    );
    await expect(unwrapBlobErrorBody(err)).resolves.toBeUndefined();
    expect(normalizeError(err).message).toBe('Request failed with status code 500');
  });

  it('is a no-op when there is no response at all', async () => {
    const err = new AxiosError('Network Error', 'ERR_NETWORK');
    await expect(unwrapBlobErrorBody(err)).resolves.toBeUndefined();
  });
});

describe('errorCode', () => {
  it("reads the envelope's code off a server answer", () => {
    const err = new AxiosError(
      'Request failed with status code 503',
      'ERR_BAD_RESPONSE',
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      {} as any,
      undefined,
      {
        status: 503,
        statusText: 'Service Unavailable',
        data: { error: { code: 'release_rollup_pending', message: 'still building' } },
        headers: {},
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        config: {} as any,
      },
    );
    // The 503 alone is an outage; the code is what says "still backfilling".
    expect(errorCode(normalizeError(err))).toBe('release_rollup_pending');
  });

  it('is null for anything that is not a normalized error', () => {
    expect(errorCode(new Error('boom'))).toBeNull();
    expect(errorCode('nope')).toBeNull();
    expect(errorCode(undefined)).toBeNull();
  });
});

function bodyError(status: number, data: unknown): AxiosError {
  return new AxiosError(
    `Request failed with status code ${status}`,
    'ERR_BAD_RESPONSE',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    {} as any,
    undefined,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    { status, statusText: '', data, headers: {}, config: {} as any },
  );
}

describe('normalizeError on a body with no envelope', () => {
  // The API's TimeoutLayer answers 503 with an EMPTY body; a reverse proxy in
  // front of a dead API answers 503 with its own HTML page. Only the first is
  // "took too long", so the two must not share a code.
  it('marks an empty body as such', () => {
    expect(normalizeError(bodyError(503, '')).code).toBe('empty_response');
    expect(normalizeError(bodyError(503, undefined)).code).toBe('empty_response');
  });

  it('keeps a non-empty body without an envelope as a plain http_error', () => {
    expect(normalizeError(bodyError(503, '<html>502 Bad Gateway</html>')).code).toBe('http_error');
  });
});

describe('errorMessage', () => {
  // What the API client rejects with: a plain object, NOT an Error. Pages that
  // tested `instanceof Error` printed it as "[object Object]".
  const apiError = { status: 422, code: 'invalid_filter', message: 'filter.level: unknown level', isNetwork: false };

  it("reads an API rejection's own message", () => {
    expect(errorMessage(apiError)).toBe('filter.level: unknown level');
    expect(errorMessage(apiError, 'Save failed')).toBe('filter.level: unknown level');
  });

  it('reads an Error', () => {
    expect(errorMessage(new Error('boom'), 'Save failed')).toBe('boom');
  });

  it("uses the caller's fallback when there is no message to show", () => {
    expect(errorMessage('weird', 'Save failed')).toBe('Save failed');
    expect(errorMessage(new Error(''), 'Save failed')).toBe('Save failed');
    expect(errorMessage({ ...apiError, message: '' }, 'Save failed')).toBe('Save failed');
  });
});
