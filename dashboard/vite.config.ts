import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Plain Vite + Svelte 5 (no SvelteKit).
export default defineConfig({
  plugins: [svelte()],
  // Serve static/ (config.js, favicon) at the web root. config.js is injected at
  // runtime in production and must be reachable at /config.js.
  publicDir: 'static',
  // The backend's CORS allowlist permits http://localhost:3000, so the dev and
  // preview servers run there to work against the live API without proxying.
  server: {
    port: 3000,
    host: true,
    strictPort: true,
  },
  preview: {
    port: 3000,
    strictPort: true,
  },
});
