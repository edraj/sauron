import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

/**
 * Render harness for the permission-locking work.
 *
 * Exists because none of it can fail in a way the static gates see.
 * `svelte-check` and vitest are both happy with a tooltip that never opens, a
 * locked button that is unreachable by keyboard, and a locked nav item whose
 * click still fires — the three things this harness is here to disprove.
 *
 * No bootstrap stub: the harness seeds `sessionStore` directly, because what is
 * under test is rendering from grants, not the bootstrap that fetches them. The
 * one API stub is `/v1/orgs/org1/settings` (in-memory), which the org AI-access
 * card reads on mount.
 */
let mcpEnabled = false;
const orgSettingsStub = {
  name: 'org-settings-stub',
  configureServer(server) {
    server.middlewares.use((req, res, next) => {
      const path = (req.url ?? '').split('?')[0];
      if (path !== '/v1/orgs/org1/settings') return next();
      res.setHeader('Content-Type', 'application/json');
      res.setHeader('Cache-Control', 'no-store');
      if (req.method === 'PATCH') {
        let body = '';
        req.on('data', (c) => (body += c));
        req.on('end', () => {
          const patch = JSON.parse(body || '{}');
          if (typeof patch.mcp_enabled === 'boolean') mcpEnabled = patch.mcp_enabled;
          res.end(JSON.stringify({ mcp_enabled: mcpEnabled }));
        });
        return;
      }
      res.end(JSON.stringify({ mcp_enabled: mcpEnabled }));
    });
  },
};

export default defineConfig({
  plugins: [svelte(), orgSettingsStub],
  root: fileURLToPath(new URL('./perm-harness', import.meta.url)),
  server: { port: 3041, strictPort: true },
});
