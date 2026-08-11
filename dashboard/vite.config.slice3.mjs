import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Dev config for verifying table sorting + pagination (slices 3 and 4).
 *
 * Why this file exists rather than `VITE_API_BASE_URL`:
 * `static/config.js` is COMMITTED with `apiBaseUrl` pinned to
 * `http://localhost:8090`, and because it assigns a runtime global
 * (`window.__SAURON_CONFIG__`) it wins over any build-time env var. Pointing a
 * dev dashboard at a task-specific API therefore needs the served `/config.js`
 * replaced, not an env var set — that mistake has previously produced a
 * dashboard silently talking to a stale API whose schema predates the change
 * under test, which reads as "the feature is broken" when it is not.
 *
 * Serving the override from a middleware keeps the committed file untouched,
 * so nothing has to be remembered and restored afterwards.
 */
const API = process.env.SLICE3_API_BASE ?? 'http://localhost:8100';

function configOverride() {
  return {
    name: 'slice3-config-override',
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
            ingestBaseUrl: API,
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
  server: { port: 3010, host: true, strictPort: true },
});
