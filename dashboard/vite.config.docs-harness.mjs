import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the Docs page.
 *
 * Exists for the layout, which is the part no unit test can see: whether the
 * table of contents actually STICKS while the page scrolls, and how much of a
 * wide viewport the content column uses. Both are properties of the real
 * cascade under the real `AppShell`, and `position: sticky` in particular fails
 * *silently* — it had been a no-op here because an ancestor's
 * `overflow-x: hidden` made that ancestor a scroll container.
 *
 * Stubbed at the HTTP layer rather than by aliasing modules, for the reason
 * `vite.config.search-harness.mjs` records: an alias matches the import
 * specifier before resolution and silently misses relative imports, leaving the
 * real client pointed at the committed :8090 pin in `static/config.js`.
 */
const ORG = { id: 'org1', name: 'Harness Org', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const PROJECT = { id: 'proj1', org_id: 'org1', name: 'Harness Project', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const APP = { id: 'app1', project_id: 'proj1', name: 'Harness App', slug: 'harness', app_type: 'flutter', ingest_enabled: true, platform: null, store_environment_id: null };
const ENV = { id: 'env1', app_id: 'app1', name: 'production', is_default: true, public_key: 'pk_harness0000000000000000000000', created_at: '2026-01-01T00:00:00Z' };

/** Enough of a schema that the field tables render with real rows. */
const schema = (resource) => ({
  resource,
  variables: [
    { prefix: '@tag', description: 'Developer tags', chainable: true },
    { prefix: '@context', description: 'Device and runtime context', chainable: true },
  ],
  dimensions: [
    { name: 'level', type: 'enum', ops: ['eq', 'neq', 'in'], options: ['debug', 'info', 'warning', 'error', 'fatal'] },
    { name: 'status', type: 'enum', ops: ['eq', 'neq', 'in'], options: ['unresolved', 'resolved', 'ignored'] },
    { name: 'firstSeen', type: 'timestamp', ops: ['gt', 'gte', 'lt', 'lte'], aliases: ['first_seen'] },
    { name: 'lastSeen', type: 'timestamp', ops: ['gt', 'gte', 'lt', 'lte'], aliases: ['last_seen'] },
    { name: 'timesSeen', type: 'integer', ops: ['gt', 'gte', 'lt', 'lte'], aliases: ['times_seen'] },
    { name: 'culprit', type: 'string', ops: ['eq', 'neq', 'contains'] },
  ],
  available_tags: [{ key: 'region', sample_values: ['eu', 'us'] }],
  available_labels: [],
});

function json(res, body) {
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.end(JSON.stringify(body));
}

function stubApi() {
  const root = fileURLToPath(new URL('./docs-harness', import.meta.url));
  return {
    name: 'docs-harness-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const [path, query] = (req.url ?? '').split('?');

        if (path === '/config.js') {
          res.setHeader('Content-Type', 'application/javascript');
          res.setHeader('Cache-Control', 'no-store');
          res.end(
            `window.__SAURON_CONFIG__ = ${JSON.stringify({
              apiBaseUrl: 'http://localhost:3035',
              ingestBaseUrl: 'http://localhost:3035',
            })};\n`,
          );
          return;
        }

        if (path === '/v1/orgs') return json(res, [ORG]);
        if (path === '/v1/orgs/org1/access') {
          return json(res, {
            permissions: ['*'],
            grants: [{ org_id: 'org1', project_id: null, app_id: null, environment_id: null, permissions: ['*'] }],
          });
        }
        if (path === '/v1/orgs/org1/projects') return json(res, [PROJECT]);
        if (path === '/v1/projects/proj1/apps') return json(res, [APP]);
        if (path === '/v1/apps/app1/environments') return json(res, [ENV]);
        if (path === '/v1/apps/app1/search/schema') {
          const ctx = new URLSearchParams(query ?? '').get('context') ?? 'issues';
          return json(res, schema(ctx));
        }

        if (path?.startsWith('/v1/')) {
          console.log(`[docs-harness] unstubbed ${req.method} ${path}`);
          return json(res, []);
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
  root: fileURLToPath(new URL('./docs-harness', import.meta.url)),
  server: { port: 3035, strictPort: true },
});
