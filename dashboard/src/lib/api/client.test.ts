import { describe, expect, it } from 'vitest';
import { AxiosError } from 'axios';
import { normalizeError, unwrapBlobErrorBody } from './client';

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
