/**
 * Whether a user-initiated global Refresh is currently in flight.
 *
 * Read by the request interceptor in `client.ts` to append `force=true` to the
 * server-cached endpoints. A module flag rather than a parameter because the
 * thing that needs to say "force" (the Topbar button) and the thing that builds
 * the request (a closure a page constructed) are separated by
 * `CachedView.load`, whose signature is shared by all 34 pages — threading a
 * flag through it would touch every one of them and re-introduce exactly the
 * drift the registry design exists to avoid.
 *
 * Not reactive: nothing renders from it, and the window it covers is one
 * `await` inside a single click handler. `runGlobalRefresh` clears it in a
 * `finally`, because a flag left set would append `force=true` to every later
 * request for the life of the tab and defeat the cache entirely.
 */
let forcing = false;

export function beginForcing(): void {
  forcing = true;
}

export function endForcing(): void {
  forcing = false;
}

export function isForcing(): boolean {
  return forcing;
}
