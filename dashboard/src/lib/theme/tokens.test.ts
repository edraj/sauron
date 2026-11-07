// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import fs from 'node:fs';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { contrast, parseThemes } from './tokens';

// Read from disk, not `?raw`: vitest's CSS pipeline swallows `.css?raw` and
// hands back an empty string, which would make every floor below vacuous.
const css = fs.readFileSync(
  path.resolve(path.dirname(new URL(import.meta.url).pathname), '../../app.css'),
  'utf-8',
);

/**
 * The Quiet theme is restrained, not low-contrast. These floors are the
 * spec's, checked against the values actually shipped in `app.css`, so a
 * hex tuned by eye cannot quietly push muted text below legibility.
 */
const themes = parseThemes(css);

describe('contrast()', () => {
  it('is 21 for black on white and 1 for a colour on itself', () => {
    expect(contrast('#000000', '#ffffff')).toBeCloseTo(21, 1);
    expect(contrast('#7c83f7', '#7c83f7')).toBeCloseTo(1, 5);
  });
});

describe.each(['dark', 'light'] as const)('%s theme', (name) => {
  const v = themes[name];

  it.each(['text', 'text-muted'])('--%s reaches 4.5:1 on --bg and --surface', (tok) => {
    expect(contrast(v[tok], v.bg)).toBeGreaterThanOrEqual(4.5);
    expect(contrast(v[tok], v.surface)).toBeGreaterThanOrEqual(4.5);
  });

  it('--text-faint reaches 3:1 on --bg and --surface', () => {
    expect(contrast(v['text-faint'], v.bg)).toBeGreaterThanOrEqual(3);
    expect(contrast(v['text-faint'], v.surface)).toBeGreaterThanOrEqual(3);
  });

  it.each(['fatal', 'error', 'warning', 'info', 'success'])('--%s reaches 4.5:1 on --bg', (tok) => {
    expect(contrast(v[tok], v.bg)).toBeGreaterThanOrEqual(4.5);
  });

  // White labels on the filled buttons. The accent and error hues that read
  // well as TEXT on a dark page are too light to carry white text, so the
  // filled buttons get their own deeper "solid" fills.
  it('primary and danger button labels reach 4.5:1 on their fills', () => {
    expect(contrast(v['primary-contrast'], v['primary-solid'])).toBeGreaterThanOrEqual(4.5);
    expect(contrast(v['primary-contrast'], v['error-solid'])).toBeGreaterThanOrEqual(4.5);
    // Hover and pressed must not lighten the fill below the floor.
    expect(contrast(v['primary-contrast'], v['primary-solid-hover'])).toBeGreaterThanOrEqual(4.5);
    expect(contrast(v['primary-contrast'], v['primary-active'])).toBeGreaterThanOrEqual(4.5);
  });

  it('has no background glow and the Quiet geometry', () => {
    expect(v['bg-gradient']).toBeUndefined();
    expect(v['control-h']).toBe('32px');
    expect(v.radius).toBe('6px');
  });
});
