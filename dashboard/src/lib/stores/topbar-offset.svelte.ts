/**
 * The top bar's measured height, for script that has to clear the bar.
 *
 * `Topbar` publishes it alongside the `--topbar-offset` CSS variable. CSS
 * consumers read the variable; this is for the ones CSS cannot reach, like the
 * Docs scroll-spy's IntersectionObserver margin, which used to hard-code the
 * old 60px bar and went stale when the bar changed height.
 *
 * `0` until the bar mounts and measures itself.
 */
export const topbarOffset = $state({ px: 0 });

/**
 * The Docs scroll-spy's root margin: the band starts 12px under the bar (less
 * than the 16px `scroll-margin-top` a jump lands at, so the section jumped to
 * is counted), and the lower 65% of the viewport is ignored.
 */
export function scrollSpyMargin(barPx: number): string {
  return `-${barPx + 12}px 0px -65% 0px`;
}
