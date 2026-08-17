import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the pager.
 *
 * The pager's defects are visual and arithmetic, and neither `svelte-check` nor
 * vitest can see them: a strip that changes width between pages, a bar with no
 * separation from the table above it, and an ellipsis standing in for a single
 * page all compile, type-check and render "successfully".
 *
 * No API stub — `PageStrip` and its two adapters take plain props, so nothing
 * here needs a server. That is the point of having split the presentation out.
 */
export default defineConfig({
  plugins: [svelte()],
  root: fileURLToPath(new URL('./pager-harness', import.meta.url)),
  server: { port: 3031, strictPort: true },
});
