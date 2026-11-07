// Single source of truth for the detector vocabulary the Policy tab renders.
//
// Mirrors `ALL_DETECTORS` and `Detector::id()` in
// backend/crates/sauron-inspector/src/detect.rs. The ids are the wire values
// the API stores in `inspector_policies.detectors`, and `parse_detectors`
// DROPS any id this build does not know rather than failing the scan — so a
// typo here does not 400, it silently disables the detector. That failure is
// invisible on a privacy scan (a confident zero-findings result), which is why
// `inspectorDetectors.test.ts` asserts this list against `detect.rs`'s source
// the same way `permissions.test.ts` asserts against `rbac.rs`.

import { t, type MessageKey } from '../i18n';

export interface DetectorOption {
  /** The wire id. Must equal a `Detector::id()` arm in `detect.rs`. */
  id: string;
  /** Catalog key for the checkbox label; translated at render time. */
  labelKey: MessageKey;
  /**
   * A literal value of the shape, shown as-is in every language: like the
   * format-example placeholders, its shape IS the instruction.
   */
  example?: string;
  /** Catalog key for what the value shape is, for the hint under the checkbox. */
  hintKey?: MessageKey;
}

export const DETECTORS: DetectorOption[] = [
  {
    id: 'email',
    labelKey: 'inspector.detector.email',
    example: 'jane@acme.co.uk',
    hintKey: 'inspector.detectorHint.email',
  },
  {
    id: 'phone_e164',
    labelKey: 'inspector.detector.phoneE164',
    example: '+213551234567',
    hintKey: 'inspector.detectorHint.phoneE164',
  },
  { id: 'ipv4', labelKey: 'inspector.detector.ipv4', example: '203.0.113.7' },
  { id: 'ipv6', labelKey: 'inspector.detector.ipv6', example: '2001:db8::1' },
  { id: 'jwt', labelKey: 'inspector.detector.jwt', hintKey: 'inspector.detectorHint.jwt' },
  { id: 'iban', labelKey: 'inspector.detector.iban', hintKey: 'inspector.detectorHint.iban' },
  { id: 'ssn_us', labelKey: 'inspector.detector.ssnUs', example: '123-45-6789' },
  {
    id: 'credit_card',
    labelKey: 'inspector.detector.creditCard',
    hintKey: 'inspector.detectorHint.creditCard',
  },
];

/**
 * The tooltip for a detector checkbox: the example, the translated hint, or
 * both joined by an em dash ("jane@acme.co.uk — requires a dot-separated
 * host"). Calls `t()`, so call it from markup or `$derived` to follow a
 * language switch.
 */
export function detectorHint(d: DetectorOption): string {
  const hint = d.hintKey ? t(d.hintKey) : '';
  if (d.example && hint) return `${d.example} — ${hint}`;
  return d.example ?? hint;
}

/**
 * The starting configuration offered to someone creating their first policy.
 *
 * Keys, not detectors: the prefilter is built from the KEY list, so a
 * detector-only policy reads far more rows for the same scan. These four are
 * the names the SDKs' own examples use.
 */
export const SUGGESTED_KEYS: string[] = ['email', 'phone', 'password', 'token'];
