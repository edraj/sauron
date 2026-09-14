import { describe, it, expect, afterEach } from 'vitest';
import { Sauron } from '../src';
import { getClient } from '../src/client';

describe('init release validation', () => {
  afterEach(() => {
    getClient()?.teardown();
  });

  it('throws without a release', () => {
    expect(() => Sauron.init({ dsn: 'https://pk_test@localhost:8081/1' } as never)).toThrow(
      /requires a `release`/,
    );
  });

  it('throws on a whitespace-only release', () => {
    expect(() =>
      Sauron.init({ dsn: 'https://pk_test@localhost:8081/1', release: '   ' }),
    ).toThrow(/requires a `release`/);
  });

  it('trims the release it sends', () => {
    const client = Sauron.init({
      dsn: 'https://pk_test@localhost:8081/1',
      release: ' 1.4.2 ',
    });
    const built = client.makeEnvelope([]);
    expect(built.header.release).toBe('1.4.2');
  });
});
