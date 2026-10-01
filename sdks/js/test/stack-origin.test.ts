import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { Sauron } from '../src';
import { captureException as captureExceptionInternal } from '../src/api/capture';
import { getClient } from '../src/client';
import type { EnvelopeItem, ErrorItem } from '../src/types';

/**
 * Where an error item's stack comes from when the captured value has none:
 * the manual `captureException` call site, or nothing at all — never the
 * SDK's own frames.
 */

let items: ErrorItem[] = [];

beforeEach(() => {
  items = [];
  Sauron.init({
    dsn: 'https://pk_test@localhost:9/1',
    release: '1.0.0',
    beforeSend: (item: EnvelopeItem) => {
      if (item.type === 'error') items.push(item);
      return null;
    },
  });
});

afterEach(() => {
  getClient()?.teardown();
});

const functions = (item: ErrorItem) => item.exception!.stacktrace.map((frame) => frame.function);
const last = (item: ErrorItem) => functions(item)[functions(item).length - 1];

describe('captureException of a value with no stack', () => {
  it('records the call site for a string', () => {
    function checkoutFails() {
      Sauron.captureException('payment failed');
    }
    checkoutFails();
    expect(items[0].exception).toMatchObject({ type: 'Error', value: 'payment failed' });
    // Crash-last: the capturing function is the last frame, and no frame of
    // the SDK's own public function follows it.
    expect(last(items[0])).toBe('checkoutFails');
    expect(functions(items[0])).not.toContain('captureException');
  });

  it('records the call site for a plain object', () => {
    function loadCart() {
      Sauron.captureException({ status: 404, url: '/api/cart' });
    }
    loadCart();
    expect(last(items[0])).toBe('loadCart');
  });

  it("keeps a real error's own stack", () => {
    function throwSite() {
      return new Error('boom');
    }
    const err = throwSite();
    (function captureSite() {
      Sauron.captureException(err);
    })();
    expect(last(items[0])).toBe('throwSite');
    expect(functions(items[0])).not.toContain('captureSite');
  });

  it('records none on the internal path the handlers and GTM replay use', () => {
    captureExceptionInternal('rejected');
    expect(items[0].exception!.stacktrace).toEqual([]);
  });
});

describe('window.onerror', () => {
  it('reports a cross-origin "Script error." with no frames, not the handler\'s', () => {
    const onerror = (globalThis as { onerror?: OnErrorEventHandlerNonNull }).onerror!;
    onerror('Script error.', '', 0, 0, undefined);
    expect(items[0].exception).toMatchObject({ type: 'Error', value: 'Script error.', stacktrace: [] });
  });

  it('keeps the one frame it has when the browser gives a location', () => {
    const onerror = (globalThis as { onerror?: OnErrorEventHandlerNonNull }).onerror!;
    onerror('Uncaught boom', 'https://shop.example/app.js', 3, 7, undefined);
    expect(items[0].exception!.stacktrace).toEqual([
      { function: null, filename: 'https://shop.example/app.js', lineno: 3, colno: 7, in_app: false },
    ]);
  });
});
