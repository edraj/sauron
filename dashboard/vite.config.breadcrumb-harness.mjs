import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the navigation direction glyph on `BreadcrumbTrail`.
 *
 * Unlike the page-level harnesses in this directory, this one stubs no HTTP:
 * `BreadcrumbTrail` is a leaf that takes a single `breadcrumbs` prop, so the
 * component is mounted directly and the fixtures ARE the input. What needs
 * looking at cannot be asserted in the node-environment unit tests:
 *
 *  - the rail stays straight where glyph rows and dot rows alternate (the
 *    `mixed` fixture) — the reason the node column is a constant width,
 *  - the glyph's opaque background actually hides the line behind it rather
 *    than leaving a visible seam, in BOTH themes,
 *  - a crumb with no direction, or an unrecognised one, still renders its
 *    level dot instead of a blank node (the `fallback` fixture).
 *
 * `?theme=light` / `?theme=dark` seeds the theme before mount.
 */
export default defineConfig({
  plugins: [svelte()],
  root: fileURLToPath(new URL('./breadcrumb-harness', import.meta.url)),
  server: { port: 3034, strictPort: true },
});
