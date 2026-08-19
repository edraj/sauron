import type { Message, PluralMessage } from '../types';
import { common, commonPlurals } from './common';
import { time } from './time';
import { nav } from './nav';
import { account } from './account';
import { ui } from './ui';
import { auth } from './auth';
import { notifications } from './notifications';
import { monitor } from './monitor';
import { explore } from './explore';
import { analyze } from './analyze';
import { admin } from './admin';
import { ops } from './ops';
import { prose } from './prose';
import { docs } from './docs';
import { docsProse } from './docs-prose';

/**
 * Every translatable string in the dashboard, composed from the per-domain
 * files beside this one.
 *
 * Split by domain rather than by language: each key carries its English and
 * Arabic together, so adding a string forces a decision about both, and the
 * two variants sit on adjacent lines for review. The `satisfies Record<string,
 * Message>` on each domain file is what turns a forgotten Arabic value into a
 * `svelte-check` failure instead of a silent English fallback in production.
 *
 * Domain files are added here as slices land. A key must appear exactly once
 * across all of them — the spread would otherwise let a later file silently
 * overwrite an earlier one, which `catalog.test.ts` checks for.
 */
export const MESSAGES = {
  ...common,
  ...time,
  ...nav,
  ...account,
  ...ui,
  ...auth,
  ...notifications,
  ...monitor,
  ...explore,
  ...analyze,
  ...admin,
  ...ops,
  ...prose,
  ...docs,
  ...docsProse,
} as const satisfies Record<string, Message>;

/** Count-dependent strings, selected through `Intl.PluralRules`. */
export const PLURALS = {
  ...commonPlurals,
} as const satisfies Record<string, PluralMessage>;

export type MessageKey = keyof typeof MESSAGES;
export type PluralKey = keyof typeof PLURALS;

/**
 * The domain files, exposed for the parity test.
 *
 * The test needs to see them separately to detect a key defined in two
 * domains — information the merged `MESSAGES` object has already lost.
 */
export const DOMAINS: Record<string, Record<string, Message>> = {
  common,
  time,
  nav,
  account,
  ui,
  auth,
  notifications,
  monitor,
  explore,
  analyze,
  admin,
  ops,
  prose,
  docs,
  docsProse,
};

export const PLURAL_DOMAINS: Record<string, Record<string, PluralMessage>> = {
  commonPlurals,
};
