import { describe, expect, it } from 'vitest';
import { labelFor, type MessageKey } from './index';

const MAP: Record<string, MessageKey> = {
  resolved: 'status.resolved',
  ignored: 'status.ignored',
};

describe('labelFor', () => {
  it('translates a value the map knows', () => {
    expect(labelFor(MAP, 'resolved')).toBe('Resolved');
  });

  it('shows a value the map does not know exactly as the server sent it', () => {
    expect(labelFor(MAP, 'snoozed')).toBe('snoozed');
  });

  // A wire value is untrusted input: `constructor` must not resolve to
  // Object.prototype.constructor and render "function Object() …".
  it('never resolves through Object.prototype', () => {
    expect(labelFor(MAP, 'constructor')).toBe('constructor');
    expect(labelFor(MAP, 'toString')).toBe('toString');
    expect(labelFor(MAP, '__proto__')).toBe('__proto__');
  });
});
