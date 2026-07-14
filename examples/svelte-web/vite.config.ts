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
  // The linked local SDK ships a prebuilt ESM bundle; let Vite pre-bundle it.
  optimizeDeps: {
    include: ['@sauron/browser'],
  },
});
