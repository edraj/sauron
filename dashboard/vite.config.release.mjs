import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/** Dev config for verifying the release filter (2026-09-14). Same config.js override trick as vite.config.slice3.mjs. */
const API = process.env.RELEASE_API_BASE ?? 'http://localhost:8140';

function configOverride() {
  return {
    name: 'release-config-override',
    configureServer(server) {
      // Registered inside configureServer (not in a returned callback) so it
      // runs BEFORE Vite's static handler for publicDir, which would otherwise
      // serve the committed static/config.js first.
      server.middlewares.use((req, res, next) => {
        if (req.url?.split('?')[0] !== '/config.js') return next();
        res.setHeader('Content-Type', 'application/javascript');
        res.setHeader('Cache-Control', 'no-store');
        res.end(
          `window.__SAURON_CONFIG__ = ${JSON.stringify({
            apiBaseUrl: API,
            ingestBaseUrl: process.env.RELEASE_INGEST_BASE ?? 'http://localhost:8141',
          })};\n`,
        );
      });
    },
  };
}

export default defineConfig({
  plugins: [configOverride(), svelte()],
  base: '/',
  // Pinned to this file's own directory rather than left to default to the
  // launcher's cwd. `.claude/launch.json` runs vite from the REPO ROOT, where
  // there is no `index.html` and no `static/`, so the default made `/` a 404
  // while `/config.js` still answered from the middleware above — a dev server
  // that looks up and serves nothing.
  root: fileURLToPath(new URL('.', import.meta.url)),
  publicDir: 'static',
  server: { port: 3046, host: true, strictPort: true },
});
