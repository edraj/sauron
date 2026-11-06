import { describe, expect, it } from 'vitest';
import { scrollSpyMargin, topbarOffset } from './topbar-offset.svelte';

describe('scrollSpyMargin', () => {
  it('starts the spy just under the bar, whatever its measured height', () => {
    // Desktop bar, and the phone bar wrapped to two rows.
    expect(scrollSpyMargin(48)).toBe('-60px 0px -65% 0px');
    expect(scrollSpyMargin(83)).toBe('-95px 0px -65% 0px');
  });

  it('leaves a section scrolled to (bar + 16px) inside the spied band', () => {
    // Docs' `scroll-margin-top` is `--topbar-offset + 16px`; the band must
    // start above that, or the section a jump lands on is not counted.
    const top = -Number(/^-(\d+)px/.exec(scrollSpyMargin(83))![1]);
    expect(-top).toBeLessThan(83 + 16);
  });
});

describe('topbarOffset', () => {
  it('is 0 until the bar has measured itself', () => {
    expect(topbarOffset.px).toBe(0);
  });
});
