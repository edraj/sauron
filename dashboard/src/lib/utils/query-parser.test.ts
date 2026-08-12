import { describe, it, expect } from 'vitest';
import { parseQuery, QueryParseError, type QueryNode } from './query-parser';

describe('query-parser', () => {
  describe('Tier 1: Feature Coverage (AST Node & Operator Parsing)', () => {
    it('parses equality operator (= and :) into Pred nodes', () => {
      const ast1 = parseQuery('status=unresolved');
      expect(ast1).toEqual({
        Pred: { field: 'status', value: 'unresolved', quoted: false, at: 0 },
      });

      const ast2 = parseQuery('level:error');
      expect(ast2).toEqual({
        Pred: { field: 'level', value: 'error', quoted: false, at: 0 },
      });
    });

    it('parses inequality operator (!=) into Not(Pred) nodes', () => {
      const ast = parseQuery('level!=error');
      expect(ast).toEqual({
        Not: {
          Pred: { field: 'level', value: 'error', quoted: false, at: 0 },
        },
      });
    });

    it('parses comparison operators (>, >=, <, <=) into Pred nodes with prefixed values', () => {
      expect(parseQuery('times_seen>10')).toEqual({
        Pred: { field: 'times_seen', value: '>10', quoted: false, at: 0 },
      });
      expect(parseQuery('times_seen>=10')).toEqual({
        Pred: { field: 'times_seen', value: '>=10', quoted: false, at: 0 },
      });
      expect(parseQuery('times_seen<50')).toEqual({
        Pred: { field: 'times_seen', value: '<50', quoted: false, at: 0 },
      });
      expect(parseQuery('times_seen<=50')).toEqual({
        Pred: { field: 'times_seen', value: '<=50', quoted: false, at: 0 },
      });
    });

    it('parses in, has, like, and contains operators', () => {
      // in operator
      expect(parseQuery('level:[error,fatal]')).toEqual({
        Pred: { field: 'level', value: '[error,fatal]', quoted: false, at: 0 },
      });

      // has operator
      expect(parseQuery('has:tag')).toEqual({
        Pred: { field: 'has', value: 'tag', quoted: false, at: 0 },
      });

      // like operator (wildcard)
      expect(parseQuery('@tag=v*')).toEqual({
        Pred: { field: '@tag', value: 'v*', quoted: false, at: 0 },
      });

      // contains operator
      expect(parseQuery('contains:handler')).toEqual({
        Pred: { field: 'contains', value: 'handler', quoted: false, at: 0 },
      });
    });

    it('parses free text into Text nodes and implicit AND groups', () => {
      expect(parseQuery('timeout')).toEqual({
        Text: 'timeout',
      });

      expect(parseQuery('level:error timeout')).toEqual({
        And: [
          { Pred: { field: 'level', value: 'error', quoted: false, at: 0 } },
          { Text: 'timeout' },
        ],
      });
    });
  });

  describe('Tier 2: Boundary & Corner Cases', () => {
    it('handles empty query string and whitespace-only queries by returning empty And', () => {
      expect(parseQuery('')).toEqual({ And: [] });
      expect(parseQuery('   ')).toEqual({ And: [] });
    });

    it('handles double negation (!! or NOT NOT)', () => {
      const ast = parseQuery('!!status=unresolved');
      expect(ast).toEqual({
        Not: {
          Not: {
            Pred: { field: 'status', value: 'unresolved', quoted: false, at: 0 },
          },
        },
      });
    });

    it('throws QueryParseError on unclosed quotes', () => {
      expect(() => parseQuery('"unclosed text')).toThrow(QueryParseError);
    });

    it('throws QueryParseError on mismatched opening parentheses', () => {
      expect(() => parseQuery('(status=unresolved')).toThrow(QueryParseError);
    });

    it('throws QueryParseError on mismatched closing parentheses', () => {
      expect(() => parseQuery('status=unresolved)')).toThrow(QueryParseError);
    });

    it('throws QueryParseError on dangling AND/OR/NOT keywords', () => {
      expect(() => parseQuery('status=unresolved AND')).toThrow(QueryParseError);
      expect(() => parseQuery('status=unresolved OR')).toThrow(QueryParseError);
      expect(() => parseQuery('NOT')).toThrow(QueryParseError);
    });
  });

  describe('Tier 3: Cross-Feature Logic with Variables', () => {
    it('parses combined boolean logic with @tag and @context.app_version', () => {
      const query = '@tag=v1 and @context.app_version=3.0.2';
      const ast = parseQuery(query);

      expect(ast).toEqual({
        And: [
          { Pred: { field: '@tag', value: 'v1', quoted: false, at: 0 } },
          { Pred: { field: '@context.app_version', value: '3.0.2', quoted: false, at: 13 } },
        ],
      });
    });

    it('parses variable property chaining (@$label.xxx, @extra.level, @context.os)', () => {
      const query = '@$label.team=frontend AND @extra.level=warn';
      const ast = parseQuery(query);

      expect(ast).toEqual({
        And: [
          { Pred: { field: '@$label.team', value: 'frontend', quoted: false, at: 0 } },
          { Pred: { field: '@extra.level', value: 'warn', quoted: false, at: 26 } },
        ],
      });
    });
  });

  describe('Tier 4: Real-World Application Scenarios (Complex Nested Expressions)', () => {
    it('correctly parses complex nested query: ((@tag=v1 and @context.app_version=3.0.2) or (@extra.level=warn))', () => {
      const query = '((@tag=v1 and @context.app_version=3.0.2) or (@extra.level=warn))';
      const ast = parseQuery(query);

      expect(ast).toEqual({
        Or: [
          {
            And: [
              { Pred: { field: '@tag', value: 'v1', quoted: false, at: 2 } },
              { Pred: { field: '@context.app_version', value: '3.0.2', quoted: false, at: 15 } },
            ],
          },
          { Pred: { field: '@extra.level', value: 'warn', quoted: false, at: 47 } },
        ],
      });
    });

    it('preserves operator precedence where AND binds tighter than OR', () => {
      const query = '@tag=v1 AND @context.env=prod OR @extra.level=error';
      const ast = parseQuery(query);

      expect(ast).toEqual({
        Or: [
          {
            And: [
              { Pred: { field: '@tag', value: 'v1', quoted: false, at: 0 } },
              { Pred: { field: '@context.env', value: 'prod', quoted: false, at: 12 } },
            ],
          },
          { Pred: { field: '@extra.level', value: 'error', quoted: false, at: 33 } },
        ],
      });
    });
  });
});
