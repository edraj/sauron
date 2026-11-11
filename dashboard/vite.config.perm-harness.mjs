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

/**
 * The Members page and the Account API-keys card, for the key-revocation
 * work: an in-memory roster whose `api_keys` summaries the bulk revoke
 * zeroes, and one member's key list with revoked rows behind
 * `?include_revoked=1`. Enough for both to render and act without the API.
 */
const daysAgo = (n) => new Date(Date.now() - n * 86_400_000).toISOString();
const grant = (id, user, email, name, role, keys) => ({
  id,
  user_id: user,
  email,
  name,
  role_id: `r-${role}`,
  role_name: role,
  scope_type: 'org',
  scope_id: 'org1',
  is_active: true,
  credentials_invalidated_at: null,
  pending_email_change: null,
  api_keys: keys,
});
const roster = [
  grant('g1', 'u-ada', 'ada@example.com', 'Ada Lovelace', 'Developer', { active: 2, revoked_by_password_reset_at: null }),
  grant('g2', 'u-ben', 'ben@example.com', 'Ben Ito', 'Developer', { active: 0, revoked_by_password_reset_at: daysAgo(2) }),
  grant('g3', 'u-cy', 'cy@example.com', 'Cy Park', 'Viewer', { active: 1, revoked_by_password_reset_at: null }),
  grant('g4', 'u-dee', 'dee@example.com', null, 'Viewer', { active: 0, revoked_by_password_reset_at: null }),
];
const myKeys = [
  { id: 'k1', name: 'laptop', prefix: 'sauron_pat_000001', created_at: daysAgo(1), expires_at: daysAgo(-89), last_used_at: daysAgo(0), expired: false, revoked_at: null, revoked_reason: null },
];
const myRevoked = [
  { id: 'k2', name: 'old laptop', prefix: 'sauron_pat_000002', created_at: daysAgo(40), expires_at: null, last_used_at: daysAgo(3), expired: false, revoked_at: daysAgo(2), revoked_reason: 'password_reset' },
  { id: 'k3', name: 'ci', prefix: 'sauron_pat_000003', created_at: daysAgo(60), expires_at: daysAgo(-30), last_used_at: null, expired: false, revoked_at: daysAgo(20), revoked_reason: 'admin_revoked' },
  { id: 'k4', name: 'scratch', prefix: 'sauron_pat_000004', created_at: daysAgo(90), expires_at: null, last_used_at: null, expired: false, revoked_at: daysAgo(80), revoked_reason: 'user_revoked' },
];
const membersStub = {
  name: 'members-and-keys-stub',
  configureServer(server) {
    server.middlewares.use((req, res, next) => {
      const [path, query = ''] = (req.url ?? '').split('?');
      const json = (status, body) => {
        res.statusCode = status;
        res.setHeader('Content-Type', 'application/json');
        res.setHeader('Cache-Control', 'no-store');
        res.end(JSON.stringify(body));
      };
      if (path === '/v1/orgs/org1/members' && req.method === 'GET') return json(200, roster);
      if (path === '/v1/orgs/org1/roles' && req.method === 'GET') return json(200, []);
      if (path === '/v1/me/api-keys' && req.method === 'GET') {
        const revoked = /(^|&)include_revoked=(1|true)/.test(query);
        return json(200, revoked ? [...myKeys, ...myRevoked] : myKeys);
      }
      if (path === '/v1/orgs/org1/members/revoke-api-keys' && req.method === 'POST') {
        let body = '';
        req.on('data', (c) => (body += c));
        req.on('end', () => {
          const ids = new Set(JSON.parse(body || '{}').user_ids ?? []);
          const members = [];
          let revoked = 0;
          for (const g of roster) {
            if (!ids.has(g.user_id)) continue;
            members.push({ user_id: g.user_id, revoked: g.api_keys.active });
            revoked += g.api_keys.active;
            g.api_keys = { ...g.api_keys, active: 0 };
          }
          json(200, { ok: true, revoked, members });
        });
        return;
      }
      next();
    });
  },
};

export default defineConfig({
  plugins: [svelte(), orgSettingsStub, membersStub],
  root: fileURLToPath(new URL('./perm-harness', import.meta.url)),
  server: { port: 3041, strictPort: true },
});
