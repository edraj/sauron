/**
 * Turning a failed search request into a message that belongs ON the input.
 *
 * Only failures that are ABOUT the query land here. A 500 or a dropped
 * connection is the page's error card's job — marking the input invalid for a
 * server fault would tell the reader to fix a query that is fine, and they
 * would sit there editing it.
 */
import { didYouMean, type SchemaDefinition } from '../api/schema';

/** Pulls the field name out of the backend's `unknown field \`x\`` wording. */
function unknownFieldIn(message: string): string | null {
  const m = message.match(/unknown field [`'"]?([A-Za-z0-9_.$@-]+)[`'"]?/i);
  return m ? m[1] : null;
}

export function queryErrorFor(
  status: number | null,
  message: string | null,
  schema: SchemaDefinition | null,
): string | null {
  if (!message) return null;
  // 400 = the query is malformed or names something unknown.
  // 403 = a withheld dimension; the backend's text already names the
  //       permission that lifts it, so it is passed through unparaphrased —
  //       a paraphrase here would drift from the wording the API owns.
  if (status !== 400 && status !== 403) return null;
  if (status === 403) return message;

  const bad = unknownFieldIn(message);
  const near = bad ? didYouMean(schema, bad) : null;
  return near ? `${message} — did you mean \`${near}\`?` : message;
}

/**
 * Structural problems worth catching before a request goes out.
 *
 * Deliberately shallow: unknown FIELDS stay a server-side 400, because only the
 * backend holds the catalog and a client-side copy is exactly the rot the
 * anti-rot test exists to prevent. What is checked here is syntax the grammar
 * cannot accept under any catalog.
 */
export function preflight(query: string): string | null {
  const q = query.trim();
  if (!q) return null;

  let depth = 0;
  let inQuote = false;
  for (const ch of q) {
    if (ch === '"') inQuote = !inQuote;
    if (inQuote) continue;
    if (ch === '(') depth++;
    if (ch === ')') depth--;
    if (depth < 0) return 'Unbalanced parentheses — a `)` has no opening `(`.';
  }
  if (depth > 0) return 'Unbalanced parentheses — close the `(` before searching.';
  if (inQuote) return 'Unclosed quote — close the `"` before searching.';

  const last = q.split(/\s+/).pop() ?? '';
  if (last === 'OR' || last === 'AND') {
    return `Dangling \`${last}\` — add the term it joins.`;
  }
  return null;
}
