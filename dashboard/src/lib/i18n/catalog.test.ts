import { describe, expect, it } from 'vitest';
import { MESSAGES, PLURALS, DOMAINS, PLURAL_DOMAINS } from './catalog';
import { LOCALES } from './types';

/** Any character from the Arabic blocks, including presentation forms. */
const ARABIC = /[؀-ۿݐ-ݿࢠ-ࣿﭐ-﷿ﹰ-﻿]/;

/**
 * Keys whose Arabic value is legitimately free of Arabic script.
 *
 * Only proper nouns, acronyms, and units belong here. Every addition is a
 * claim that the string reads correctly to an Arabic speaker *as Latin text* —
 * so the list is deliberately short and deliberately annoying to grow.
 */
const NO_TRANSLATION_NEEDED = new Set<string>([
  // Apple's and Google's store brands are not translated in Arabic-language
  // app listings either; rendering them in Arabic script would name a
  // different thing than the button the user is looking for.
  'ui.store.appStore',
  'ui.store.play',
  // An example email address. The shape is what the placeholder communicates;
  // Arabic script here would stop it reading as an address at all.
  'auth.placeholder.email',
  // A protocol name. "HTTP(S)" is what the monitor's own type field carries and
  // what the user types into a URL bar; transliterating it names nothing.
  'monitors.http',
  // An example host:port. Like the email placeholder, the shape is the message.
  'monitors.placeholder.hostPort',
  // Another example address, in the member-invite dialog.
  'members.placeholder.email',
  // Environment names are lowercase Latin identifiers — they travel in DSNs,
  // filter chips and the query language, so the example has to look like one.
  'environments.placeholder',
  // Literal JSON key names the detector matches against payloads. Translating
  // them would make the example describe keys no SDK actually sends.
  'inspector.placeholder.keys',
  // A permission identifier. `app:read` is the string the server checks and the
  // role editor lists; an Arabic rendering would name no permission at all.
  'prose.env.partialList.b',
]);

describe('catalogue parity', () => {
  it('defines every locale for every message', () => {
    const incomplete: string[] = [];
    for (const [key, message] of Object.entries(MESSAGES)) {
      for (const locale of LOCALES) {
        const value = (message as Record<string, string>)[locale];
        if (typeof value !== 'string' || value.trim() === '') {
          incomplete.push(`${key} [${locale}]`);
        }
      }
    }
    expect(incomplete).toEqual([]);
  });

  /**
   * The type system already forbids omitting the `ar` field, but not filling
   * it with the English text — which is exactly what happens when a string is
   * added in a hurry. Nothing downstream would notice: it type-checks,
   * renders, and looks like a deliberate choice.
   */
  it('writes Arabic values in Arabic script', () => {
    const untranslated: string[] = [];
    for (const [key, message] of Object.entries(MESSAGES)) {
      if (NO_TRANSLATION_NEEDED.has(key)) continue;
      const ar = (message as Record<string, string>).ar;
      if (!ARABIC.test(ar)) untranslated.push(`${key}: ${JSON.stringify(ar)}`);
    }
    expect(untranslated).toEqual([]);
  });

  /**
   * `MESSAGES` is built by spreading the domain files together, so a key
   * present in two of them resolves to whichever spread ran last — silently,
   * and with the losing file's translation discarded. The merged object cannot
   * show this, which is why `DOMAINS` exposes the parts.
   */
  it('defines each key in exactly one domain', () => {
    const seen = new Map<string, string>();
    const collisions: string[] = [];
    for (const [domain, messages] of Object.entries(DOMAINS)) {
      for (const key of Object.keys(messages)) {
        const previous = seen.get(key);
        if (previous) collisions.push(`${key} in both ${previous} and ${domain}`);
        else seen.set(key, domain);
      }
    }
    expect(collisions).toEqual([]);
  });

  it('exposes every domain through DOMAINS', () => {
    // A domain imported into MESSAGES but forgotten in DOMAINS would escape
    // the collision check above without any other symptom.
    const fromDomains = new Set(Object.values(DOMAINS).flatMap((m) => Object.keys(m)));
    const missing = Object.keys(MESSAGES).filter((k) => !fromDomains.has(k));
    expect(missing).toEqual([]);
  });
});

describe('plural catalogue', () => {
  /**
   * The categories `Intl.PluralRules` can actually return for each locale.
   * Asserting against the runtime's own answer rather than a hardcoded list
   * means this keeps testing the real contract if ICU data ever shifts.
   */
  it('covers every plural category the runtime selects', () => {
    const gaps: string[] = [];
    for (const locale of LOCALES) {
      const categories = new Set(
        // 0-200 reaches every Arabic category: zero(0), one(1), two(2),
        // few(3-10), many(11-99), other(100+).
        Array.from({ length: 201 }, (_, n) => new Intl.PluralRules(locale).select(n)),
      );
      for (const [key, message] of Object.entries(PLURALS)) {
        const forms = (message as Record<string, Record<string, string>>)[locale];
        for (const category of categories) {
          const form = forms?.[category];
          if (typeof form !== 'string' || form.trim() === '') {
            gaps.push(`${key} [${locale}.${category}]`);
          }
        }
      }
    }
    expect(gaps).toEqual([]);
  });

  it('writes Arabic plural forms in Arabic script', () => {
    const untranslated: string[] = [];
    for (const [key, message] of Object.entries(PLURALS)) {
      for (const [category, form] of Object.entries(message.ar)) {
        if (!ARABIC.test(form)) untranslated.push(`${key}.${category}: ${JSON.stringify(form)}`);
      }
    }
    expect(untranslated).toEqual([]);
  });

  it('defines each plural key in exactly one domain', () => {
    const seen = new Map<string, string>();
    const collisions: string[] = [];
    for (const [domain, messages] of Object.entries(PLURAL_DOMAINS)) {
      for (const key of Object.keys(messages)) {
        const previous = seen.get(key);
        if (previous) collisions.push(`${key} in both ${previous} and ${domain}`);
        else seen.set(key, domain);
      }
    }
    expect(collisions).toEqual([]);
  });
});
