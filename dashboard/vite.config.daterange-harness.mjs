import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the custom date-range picker.
 *
 * Vitest cannot compile Svelte in this project, so nothing under test here —
 * the month grid's arithmetic, its keyboard navigation, its RTL arrow
 * direction, the value it emits — is reachable from the unit suite. `date-
 * range.ts` is unit-tested to death; this exists for the half that only a DOM
 * can answer.
 */
export default defineConfig({
  plugins: [svelte()],
  root: fileURLToPath(new URL('./daterange-harness', import.meta.url)),
  server: { port: 3042, strictPort: true },
});
