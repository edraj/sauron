import { describe, expect, it } from 'vitest';
import { opLabel, spanStatusLabel } from './perf-ops';

describe('opLabel', () => {
  it('names the ops every SDK sends in words', () => {
    expect(opLabel('navigation')).toBe('Navigation');
    expect(opLabel('screen_load')).toBe('Screen load');
    expect(opLabel('resource')).toBe('Resource');
    expect(opLabel('custom')).toBe('Custom');
  });

  it('keeps HTTP as the protocol name in every locale', () => {
    expect(opLabel('http')).toBe('HTTP');
  });

  it("names the Performance page's All sentinel", () => {
    expect(opLabel('All')).toBe('All');
  });

  it('passes an op the SDKs never send through, spaced, as a developer wrote it', () => {
    expect(opLabel('db_query')).toBe('db query');
    expect(opLabel('constructor')).toBe('constructor');
  });
});

describe('spanStatusLabel', () => {
  it('gives the two statuses every SDK sends a word', () => {
    expect(spanStatusLabel('ok')).toBe('OK');
    expect(spanStatusLabel('error')).toBe('Error');
  });

  it("shows a developer's own status exactly as sent", () => {
    expect(spanStatusLabel('deadline_exceeded')).toBe('deadline_exceeded');
  });
});
