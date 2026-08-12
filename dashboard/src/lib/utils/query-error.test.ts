import { describe, it, expect } from 'vitest';
import { queryErrorFor, preflight } from './query-error';
import type { SchemaDefinition } from '../api/schema';

const schema: SchemaDefinition = {
  resource: 'issues',
  variables: [],
  dimensions: [{ name: 'level', type: 'enum', ops: ['='], options: ['error'] }],
  available_tags: [],
  available_labels: [],
};

describe('queryErrorFor', () => {
  it('is silent when nothing failed', () => {
    expect(queryErrorFor(null, null, schema)).toBeNull();
  });

  it('surfaces a 400 verbatim and appends a suggestion', () => {
    const msg = queryErrorFor(400, 'unknown field `levl`', schema);
    expect(msg).toContain('unknown field `levl`');
    expect(msg).toContain('did you mean `level`');
  });

  it('surfaces a 400 with no suggestion when nothing is close', () => {
    const msg = queryErrorFor(400, 'unknown field `zzzzzzzz`', schema);
    expect(msg).toBe('unknown field `zzzzzzzz`');
  });

  it('passes a 403 through unchanged — the backend names the permission', () => {
    const back = 'filtering by tag requires event:read';
    expect(queryErrorFor(403, back, schema)).toBe(back);
  });

  it('ignores failures that are not about the query', () => {
    // A 500 or a network drop is the page error card's job, not the input's.
    // Marking the box invalid would send the reader to edit a fine query.
    expect(queryErrorFor(500, 'internal error', schema)).toBeNull();
    expect(queryErrorFor(0, 'Network Error', schema)).toBeNull();
  });
});

describe('preflight', () => {
  it('passes a well-formed query', () => {
    expect(preflight('level:error (a OR b)')).toBeNull();
    expect(preflight('')).toBeNull();
  });

  it('catches an unbalanced paren before a request is issued', () => {
    expect(preflight('(level:error')).toContain('parenthes');
  });

  it('catches a closing paren that opens nothing', () => {
    expect(preflight('level:error)')).toContain('parenthes');
  });

  it('catches a dangling boolean operator', () => {
    expect(preflight('level:error OR')).toContain('OR');
  });

  it('ignores parens INSIDE a quoted value', () => {
    // A `)` anywhere in a value has broken this grammar's round trip before,
    // so the preflight must not invent an error where the lexer sees none.
    expect(preflight('title:"boom (fatal)"')).toBeNull();
  });
});
