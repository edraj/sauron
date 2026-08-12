import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the search controls.
 *
 * Exists because the defect this work repairs is invisible to every static
 * gate: `svelte-check` and vitest both passed while the search input was styled
 * entirely in Tailwind classes this project does not define, so the input
 * ignored every design token and the suggestion dropdown rendered with no
 * background at all — transparent over the table beneath it.
 *
 * **Stubbed at the HTTP layer, not the module layer.** A `resolve.alias` on
 * `lib/api/client` does not work: aliases match the import SPECIFIER before
 * resolution, and `schema.ts` imports it as the relative `'./client'`, so the
 * pattern silently matched nothing and the harness ran the real client — which
 * went to `localhost:8090` (the pin in the committed `static/config.js`) and
 * failed with a connection refused that read as a broken component. A
 * `resolveId` plugin did not take either.
 *
 * Answering the request instead is both simpler and a better test: axios, the
 * interceptors, `fetchSchema` and the suggestion model are all the real code,
 * and only the server on the far end is canned.
 */
const SCHEMAS = {
  issues: {
    resource: 'issues',
    variables: [{ prefix: '@tag', description: 'Developer tags', chainable: true }],
    dimensions: [
      {
        name: 'level',
        type: 'enum',
        ops: ['=', '!=', 'in'],
        options: ['debug', 'info', 'warning', 'error', 'fatal'],
      },
      { name: 'status', type: 'enum', ops: ['=', '!='], options: ['unresolved', 'resolved'] },
      { name: 'type', type: 'string', ops: ['=', '!=', 'contains'] },
      { name: 'culprit', type: 'string', ops: ['=', 'contains'] },
      { name: 'timesSeen', type: 'integer', ops: ['=', '>', '<'], aliases: ['times_seen'] },
      { name: 'usersSeen', type: 'integer', ops: ['=', '>', '<'] },
      { name: 'lastSeen', type: 'timestamp', ops: ['>', '<'] },
    ],
    // App-specific rather than the old `environment`/`release` fixture — this
    // is the shape the sampler now returns.
    available_tags: [
      { key: 'checkout_step', sample_values: ['payment', 'address'] },
      { key: 'region', sample_values: ['eu-central-1', 'us-east-1'] },
    ],
    available_labels: [],
  },
  sessions: {
    resource: 'sessions',
    // No `@tag` — the resource genuinely has none, which is finding C.
    variables: [{ prefix: '@context', description: 'Device/runtime context', chainable: true }],
    dimensions: [
      { name: 'startedAt', type: 'timestamp', ops: ['>', '<'] },
      { name: 'duration', type: 'duration', ops: ['>', '<'] },
    ],
    available_tags: [],
    available_labels: [],
  },
};

function stubApi() {
  const root = fileURLToPath(new URL('./search-harness', import.meta.url));
  return {
    name: 'search-harness-stub-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const [path, qs] = (req.url ?? '').split('?');

        // Point the app's runtime config at THIS origin. `static/config.js` is
        // committed with `apiBaseUrl` pinned to :8090 and, because it assigns a
        // runtime global, it outranks any build-time env var.
        if (path === '/config.js') {
          res.setHeader('Content-Type', 'application/javascript');
          res.setHeader('Cache-Control', 'no-store');
          res.end(
            `window.__SAURON_CONFIG__ = ${JSON.stringify({
              apiBaseUrl: 'http://localhost:3021',
              ingestBaseUrl: 'http://localhost:3021',
            })};\n`,
          );
          return;
        }

        if (path?.endsWith('/search/schema')) {
          const ctx = new URLSearchParams(qs ?? '').get('context') ?? 'issues';
          const body = SCHEMAS[ctx];
          res.setHeader('Content-Type', 'application/json');
          res.setHeader('Cache-Control', 'no-store');
          if (!body) {
            res.statusCode = 400;
            res.end(JSON.stringify({ error: `invalid context: ${ctx}` }));
            return;
          }
          res.end(JSON.stringify(body));
          return;
        }

        // The app shell asks for these on boot; answering keeps the console
        // clean so a real error stands out.
        if (path?.startsWith('/v1/')) {
          res.setHeader('Content-Type', 'application/json');
          res.statusCode = 404;
          res.end('{}');
          return;
        }

        if (path === '/' || path === '/index.html') {
          res.setHeader('Content-Type', 'text/html');
          res.end(readFileSync(`${root}/index.html`, 'utf8'));
          return;
        }
        next();
      });
    },
  };
}

export default defineConfig({
  plugins: [stubApi(), svelte()],
  root: fileURLToPath(new URL('./search-harness', import.meta.url)),
  server: { port: 3021, strictPort: true },
});
