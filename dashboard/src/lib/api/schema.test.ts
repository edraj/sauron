import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { api } from './client';
import {
  fetchSchema,
  normalizePropertyChain,
  getAutocompleteSuggestions,
  type SchemaDefinition,
} from './schema';
import { AxiosError } from 'axios';

describe('schema API client', () => {
  const mockAppId = 'app_12345';

  const mockSchemaIssues: SchemaDefinition = {
    resource: 'issues',
    variables: [
      { prefix: '@tag', description: 'Developer tags', chainable: true },
      { prefix: '@context', description: 'Device/runtime context', chainable: true },
      { prefix: '@extra', description: 'Extra metadata', chainable: true },
      { prefix: '@$label', description: 'Label properties', chainable: true },
    ],
    dimensions: [
      { name: 'level', type: 'enum', ops: ['=', '!=', 'in'], options: ['debug', 'info', 'warning', 'error', 'fatal'] },
      { name: 'status', type: 'enum', ops: ['=', '!=', 'in'], options: ['unresolved', 'resolved', 'ignored'] },
    ],
    available_tags: [{ key: 'environment', sample_values: ['production', 'staging'] }],
    available_labels: [{ key: 'team', type: 'string' }],
  };

  const mockSchemaSessions: SchemaDefinition = {
    resource: 'sessions',
    variables: [
      { prefix: '@context', description: 'Runtime context', chainable: true },
    ],
    dimensions: [
      { name: 'duration_ms', type: 'duration', ops: ['>', '>=', '<', '<='] },
      { name: 'user_id', type: 'string', ops: ['=', '!='] },
    ],
    available_tags: [],
    available_labels: [],
  };

  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe('Tier 1: Schema API Fetching', () => {
    it('fetches schema definition via GET /v1/apps/{app_id}/search/schema?context={context}', async () => {
      const getSpy = vi.spyOn(api, 'get').mockResolvedValueOnce({ data: mockSchemaIssues });

      const schema = await fetchSchema(mockAppId, 'issues');

      expect(getSpy).toHaveBeenCalledWith(`/v1/apps/${mockAppId}/search/schema`, {
        params: { context: 'issues' },
      });
      expect(schema).toEqual(mockSchemaIssues);
      expect(schema.resource).toBe('issues');
      expect(schema.dimensions).toHaveLength(2);
    });
  });

  describe('Tier 2: Boundary & Error Handling', () => {
    it('throws error when app_id is empty or missing', async () => {
      await expect(fetchSchema('', 'issues')).rejects.toThrow('app_id is required');
    });

    it('handles 404 Not Found response error from API', async () => {
      const err = new AxiosError('Request failed with status code 404', 'ERR_BAD_REQUEST', undefined, undefined, {
        status: 404,
        statusText: 'Not Found',
        data: { error: { code: 'not_found', message: 'App not found' } },
        headers: {},
        config: {} as any,
      });
      vi.spyOn(api, 'get').mockRejectedValueOnce(err);

      await expect(fetchSchema('invalid_app', 'issues')).rejects.toThrow();
    });

    it('handles 500 Internal Server Error response', async () => {
      const err = new AxiosError('Request failed with status code 500', 'ERR_BAD_RESPONSE', undefined, undefined, {
        status: 500,
        statusText: 'Internal Server Error',
        data: { error: { code: 'internal_error', message: 'Database connection failed' } },
        headers: {},
        config: {} as any,
      });
      vi.spyOn(api, 'get').mockRejectedValueOnce(err);

      await expect(fetchSchema(mockAppId, 'issues')).rejects.toThrow();
    });

    it('handles network error (no server response)', async () => {
      const netErr = new AxiosError('Network Error', 'ERR_NETWORK');
      vi.spyOn(api, 'get').mockRejectedValueOnce(netErr);

      await expect(fetchSchema(mockAppId, 'issues')).rejects.toThrow('Network Error');
    });

    it('throws error when response data is malformed JSON or empty', async () => {
      vi.spyOn(api, 'get').mockResolvedValueOnce({ data: null });

      await expect(fetchSchema(mockAppId, 'issues')).rejects.toThrow('Malformed JSON payload');
    });
  });

  describe('Tier 3: Variable Property Chaining & Autocomplete Normalization', () => {
    it('normalizes variable property chains (@$label.xxx, @tag, @context, @extra)', () => {
      expect(normalizePropertyChain('@$label', 'team')).toBe('@$label.team');
      expect(normalizePropertyChain('@tag', 'environment')).toBe('@tag.environment');
      expect(normalizePropertyChain('@context', 'os.version')).toBe('@context.os.version');
      expect(normalizePropertyChain('@extra', 'level')).toBe('@extra.level');
    });

    it('returns dynamic autocomplete suggestions for variables and property chains', () => {
      const labelSuggestions = getAutocompleteSuggestions(mockSchemaIssues, '@$label.');
      expect(labelSuggestions).toContain('@$label.team');

      const tagSuggestions = getAutocompleteSuggestions(mockSchemaIssues, '@tag.');
      expect(tagSuggestions).toContain('@tag.environment');

      const dimSuggestions = getAutocompleteSuggestions(mockSchemaIssues, 'lev');
      expect(dimSuggestions).toContain('level');
    });
  });

  describe('Tier 4: Real-World Context Switching', () => {
    it('fetches correct schema definitions across different contexts (issues, sessions, occurrences, events)', async () => {
      const getSpy = vi.spyOn(api, 'get');

      // Context 1: issues
      getSpy.mockResolvedValueOnce({ data: mockSchemaIssues });
      const issuesSchema = await fetchSchema(mockAppId, 'issues');
      expect(issuesSchema.resource).toBe('issues');

      // Context 2: sessions
      getSpy.mockResolvedValueOnce({ data: mockSchemaSessions });
      const sessionsSchema = await fetchSchema(mockAppId, 'sessions');
      expect(sessionsSchema.resource).toBe('sessions');
      expect(sessionsSchema.dimensions[0].name).toBe('duration_ms');

      // Context 3: occurrences
      const mockSchemaOccurrences = { ...mockSchemaIssues, resource: 'occurrences' };
      getSpy.mockResolvedValueOnce({ data: mockSchemaOccurrences });
      const occSchema = await fetchSchema(mockAppId, 'occurrences');
      expect(occSchema.resource).toBe('occurrences');

      // Context 4: events
      const mockSchemaEvents = { ...mockSchemaIssues, resource: 'events' };
      getSpy.mockResolvedValueOnce({ data: mockSchemaEvents });
      const eventsSchema = await fetchSchema(mockAppId, 'events');
      expect(eventsSchema.resource).toBe('events');

      expect(getSpy).toHaveBeenCalledTimes(4);
    });
  });
});
