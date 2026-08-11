/**
 * The four descriptor columns the Devices inventory groups by, and the
 * querystring encoding that carries one group into the drill-down URL.
 *
 * Absent and empty are kept distinct on the wire: a null component is omitted
 * from the querystring entirely, while an empty-string component is emitted as
 * `os_version=`. `URLSearchParams.get` returns `null` for the former and `''`
 * for the latter, so the backend's `IS NOT DISTINCT FROM` sees the value the
 * device actually stores. Collapse the two and a device whose `os_version` is
 * `''` drills down into the NULL group instead of its own.
 */
export interface DeviceGroupKey {
  family: string | null;
  model: string | null;
  os_name: string | null;
  os_version: string | null;
}

const KEY_FIELDS = ['family', 'model', 'os_name', 'os_version'] as const;

/** `group=1&family=iPhone&…` — the querystring for a group's drill-down URL. */
export function encodeGroupKey(k: DeviceGroupKey): string {
  const p = new URLSearchParams();
  p.set('group', '1');
  for (const f of KEY_FIELDS) {
    const v = k[f];
    if (v !== null) p.set(f, v);
  }
  return p.toString();
}

/** The key a drill-down URL carries, or null when the page is in grouped mode. */
export function decodeGroupKey(qs: string | null): DeviceGroupKey | null {
  const p = new URLSearchParams(qs ?? '');
  if (p.get('group') !== '1') return null;
  return {
    family: p.get('family'),
    model: p.get('model'),
    os_name: p.get('os_name'),
    os_version: p.get('os_version'),
  };
}

/**
 * Value-equality for two possibly-null group keys — the comparison the page's
 * URL-sync effect needs to decide whether the drill-down actually changed.
 *
 * This must NOT be implemented as `encodeGroupKey(a ?? SOME_SENTINEL) ===
 * encodeGroupKey(b ?? SOME_SENTINEL)`: `encodeGroupKey` omits null
 * components, so an all-null sentinel object encodes to the exact same
 * string (`"group=1"`) as the real all-NULL group (every device whose
 * family/model/os_name/os_version are all NULL). That collision makes "no
 * group selected" indistinguishable from "the all-NULL group is selected",
 * which is the bug this function exists to prevent — `null` is checked
 * explicitly, before any field comparison, so it can never be confused with
 * an object whose fields all happen to be null.
 */
export function sameGroupKey(a: DeviceGroupKey | null, b: DeviceGroupKey | null): boolean {
  if (a === null && b === null) return true;
  if (a === null || b === null) return false;
  return KEY_FIELDS.every((f) => a[f] === b[f]);
}

/** Human label for a group — the header chip on the drill-down. */
export function groupLabel(k: DeviceGroupKey): string {
  const device = [k.family, k.model].filter(Boolean).join(' ').trim();
  const os = [k.os_name, k.os_version].filter(Boolean).join(' ').trim();
  const parts = [device, os].filter(Boolean);
  return parts.length > 0 ? parts.join(' · ') : 'Unknown device';
}
