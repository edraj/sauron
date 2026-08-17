<script lang="ts">
  import BreadcrumbTrail from '../src/lib/components/BreadcrumbTrail.svelte';
  import type { Breadcrumb } from '../src/lib/models';

  function at(s: number): string {
    return new Date(Date.UTC(2026, 7, 17, 10, 0, s)).toISOString();
  }

  /** A browser SDK crumb: no message, direction plus `{from, to}` in data. */
  function web(s: number, from: string, to: string, operation: string): Breadcrumb {
    return {
      type: 'navigation',
      category: 'history',
      message: null,
      level: 'info',
      timestamp: at(s),
      data: { from, to, operation },
    };
  }

  /** A Flutter SDK crumb: the route name is the message, direction in data. */
  function flutter(s: number, route: string, operation: string): Breadcrumb {
    return {
      type: 'navigation',
      category: 'route',
      message: route,
      level: 'info',
      timestamp: at(s),
      data: { operation },
    };
  }

  function other(s: number, type: string, category: string, level: string, message: string): Breadcrumb {
    return { type, category, message, level, timestamp: at(s), data: null };
  }

  const FIXTURES: Record<string, { title: string; note: string; crumbs: Breadcrumb[] }> = {
    web: {
      title: 'Browser SDK — the three operations the web can emit',
      note: 'push → arrow-right, replace → refresh, pop → arrow-left. A forward step is a pop too (last row).',
      crumbs: [
        web(1, '/', '/catalogue', 'push'),
        web(4, '/catalogue', '/catalogue?sort=price', 'replace'),
        web(9, '/catalogue?sort=price', '/checkout', 'push'),
        web(14, '/checkout', '/catalogue?sort=price', 'pop'),
        web(18, '/catalogue?sort=price', '/checkout', 'pop'),
      ],
    },
    flutter: {
      title: 'Flutter SDK — all four, including remove',
      note: 'The route name comes from `message`; `operation: …` no longer repeats as text below it.',
      crumbs: [
        flutter(1, '/home', 'push'),
        flutter(5, '/checkout', 'push'),
        flutter(11, '/checkout', 'pop'),
        flutter(15, '/home', 'replace'),
        flutter(20, '/login', 'remove'),
      ],
    },
    mixed: {
      title: 'Mixed trail — the rail must stay straight',
      note: 'Glyph rows and dot rows alternate. The connecting line runs behind both and must not zigzag or break.',
      crumbs: [
        other(0, 'default', 'console', 'debug', 'app booted'),
        web(2, '/', '/catalogue', 'push'),
        other(4, 'http', 'fetch', 'info', 'GET /api/products → 200'),
        web(7, '/catalogue', '/checkout', 'push'),
        other(9, 'ui.click', 'ui', 'info', 'button#pay'),
        other(12, 'http', 'fetch', 'error', 'POST /api/checkout → 500'),
        web(14, '/checkout', '/catalogue', 'pop'),
        other(16, 'default', 'console', 'warning', 'retrying'),
      ],
    },
    fallback: {
      title: 'No direction to show — must fall back to the level dot',
      note: 'Row 1: an older SDK sent no operation. Row 2: a value outside the vocabulary, which must still print as text so nothing is swallowed. Row 3: level `error` keeps its red dot.',
      crumbs: [
        {
          type: 'navigation',
          category: 'history',
          message: null,
          level: 'info',
          timestamp: at(1),
          data: { from: '/', to: '/settings' },
        },
        {
          type: 'navigation',
          category: 'history',
          message: null,
          level: 'warning',
          timestamp: at(5),
          data: { from: '/settings', to: '/x', operation: 'teleport' },
        },
        other(9, 'error', 'exception', 'error', 'TypeError: undefined is not a function'),
      ],
    },
  };

  const keys = Object.keys(FIXTURES);
  let active = $state(keys[0]);
</script>

<main>
  <h1>BreadcrumbTrail — navigation direction</h1>
  <nav>
    {#each keys as key (key)}
      <button class:on={active === key} onclick={() => (active = key)}>{key}</button>
    {/each}
  </nav>

  <h2>{FIXTURES[active].title}</h2>
  <p class="note">{FIXTURES[active].note}</p>

  <section class="surface">
    <BreadcrumbTrail breadcrumbs={FIXTURES[active].crumbs} />
  </section>
</main>

<style>
  main {
    max-width: 720px;
    margin: 0 auto;
    padding: 32px 24px 64px;
    color: var(--text);
    font-family: var(--font-sans, system-ui, sans-serif);
  }
  h1 {
    font-size: 18px;
    margin: 0 0 16px;
  }
  h2 {
    font-size: 14px;
    margin: 24px 0 4px;
  }
  .note {
    font-size: 12.5px;
    color: var(--text-muted);
    margin: 0 0 16px;
  }
  nav {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  button {
    font: inherit;
    font-size: 12.5px;
    padding: 4px 10px;
    border-radius: var(--radius-pill, 999px);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text-muted);
    cursor: pointer;
  }
  button.on {
    background: var(--surface);
    color: var(--text);
    border-color: var(--text-faint);
  }
  /* The glyph punches the connecting line with `background: var(--surface)`,
     so the trail must sit on a real surface for that hole to be invisible. */
  .surface {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius, 8px);
    padding: 16px;
  }
</style>
