import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the clickable-row → openable-link work.
 *
 * No API stub: this exercises event plumbing and layout, not wire format, so it
 * mounts the real DataTable, the real TimeValue and the real `rowNav` over a
 * fixed pair of rows rather than booting a page behind a fake server.
 */
export default defineConfig({
  plugins: [svelte()],
  root: fileURLToPath(new URL('./rowlink-harness', import.meta.url)),
  resolve: { alias: { '/src': fileURLToPath(new URL('./src', import.meta.url)) } },
  server: { port: 3043, strictPort: true },
});
