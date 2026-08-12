import { describe, it, expect } from 'vitest';
import {
  getAutocompleteSuggestions,
  placeholderFor,
  didYouMean,
  type SchemaDefinition,
} from './schema';

const schema: SchemaDefinition = {
  resource: 'issues',
  variables: [{ prefix: '@tag', description: 'Developer tags', chainable: true }],
  dimensions: [
    { name: 'level', type: 'enum', ops: ['=', '!=', 'in'], options: ['warning', 'error', 'fatal'] },
    { name: 'status', type: 'enum', ops: ['=', '!='], options: ['unresolved', 'resolved'] },
    { name: 'timesSeen', type: 'integer', ops: ['=', '>', '<'] },
  ],
  available_tags: [{ key: 'region', sample_values: ['eu', 'us'] }],
  available_labels: [{ key: 'team', type: 'string' }],
};

describe('getAutocompleteSuggestions', () => {
  it('completes a field WITH its colon so the parser reads a predicate', () => {
    const s = getAutocompleteSuggestions(schema, 'lev');
    expect(s).toHaveLength(1);
    // The whole point: `level ` would lex as free text.
    expect(s[0].insert).toBe('level:');
    expect(s[0].kind).toBe('field');
    expect(s[0].detail).toBe('enum');
  });

  it('offers enum values once the colon is typed', () => {
    const s = getAutocompleteSuggestions(schema, 'level:');
    expect(s.map((x) => x.insert)).toEqual(['level:warning', 'level:error', 'level:fatal']);
    expect(s.every((x) => x.kind === 'value')).toBe(true);
  });

  it('narrows enum values by the partial value already typed', () => {
    const s = getAutocompleteSuggestions(schema, 'level:f');
    expect(s.map((x) => x.insert)).toEqual(['level:fatal']);
  });

  it('offers nothing for a field with no options, rather than a wrong guess', () => {
    expect(getAutocompleteSuggestions(schema, 'timesSeen:')).toEqual([]);
  });

  it('offers real tag keys after @tag., and keeps bare @tag as its own field', () => {
    expect(getAutocompleteSuggestions(schema, '@tag').map((x) => x.insert)).toContain('@tag');
    const keys = getAutocompleteSuggestions(schema, '@tag.re');
    expect(keys.map((x) => x.insert)).toEqual(['@tag.region:']);
    expect(keys[0].kind).toBe('tagKey');
  });

  it('matches a dimension by alias as well as by name', () => {
    const aliased: SchemaDefinition = {
      ...schema,
      dimensions: [{ name: 'timesSeen', type: 'integer', ops: ['='], aliases: ['count'] }],
    };
    expect(getAutocompleteSuggestions(aliased, 'cou').map((x) => x.insert)).toEqual(['timesSeen:']);
  });

  it('returns nothing for an unmatched token', () => {
    expect(getAutocompleteSuggestions(schema, 'nonexistent')).toEqual([]);
  });
});

describe('placeholderFor', () => {
  it('builds an example from what THIS resource actually declares', () => {
    // Finding C: SessionsList hand-wrote `@tag=v1`, which sessions withhold.
    expect(placeholderFor(schema)).toContain('level:');
    expect(placeholderFor(schema)).toContain('@tag');
  });

  it('never advertises a variable the resource does not declare', () => {
    const sessions: SchemaDefinition = {
      ...schema,
      resource: 'sessions',
      variables: [{ prefix: '@context', description: 'Device context', chainable: true }],
      available_tags: [],
    };
    expect(placeholderFor(sessions)).not.toContain('@tag');
    expect(placeholderFor(sessions)).toContain('@context');
  });

  it('falls back to plain copy before the schema loads', () => {
    expect(placeholderFor(null)).toBe('Search…');
  });
});

describe('didYouMean', () => {
  it('suggests the nearest known field for a typo', () => {
    expect(didYouMean(schema, 'levl')).toBe('level');
    expect(didYouMean(schema, 'staus')).toBe('status');
  });

  it('stays silent when nothing is close, rather than guessing', () => {
    expect(didYouMean(schema, 'zzzzzzzz')).toBeNull();
  });

  it('is silent with no schema in hand', () => {
    expect(didYouMean(null, 'levl')).toBeNull();
  });
});
