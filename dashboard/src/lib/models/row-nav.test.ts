import { describe, expect, it } from 'vitest';
import { decideRowNav, rowHref, type RowNavInput } from '../models/row-nav';

// `rowNav` itself is a four-line adapter over `decideRowNav` — it reads
// `e.target.closest()` and calls `push`/`window.open`. The suite here runs in
// node with no DOM, so the decision is what gets tested; the wiring is covered
// by the browser harness (vite.config.rowlink-harness.mjs).

const LEFT = 0;
const MIDDLE = 1;
const RIGHT = 2;

const on = (over: Partial<RowNavInput> = {}) => (e: Partial<RowNavInput>) =>
  decideRowNav({ button: LEFT, ...over, ...e });

const plainCell = on();
const ownLink = on({ overLink: true });
const otherLink = on({ overLink: true });
const timeToggle = on({ overControl: true });

describe('rowHref', () => {
  it('prefixes the hash the router reads', () => {
    expect(rowHref('/issues/42')).toBe('#/issues/42');
  });

  it('keeps a query string intact, for rows that carry a filter', () => {
    // DeviceGroupTable navigates to /devices?family=…&model=… rather than to a
    // record id.
    expect(rowHref('/devices?family=Pixel&model=8')).toBe('#/devices?family=Pixel&model=8');
  });
});

describe('a plain cell', () => {
  it('navigates in place on an unmodified left-click, as it always has', () => {
    expect(plainCell({})).toBe('navigate');
  });

  it.each(['ctrlKey', 'metaKey', 'shiftKey'] as const)(
    'opens a new tab on %s + left-click instead of navigating away',
    (mod) => {
      expect(plainCell({ [mod]: true })).toBe('new-tab');
    },
  );

  it('opens a new tab on middle-click', () => {
    expect(plainCell({ button: MIDDLE })).toBe('new-tab');
  });

  it('leaves right-click to the context menu', () => {
    expect(plainCell({ button: RIGHT })).toBe('ignore');
  });

  it('leaves ctrl + right-click to the context menu too', () => {
    // `auxclick` fires for middle AND right. Reading the modifier before the
    // button would open a tab behind the menu.
    expect(plainCell({ button: RIGHT, ctrlKey: true })).toBe('ignore');
  });
});

describe("the row's own first-column link", () => {
  it('is left to the anchor on a plain click, so the row does not push twice', () => {
    expect(ownLink({})).toBe('ignore');
  });

  it('does not add a second tab on middle-click', () => {
    expect(ownLink({ button: MIDDLE })).toBe('ignore');
  });

  it('does not add a second tab on ctrl-click', () => {
    expect(ownLink({ ctrlKey: true })).toBe('ignore');
  });
});

describe('a nested link to a different record', () => {
  // SessionsList columns 2 and 3 link out to the person and the device;
  // DeviceFlatTable column 4 links to the person. The row goes somewhere else.

  it('keeps its plain left-click', () => {
    expect(otherLink({})).toBe('ignore');
  });

  it('does not also open the ROW in a second tab on middle-click', () => {
    // The regression this guard exists for: the browser opens #/persons/p1 and
    // the row opens #/sessions/s1 alongside it — two tabs, one unasked for.
    // Those anchors stop propagation on `click` only, so nothing but this
    // stands between the user and that on `auxclick`.
    expect(otherLink({ button: MIDDLE })).toBe('ignore');
  });

  it('does not also open the ROW in a second tab on ctrl-click', () => {
    expect(otherLink({ ctrlKey: true })).toBe('ignore');
  });
});

describe('a non-link control in the row', () => {
  // TimeValue's relative/absolute toggle, in columns 2..n of six of these
  // tables.

  it('keeps its plain click', () => {
    expect(timeToggle({})).toBe('ignore');
  });

  it('still opens the row in a new tab on middle-click', () => {
    // A button is not a destination, so unlike an anchor there is no competing
    // tab to double up on — and middle-clicking it does nothing today, so this
    // takes nothing away.
    expect(timeToggle({ button: MIDDLE })).toBe('new-tab');
  });

  it('is never reached by a modified left-click, which the control eats first', () => {
    // Documented rather than relied on: TimeValue stops propagation in its own
    // `onclick`, so a ctrl-click there never reaches the row at all. If one
    // ever did, a new tab is the right answer for it.
    expect(timeToggle({ ctrlKey: true })).toBe('new-tab');
  });
});
