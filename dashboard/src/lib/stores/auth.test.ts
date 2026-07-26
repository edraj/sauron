import { describe, expect, it } from 'vitest';
import { isPasswordChangeRequired } from './auth.svelte';

describe('isPasswordChangeRequired', () => {
  it('is true for a normalized 403 password_change_required error', () => {
    expect(
      isPasswordChangeRequired({
        status: 403,
        code: 'password_change_required',
        message: '',
        isNetwork: false,
      }),
    ).toBe(true);
  });

  it('is false when the code does not match, even at 403', () => {
    expect(
      isPasswordChangeRequired({
        status: 403,
        code: 'http_error',
        message: '',
        isNetwork: false,
      }),
    ).toBe(false);
  });

  it('is false when the code matches but the status is not 403', () => {
    expect(
      isPasswordChangeRequired({
        status: 401,
        code: 'password_change_required',
        message: '',
        isNetwork: false,
      }),
    ).toBe(false);
  });

  it('is false for a plain Error', () => {
    expect(isPasswordChangeRequired(new Error('x'))).toBe(false);
  });

  it('is false for null, undefined, and empty object', () => {
    expect(isPasswordChangeRequired(null)).toBe(false);
    expect(isPasswordChangeRequired(undefined)).toBe(false);
    expect(isPasswordChangeRequired({})).toBe(false);
  });
});
