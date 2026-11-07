import { describe, expect, it } from 'vitest';
import { describeLoadError, loadErrorText } from './load-error';

const norm = (status: number, code = 'http_error', message = 'boom', isNetwork = false) => ({
  status,
  code,
  message,
  isNetwork,
});

describe('describeLoadError', () => {
  it('a dropped connection reads as a network problem', () => {
    expect(describeLoadError(norm(0, 'ERR_NETWORK', 'Network Error', true)).reason).toBe(
      'common.loadError.network',
    );
  });

  it('a 504 or an aborted request reads as a timeout', () => {
    expect(describeLoadError(norm(504)).reason).toBe('common.loadError.timeout');
    expect(describeLoadError(norm(0, 'ECONNABORTED', 'timeout of 60000ms exceeded', true)).reason).toBe(
      'common.loadError.timeout',
    );
  });

  it('other 5xx read as a server problem', () => {
    for (const s of [500, 502]) {
      expect(describeLoadError(norm(s)).reason).toBe('common.loadError.server');
    }
  });

  it('429, 403 and 404 each get their own sentence', () => {
    expect(describeLoadError(norm(429)).reason).toBe('common.loadError.rateLimited');
    expect(describeLoadError(norm(403)).reason).toBe('common.loadError.forbidden');
    expect(describeLoadError(norm(404)).reason).toBe('common.loadError.notFound');
  });

  it('a 400 keeps the backend text: it says what was wrong with the request', () => {
    const d = describeLoadError(norm(400, 'invalid_query', 'unknown field `foo`'));
    expect(d.reason).toBeNull();
    expect(d.details).toBe('unknown field `foo`');
  });

  it('a coded 503 the pages special-case keeps its text', () => {
    expect(describeLoadError(norm(503, 'release_rollup_pending', 'still backfilling')).reason).toBeNull();
  });

  it('a thrown Error reads as a server problem, with its message as details', () => {
    const d = describeLoadError(new Error('x'));
    expect(d.reason).toBe('common.loadError.server');
    expect(d.details).toBe('x');
  });

  it("reads a CachedView's error / errorStatus / errorCode triple", () => {
    expect(describeLoadError({ error: 'Gateway Timeout', errorStatus: 504, errorCode: null }).reason).toBe(
      'common.loadError.timeout',
    );
    expect(describeLoadError({ error: 'Network Error', errorStatus: null, errorCode: 'ERR_NETWORK' }).reason).toBe(
      'common.loadError.network',
    );
  });

  it("the backend's own query_timeout 503 reads as a timeout, not a server fault", () => {
    expect(describeLoadError(norm(503, 'query_timeout', 'query exceeded its budget')).reason).toBe(
      'common.loadError.timeout',
    );
    expect(describeLoadError({ error: 'query exceeded', errorStatus: 503, errorCode: 'query_timeout' }).reason).toBe(
      'common.loadError.timeout',
    );
  });

  // tower-http's TimeoutLayer (main.rs) answers SERVICE_UNAVAILABLE with an
  // empty body; every handler-made 503 carries a coded envelope. No envelope
  // (normalizeError's `http_error`) on a 503 is the request budget running out.
  it('a bare 503 with an empty body is the request timeout', () => {
    expect(describeLoadError(norm(503, 'empty_response', 'Request failed with status code 503')).reason).toBe(
      'common.loadError.timeout',
    );
    expect(describeLoadError({ error: 'x', errorStatus: 503, errorCode: 'empty_response' }).reason).toBe(
      'common.loadError.timeout',
    );
  });

  // A reverse proxy in front of a dead API sends its own 503 page: that is an
  // outage, not a slow query, and must not be told to narrow the window.
  it("a proxy's 503 page (a body, but no envelope) is a server problem", () => {
    expect(describeLoadError(norm(503, 'http_error', 'Request failed with status code 503')).reason).toBe(
      'common.loadError.server',
    );
  });

  it('a coded 503 that is not a timeout stays a server problem', () => {
    expect(describeLoadError(norm(503, 'db_unavailable', 'database unavailable')).reason).toBe(
      'common.loadError.server',
    );
  });

  it("a CachedView whose loader threw a plain JS error is not a network failure", () => {
    expect(describeLoadError({ error: 'x is not a function', errorStatus: null, errorCode: 'error' }).reason).toBe(
      'common.loadError.server',
    );
  });

  it('a bare string has no status to classify, so it is shown as-is', () => {
    const d = describeLoadError('Something specific broke');
    expect(d.reason).toBeNull();
    expect(d.details).toBe('Something specific broke');
  });
});

describe('loadErrorText', () => {
  it('is the human sentence when there is one', () => {
    expect(loadErrorText(norm(504))).toBe('The server took too long to answer.');
  });

  it('is the raw text when the failure keeps its own words', () => {
    expect(loadErrorText(norm(400, 'invalid_query', 'unknown field `foo`'))).toBe('unknown field `foo`');
  });
});
