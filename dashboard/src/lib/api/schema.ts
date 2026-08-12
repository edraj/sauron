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
  type: 'string' | 'number' | 'boolean' | 'enum' | 'duration' | 'timestamp' | 'integer';
  ops: string[];
  options?: string[];
  /** Alternate spellings the resolver accepts; the backend already sends these. */
  aliases?: string[];
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
 * One row of the autocomplete dropdown.
 *
 * `insert` is what replaces the current token, and it is deliberately NOT the
 * same as `label`: completing a field has to carry its own `:` or the token
 * lands as free text. That was the defect — picking `level` inserted `level `,
 * which lexes as a payload search for the literal word "level".
 */
export interface Suggestion {
  insert: string;
  label: string;
  detail?: string;
  kind: 'field' | 'value' | 'variable' | 'tagKey';
}

/** Split a token at its FIRST separator. `@tag.k:v` → field `@tag.k`, value `v`. */
function splitToken(token: string): { field: string; value: string | null } {
  const i = token.indexOf(':');
  if (i < 0) return { field: token, value: null };
  return { field: token.slice(0, i), value: token.slice(i + 1) };
}

function dimensionMatches(d: DimensionDef, prefix: string): boolean {
  if (d.name.startsWith(prefix)) return true;
  return (d.aliases ?? []).some((a) => a.startsWith(prefix));
}

/**
 * Generates autocomplete suggestions based on current input and schema context.
 */
export function getAutocompleteSuggestions(
  schema: SchemaDefinition,
  input: string,
): Suggestion[] {
  const token = input.trim();
  if (!token) return [];
  const { field, value } = splitToken(token);

  // --- a colon is already typed: complete the VALUE ------------------------
  if (value !== null) {
    const dim = (schema.dimensions ?? []).find(
      (d) => d.name === field || (d.aliases ?? []).includes(field),
    );
    // No options means we do not know this field's values. Offering a guess
    // would be worse than offering nothing — the user would insert it.
    if (!dim?.options) return [];
    return dim.options
      .filter((o) => o.startsWith(value))
      .map((o) => ({
        insert: `${dim.name}:${o}`,
        label: o,
        detail: dim.name,
        kind: 'value' as const,
      }));
  }

  // --- `@$label.` chaining -------------------------------------------------
  if (field.startsWith('@$label.')) {
    const prop = field.slice('@$label.'.length);
    return (schema.available_labels ?? [])
      .filter((l) => l.key.startsWith(prop))
      .map((l) => ({
        insert: `@$label.${l.key}`,
        label: `@$label.${l.key}`,
        detail: l.type,
        kind: 'tagKey' as const,
      }));
  }

  // --- `@tag.` chaining ----------------------------------------------------
  if (field.startsWith('@tag.')) {
    const prop = field.slice('@tag.'.length);
    return (schema.available_tags ?? [])
      .filter((t) => t.key.startsWith(prop))
      .map((t) => ({
        insert: `@tag.${t.key}:`,
        label: `@tag.${t.key}`,
        detail: t.sample_values?.slice(0, 2).join(', '),
        kind: 'tagKey' as const,
      }));
  }

  const out: Suggestion[] = [];

  // A bare `@tag` is a filterable field in its own right — it means "any tag
  // key" — so it is offered alongside the keys, not only as a chain prefix.
  for (const v of schema.variables ?? []) {
    if (v.prefix.startsWith(field)) {
      out.push({ insert: v.prefix, label: v.prefix, detail: v.description, kind: 'variable' });
    }
  }

  for (const d of schema.dimensions ?? []) {
    if (dimensionMatches(d, field)) {
      out.push({ insert: `${d.name}:`, label: d.name, detail: d.type, kind: 'field' });
    }
  }

  return out;
}

/**
 * The placeholder, built from what THIS resource declares.
 *
 * Hand-written placeholders are what let `SessionsList` advertise `@tag=v1` on
 * a resource whose tag dimension the backend deliberately withholds — a query
 * that always 400s. A page cannot make that mistake if it does not write the
 * copy.
 */
export function placeholderFor(schema: SchemaDefinition | null): string {
  if (!schema) return 'Search…';
  const parts: string[] = [];
  const first = (schema.dimensions ?? [])[0];
  if (first) {
    parts.push(first.options?.length ? `${first.name}:${first.options[0]}` : `${first.name}:…`);
  }
  const variable = (schema.variables ?? [])[0];
  if (variable) parts.push(`${variable.prefix}.key:value`);
  return parts.length ? `Search ${parts.join(', ')}…` : 'Search…';
}

/** Levenshtein distance, iterative two-row. */
function editDistance(a: string, b: string): number {
  let prev = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    const row = [i];
    for (let j = 1; j <= b.length; j++) {
      row[j] = Math.min(
        prev[j] + 1,
        row[j - 1] + 1,
        prev[j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1),
      );
    }
    prev = row;
  }
  return prev[b.length];
}

/**
 * The nearest known field to a name the server rejected.
 *
 * Client-side because the schema is already in hand — this costs no request.
 * It never invents a field: the candidates are exactly what the schema
 * advertises, so a suggestion is always something the resolver accepts.
 */
export function didYouMean(
  schema: SchemaDefinition | null,
  unknownField: string,
): string | null {
  if (!schema || !unknownField) return null;
  const candidates = (schema.dimensions ?? []).flatMap((d) => [d.name, ...(d.aliases ?? [])]);
  let best: string | null = null;
  let bestScore = Infinity;
  for (const c of candidates) {
    const score = editDistance(unknownField.toLowerCase(), c.toLowerCase());
    if (score < bestScore) {
      bestScore = score;
      best = c;
    }
  }
  // A third of the length, floor 1: close enough to be a typo, not a different
  // word. `zzzzzzzz` must return nothing rather than the least-bad match.
  const tolerance = Math.max(1, Math.floor(unknownField.length / 3));
  return best !== null && bestScore <= tolerance ? best : null;
}
