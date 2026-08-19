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
 * No API stub: the harness seeds `sessionStore` directly, because what is under
 * test is rendering from grants, not the bootstrap that fetches them.
 */
export default defineConfig({
  plugins: [svelte()],
  root: fileURLToPath(new URL('./perm-harness', import.meta.url)),
  server: { port: 3041, strictPort: true },
});
