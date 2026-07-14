import { getDeviceId } from './identity.js';
import type { AppContext, Context, DeviceContext, OsContext, RuntimeContext, UserContext } from './types.js';

/**
 * Best-effort environment detection from `navigator`. All of this is fuzzy by
 * nature (UA freezing, spoofing, non-browser hosts) so every field degrades to
 * `null` rather than guessing wrong in a way that breaks the wire contract.
 */

interface NavigatorLike {
  userAgent?: string;
  platform?: string;
  userAgentData?: { platform?: string };
}

function getNavigator(): NavigatorLike | undefined {
  const g = globalThis as { navigator?: NavigatorLike };
  return g.navigator;
}

function detectOs(ua: string): OsContext {
  // Windows
  let m = ua.match(/Windows NT ([\d.]+)/);
  if (m) return { name: 'Windows', version: m[1] };
  // iOS (before macOS, since iPad UAs can contain "Mac OS X")
  m = ua.match(/iPhone OS (\d+[_\d]*)/) || ua.match(/CPU OS (\d+[_\d]*) like Mac/);
  if (m) return { name: 'iOS', version: m[1].replace(/_/g, '.') };
  // macOS
  m = ua.match(/Mac OS X (\d+[_\d]*)/);
  if (m) return { name: 'macOS', version: m[1].replace(/_/g, '.') };
  // Android
  m = ua.match(/Android ([\d.]+)/);
  if (m) return { name: 'Android', version: m[1] };
  if (/Linux/.test(ua)) return { name: 'Linux', version: null };
  if (/CrOS/.test(ua)) return { name: 'Chrome OS', version: null };
  return { name: null, version: null };
}

function detectRuntime(ua: string): RuntimeContext {
  // Order matters: Edge/Opera masquerade as Chrome; Chrome masquerades as Safari.
  let m = ua.match(/Edg(?:e|A|iOS)?\/([\d.]+)/);
  if (m) return { name: 'Edge', version: major(m[1]) };
  m = ua.match(/OPR\/([\d.]+)/) || ua.match(/Opera\/([\d.]+)/);
  if (m) return { name: 'Opera', version: major(m[1]) };
  m = ua.match(/Firefox\/([\d.]+)/);
  if (m) return { name: 'Firefox', version: major(m[1]) };
  m = ua.match(/Chrome\/([\d.]+)/);
  if (m) return { name: 'Chrome', version: major(m[1]) };
  m = ua.match(/Version\/([\d.]+).*Safari/);
  if (m) return { name: 'Safari', version: major(m[1]) };
  if (/Safari/.test(ua)) return { name: 'Safari', version: null };
  return { name: null, version: null };
}

function detectDevice(nav: NavigatorLike | undefined, ua: string): DeviceContext {
  const platform = nav?.userAgentData?.platform ?? nav?.platform ?? '';
  let family: string | null = null;
  if (/Mac|iPhone|iPad|iPod/.test(platform) || /iPhone|iPad|Macintosh/.test(ua)) {
    family = 'Apple';
  } else if (/Win/.test(platform) || /Windows/.test(ua)) {
    family = 'Microsoft';
  } else if (/Android/.test(ua)) {
    family = 'Google';
  } else if (/Linux/.test(platform) || /Linux/.test(ua)) {
    family = 'Linux';
  }
  return { device_id: getDeviceId(), family, model: null, arch: null };
}

/** Take the leading major version component of a dotted version string. */
function major(version: string): string {
  const dot = version.indexOf('.');
  return dot === -1 ? version : version.slice(0, dot);
}

/** Derive `app.version` from a `name@version` release string. */
function appFromRelease(release: string | null): AppContext {
  if (!release) return { version: null, build: null };
  const at = release.lastIndexOf('@');
  const version = at === -1 ? release : release.slice(at + 1);
  return { version: version || null, build: null };
}

/**
 * Build the environment portion of the context (everything except the user).
 * The user is merged in by the client from its `Scope`.
 */
export function detectContext(release: string | null): Omit<Context, 'user'> {
  const nav = getNavigator();
  const ua = nav?.userAgent ?? '';
  return {
    device: detectDevice(nav, ua),
    os: detectOs(ua),
    app: appFromRelease(release),
    runtime: detectRuntime(ua),
  };
}

/** Combine the detected environment context with the current user. */
export function buildContext(release: string | null, user: UserContext): Context {
  return { ...detectContext(release), user };
}
