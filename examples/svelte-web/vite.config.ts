import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Plain Vite + Svelte 5 (no SvelteKit) — mirrors the Sauron dashboard setup.
export default defineConfig({
  plugins: [svelte()],
  server: {
    port: 5173,
    host: true,
    strictPort: false,
  },
  preview: {
    port: 5173,
  },
  // The linked local SDK (`file:../../sdks/js`) is rebuilt in-repo, so do NOT
  // pre-bundle it: Vite can't detect content changes in a symlinked dep, and a
  // cached pre-bundle silently goes stale (e.g. missing newly-added Scope
  // methods). Excluding it makes Vite serve the freshly-built ESM directly so
  // the example always reflects the current SDK build.
  optimizeDeps: {
    exclude: ['@sauron/browser'],
  },
});
