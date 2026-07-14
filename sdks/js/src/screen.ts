/**
 * Current-screen state. The SDK stamps this on every event/exception and
 * auto-emits a `$screen` view event when it changes (see api/product.ts).
 */
let currentScreen: string | null = null;

/** The current screen name, or null if none set. */
export function getScreen(): string | null {
  return currentScreen;
}

/** Set the current screen. Returns true iff the value actually changed. */
export function setScreenState(name: string | null): boolean {
  if (name === currentScreen) return false;
  currentScreen = name;
  return true;
}

/** Drop the in-memory value (tests + teardown). */
export function resetScreen(): void {
  currentScreen = null;
}
