import type { Context, Envelope, EnvelopeHeader, EnvelopeItem } from './types.js';

/**
 * Assemble a canonical envelope. This is intentionally trivial — the shape is
 * the contract, so keep it a pure, side-effect-free constructor that mirrors
 * the golden JSON exactly (header, context, items, in that order).
 */
export function buildEnvelope(
  header: EnvelopeHeader,
  context: Context,
  items: EnvelopeItem[],
): Envelope {
  return {
    header,
    context,
    items,
  };
}
