/**
 * Schema API client for context-aware search autocomplete.
 * GET /v1/apps/{app_id}/search/schema?context={context}
 */

import { api } from './client';

export interface VariableDef {
  prefix: string;
  description: string;
  chainable: boolean;
}

export interface DimensionDef {
  name: string;
  type: 'string' | 'number' | 'boolean' | 'enum' | 'duration' | 'timestamp';
  ops: string[];
  options?: string[];
}

export interface TagOption {
  key: string;
  sample_values?: string[];
}

export interface LabelOption {
  key: string;
  type: string;
}

export interface SchemaDefinition {
  resource: string;
  variables: VariableDef[];
  dimensions: DimensionDef[];
  available_tags: TagOption[];
  available_labels: LabelOption[];
}

export type SearchContext = 'issues' | 'sessions' | 'occurrences' | 'events';

/**
 * Fetch schema definition for a given app_id and context.
 */
export async function fetchSchema(appId: string, context: SearchContext | string): Promise<SchemaDefinition> {
  if (!appId) {
    throw new Error('app_id is required to fetch search schema');
  }
  const response = await api.get<SchemaDefinition>(`/v1/apps/${encodeURIComponent(appId)}/search/schema`, {
    params: { context },
  });

  if (!response.data || typeof response.data !== 'object') {
    throw new Error('Malformed JSON payload received for search schema');
  }

  return response.data;
}

/**
 * Normalize and expand variable property chaining (e.g. `@$label.team`, `@context.app_version`, `@extra.level`, `@tag.env`).
 */
export function normalizePropertyChain(prefix: string, property: string): string {
  const cleanPrefix = prefix.startsWith('@') ? prefix : `@${prefix}`;
  return property ? `${cleanPrefix}.${property}` : cleanPrefix;
}

/**
 * Generates autocomplete suggestions based on current input and schema context.
 */
export function getAutocompleteSuggestions(schema: SchemaDefinition, input: string): string[] {
  const suggestions: string[] = [];
  const trimmed = input.trim();

  // If input starts with `@$label.`
  if (trimmed.startsWith('@$label.')) {
    const prop = trimmed.slice(8);
    for (const label of schema.available_labels || []) {
      if (label.key.startsWith(prop)) {
        suggestions.push(`@$label.${label.key}`);
      }
    }
    return suggestions;
  }

  // If input starts with `@tag.` or `@tag=` or `@tag`
  if (trimmed.startsWith('@tag.') || trimmed.startsWith('@tag')) {
    const prop = trimmed.startsWith('@tag.') ? trimmed.slice(5) : '';
    for (const tag of schema.available_tags || []) {
      if (tag.key.startsWith(prop)) {
        suggestions.push(`@tag.${tag.key}`);
      }
    }
    return suggestions;
  }

  // Variable prefixes
  for (const v of schema.variables || []) {
    if (v.prefix.startsWith(trimmed) || trimmed.startsWith(v.prefix)) {
      suggestions.push(v.prefix);
    }
  }

  // Dimensions
  for (const d of schema.dimensions || []) {
    if (d.name.startsWith(trimmed)) {
      suggestions.push(d.name);
    }
  }

  return suggestions;
}
