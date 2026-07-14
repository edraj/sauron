import { describe, expect, it, beforeEach } from 'vitest';
import { getScreen, setScreenState, resetScreen } from '../src/screen.js';

describe('screen state', () => {
  beforeEach(() => resetScreen());

  it('starts null', () => {
    expect(getScreen()).toBeNull();
  });

  it('reports a change and stores the value', () => {
    expect(setScreenState('Home')).toBe(true);
    expect(getScreen()).toBe('Home');
  });

  it('reports no change for the same name', () => {
    setScreenState('Home');
    expect(setScreenState('Home')).toBe(false);
  });
});

import { init } from '../src/client.js';
import { track, setScreen } from '../src/api/product.js';

let items: unknown[] = [];

describe('screen on items', () => {
  beforeEach(() => {
    resetScreen();
    items = [];
    init({ dsn: 'https://pk_test@localhost:9/1', beforeSend: (i) => { items.push(i); return null; } });
  });

  it('stamps the current screen on events', () => {
    setScreen('Home');
    track('clicked');
    // items[0] is the $screen view, items[1] is the clicked event
    const clicked = items.find((i: any) => i.name === 'clicked') as any;
    expect(clicked.screen).toBe('Home');
  });

  it('emits a $screen event only on change', () => {
    setScreen('Home');
    setScreen('Home');
    const views = items.filter((i: any) => i.name === '$screen');
    expect(views).toHaveLength(1);
    expect((views[0] as any).properties.screen).toBe('Home');
  });
});
