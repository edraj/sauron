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

export interface DetectorOption {
  /** The wire id. Must equal a `Detector::id()` arm in `detect.rs`. */
  id: string;
  label: string;
  /** What the value shape is, for the hint under the checkbox. */
  hint: string;
}

export const DETECTORS: DetectorOption[] = [
  { id: 'email', label: 'Email address', hint: 'jane@acme.co.uk — requires a dot-separated host' },
  { id: 'phone_e164', label: 'Phone (E.164)', hint: '+213551234567 — international form only' },
  { id: 'ipv4', label: 'IPv4 address', hint: '203.0.113.7' },
  { id: 'ipv6', label: 'IPv6 address', hint: '2001:db8::1' },
  { id: 'jwt', label: 'JWT', hint: 'A signed three-segment token' },
  { id: 'iban', label: 'IBAN', hint: 'Bank account number, checksum-validated' },
  { id: 'ssn_us', label: 'US SSN', hint: '123-45-6789' },
  { id: 'credit_card', label: 'Credit card', hint: 'Luhn-validated, so order ids do not match' },
];

/**
 * The starting configuration offered to someone creating their first policy.
 *
 * Keys, not detectors: the prefilter is built from the KEY list, so a
 * detector-only policy reads far more rows for the same scan. These four are
 * the names the SDKs' own examples use.
 */
export const SUGGESTED_KEYS: string[] = ['email', 'phone', 'password', 'token'];
