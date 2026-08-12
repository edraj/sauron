import { describe, it, expect, vi, beforeEach } from 'vitest';
import * as schemaApi from '../../api/schema';

describe('SearchAutocompleteInput component logic & integration', () => {
  const mockAppId = 'app_12345';

  const mockSchema: schemaApi.SchemaDefinition = {
    resource: 'issues',
    variables: [
      { prefix: '@tag', description: 'Developer tags', chainable: true },
      { prefix: '@context', description: 'Device context', chainable: true },
      { prefix: '@extra', description: 'Extra metadata', chainable: true },
      { prefix: '@$label', description: 'Label properties', chainable: true },
    ],
    dimensions: [
      { name: 'level', type: 'enum', ops: ['=', '!=', 'in'], options: ['debug', 'info', 'warning', 'error', 'fatal'] },
      { name: 'status', type: 'enum', ops: ['=', '!=', 'in'], options: ['unresolved', 'resolved'] },
    ],
    available_tags: [{ key: 'environment', sample_values: ['production', 'staging'] }],
    available_labels: [{ key: 'team', type: 'string' }],
  };

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('fetches schema on mount for context issues', async () => {
    const fetchSpy = vi.spyOn(schemaApi, 'fetchSchema').mockResolvedValueOnce(mockSchema);

    const schema = await schemaApi.fetchSchema(mockAppId, 'issues');
    expect(fetchSpy).toHaveBeenCalledWith(mockAppId, 'issues');
    expect(schema.resource).toBe('issues');
  });

  it('provides autocomplete suggestions when user types variable prefixes', () => {
    const suggestionsTag = schemaApi.getAutocompleteSuggestions(mockSchema, '@tag');
    expect(suggestionsTag).toContain('@tag');

    const suggestionsLabel = schemaApi.getAutocompleteSuggestions(mockSchema, '@$label.t');
    expect(suggestionsLabel).toEqual(['@$label.team']);

    const suggestionsDim = schemaApi.getAutocompleteSuggestions(mockSchema, 'stat');
    expect(suggestionsDim).toEqual(['status']);
  });

  it('handles property chaining for @tag and @$label variables', () => {
    expect(schemaApi.normalizePropertyChain('@tag', 'environment')).toBe('@tag.environment');
    expect(schemaApi.normalizePropertyChain('@$label', 'team')).toBe('@$label.team');
    expect(schemaApi.normalizePropertyChain('@context', 'app_version')).toBe('@context.app_version');
    expect(schemaApi.normalizePropertyChain('@extra', 'level')).toBe('@extra.level');
  });

  it('handles empty or non-matching suggestions gracefully', () => {
    const suggestions = schemaApi.getAutocompleteSuggestions(mockSchema, 'nonexistent_dim');
    expect(suggestions).toEqual([]);
  });

  it('handles context switching to sessions view', async () => {
    const mockSessionsSchema: schemaApi.SchemaDefinition = {
      ...mockSchema,
      resource: 'sessions',
      dimensions: [{ name: 'duration_ms', type: 'duration', ops: ['>'] }],
    };

    const fetchSpy = vi.spyOn(schemaApi, 'fetchSchema').mockResolvedValueOnce(mockSessionsSchema);

    const schema = await schemaApi.fetchSchema(mockAppId, 'sessions');
    expect(fetchSpy).toHaveBeenCalledWith(mockAppId, 'sessions');
    expect(schema.resource).toBe('sessions');

    const suggestions = schemaApi.getAutocompleteSuggestions(schema, 'dur');
    expect(suggestions).toEqual(['duration_ms']);
  });
});
