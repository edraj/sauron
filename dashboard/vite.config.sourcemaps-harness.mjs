import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the Source Maps page's Dart coverage warning.
 *
 * The fixture is the whole point: three builds, one in each state the card has
 * to tell apart — complete (must NOT appear), symbols with no obfuscation map
 * (the common mistake), and a map with no symbols. A unit test pins the
 * grouping; only this shows that the card renders the right rows, hides the
 * healthy build, and offers the prefill button on the right one.
 */
const ORG = { id: 'org1', name: 'Harness Org', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const PROJECT = { id: 'proj1', org_id: 'org1', name: 'Harness Project', slug: 'harness', created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z' };
const APP = { id: 'app1', project_id: 'proj1', name: 'Harness App', slug: 'harness', app_type: 'flutter', ingest_enabled: true, platform: null, store_environment_id: null };

const artifact = (over) => ({
  id: `art-${Math.random().toString(36).slice(2)}`,
  kind: 'dart_symbols',
  platform: 'android',
  arch: 'arm64',
  release: 'app@1.4.2+12',
  dist: null,
  name: null,
  debug_id: null,
  blob_sha256: 'ab'.repeat(32),
  has_prebuilt_index: false,
  uncompressed_size: 4_200_000,
  compressed_size: 900_000,
  created_at: '2026-08-17T10:00:00Z',
  ...over,
});

const ARTIFACTS = [
  // Complete build — must NOT show up in the warning.
  artifact({ debug_id: 'aaaa1111complete0000000000000000000000aa' }),
  artifact({ debug_id: 'aaaa1111complete0000000000000000000000aa', arch: 'armeabi-v7a' }),
  artifact({ debug_id: 'aaaa1111complete0000000000000000000000aa', kind: 'dart_obfuscation_map', arch: null }),
  // Symbols only — the common mistake, gets an "Upload map" button.
  artifact({ debug_id: 'bbbb2222symbolsonly00000000000000000000bb' }),
  artifact({ debug_id: 'bbbb2222symbolsonly00000000000000000000bb', arch: 'armeabi-v7a' }),
  // Map only, on iOS — the rare one, no button.
  artifact({ debug_id: 'cccc3333maponly000000000000000000000000cc', kind: 'dart_obfuscation_map', arch: null, platform: 'ios' }),
  // A JS map, which the coverage check must ignore entirely.
  artifact({ kind: 'js_sourcemap', platform: 'web', arch: null, name: '~/static/app.min.js', release: 'web@1.4.2' }),
];

/** Enough to unlock this page: listing needs `issue:read`, upload/delete `artifact:write`. */
const PERMS = ['issue:read', 'event:read', 'artifact:write', 'source:read'];

function json(res, body) {
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.end(JSON.stringify(body));
}

function stubApi() {
  const root = fileURLToPath(new URL('./sourcemaps-harness', import.meta.url));
  return {
    name: 'sourcemaps-harness-api',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const [path] = (req.url ?? '').split('?');
        if (path === '/config.js') {
          res.setHeader('Content-Type', 'application/javascript');
          res.setHeader('Cache-Control', 'no-store');
          res.end(
            `window.__SAURON_CONFIG__ = ${JSON.stringify({
              apiBaseUrl: 'http://localhost:3036',
              ingestBaseUrl: 'http://localhost:3036',
            })};\n`,
          );
          return;
        }
        if (path === '/v1/orgs') return json(res, [ORG]);
        if (path === '/v1/orgs/org1/access') {
          // `sessionStore.can` matches grants on `scope_type`/`scope_id` and
          // asks `permissions.includes(perm)` — there is no `'*'` wildcard and
          // no `{org_id, app_id}` shape. Getting this wrong does not fail
          // loudly: every gated control simply renders locked, which reads as a
          // product bug rather than a bad fixture.
          return json(res, {
            permissions: PERMS,
            grants: [{ scope_type: 'org', scope_id: 'org1', permissions: PERMS }],
          });
        }
        if (path === '/v1/orgs/org1/projects') return json(res, [PROJECT]);
        if (path === '/v1/projects/proj1/apps') return json(res, [APP]);
        if (path === '/v1/apps/app1/environments') return json(res, []);
        if (path === '/v1/apps/app1/artifacts') return json(res, ARTIFACTS);
        if (path?.startsWith('/v1/')) {
          console.log(`[sourcemaps-harness] unstubbed ${req.method} ${path}`);
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
  root: fileURLToPath(new URL('./sourcemaps-harness', import.meta.url)),
  server: { port: 3036, strictPort: true },
});
