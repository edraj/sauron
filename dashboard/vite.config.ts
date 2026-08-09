import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Plain Vite + Svelte 5 (no SvelteKit).

/**
 * URL prefix the dashboard is served under, e.g. `/sauron/` for
 * `https://host/sauron/`. Empty or `/` (the default) means the host root.
 *
 * This has to be decided at BUILD time. Vite bakes the prefix into the
 * module-preload helper that resolves every lazy route chunk:
 *
 *     const assetsURL = function (dep) { return "/sauron/" + dep }
 *
 * Rewriting index.html in a reverse proxy is NOT an equivalent substitute. That
 * reaches the entry `<script>` and nothing else, so the app shell boots and then
 * every dynamic import resolves against the host root and 404s — a dashboard that
 * loads and cannot open a single page. Set this variable instead of rewriting
 * HTML, and rebuild whenever the prefix changes.
 *
 * Accepts `sauron`, `/sauron`, or `/sauron/` — Vite requires both slashes, so
 * they are normalised here rather than left as a way to get a silently broken
 * bundle.
 */
function basePath(): string {
  const raw = process.env.DASHBOARD_BASE_PATH?.trim();
  if (!raw) return '/';
  const trimmed = raw.replace(/^\/+|\/+$/g, '');
  return trimmed ? `/${trimmed}/` : '/';
}

export default defineConfig({
  plugins: [svelte()],
  base: basePath(),
  // Serve static/ (config.js, favicon) at the base. config.js is injected at
  // runtime in production and must be reachable at `<base>config.js` — the
  // generated index.html references it through the same base as the bundle.
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
