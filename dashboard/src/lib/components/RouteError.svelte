<!--
  Shown when a lazily-imported route chunk cannot be downloaded.

  Before this existed, a failed chunk left `RouteLoading` on screen with no
  error and no way out: the router awaits the dynamic import, and an unhandled
  rejection simply never resolves it. A stuck spinner is indistinguishable from
  a slow network, so nobody reloads and nobody reports it.

  ## Reloading is the ONLY action, because it is the only one that does anything

  This used to also offer a secondary "Try again", justified as "a genuinely
  transient blip does recover". That justification was wrong, and counting
  requests at the server is what showed it: clicking "Try again" produced ZERO
  further network requests, both with the chunk still 404ing and with the chunk
  restored on the server. A specifier that failed is recorded in the module map
  AS FAILED, so re-`import()`ing it in the same document replays the cached
  rejection without touching the network — there is no such thing as a retry
  that recovers within this page load.

  A button that issues no request is worse than no button: it invites the user
  to conclude the page is genuinely gone rather than that Sauron needs
  reloading. So the affordance is a single action: it recovers when the
  chunk is fetchable again — restored at the same hash, or a fresh
  index.html naming hashes that exist — and does not when the chunk is
  genuinely gone at that hash.

  (Cache-busting the import specifier is not the missing alternative: a query
  string defeats Vite's static analysis of the import graph, which is what
  produces the code split. See `models/route-chunk.ts`.)

  ## Weight

  This component is on the critical path (statically imported by LazyRoute, which
  routes.ts imports), so its imports are paid on every first load by everyone.
  The house `Button` accounts for +15.3 kB raw / +4.0 kB gzip of the entry chunk
  on its own — it pulls `models/page-access` in for `lockTitle`. That is accepted
  rather than hand-rolling a `<button>`: the app's own failure state looking like
  the rest of the app is worth 4 kB, and it is 0.5% of the 743 kB the route split
  removed. Worth knowing before adding anything else here.
-->
<script lang="ts">
  import { t } from '../i18n';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  interface Props {
    /** Verbatim reason — Vite's message names the chunk, which identifies a stale deploy. */
    message: string;
  }

  let { message }: Props = $props();

  function reload() {
    // Reloads the document, which re-requests index.html and with it the
    // current chunk manifest. `location.reload()` rather than a cache-busted
    // URL so the address bar (and the #hash route the user is on) is untouched.
    window.location.reload();
  }
</script>

<!-- Laid out by ErrorState's conventions (hairline-circle glyph, --text-md
     title, muted reason, raw text as a faint "Details" line, one secondary
     action) without importing it: its Retry would be the dead button the note
     above explains, and this file's imports are paid on every first load. -->
<div class="route-error" role="alert">
  <div class="glyph" aria-hidden="true"><Icon name="triangle-alert" size={15} /></div>
  <h1 class="title">{t('ui.route.errorTitle')}</h1>
  <p class="reason">{t('prose.route.error')}</p>
  <p class="details"><span>{t('common.loadError.details')}:</span> <code>{message}</code></p>
  <div class="action">
    <Button variant="secondary" onclick={reload}>
      <Icon name="refresh" size={14} />
      {t('ui.route.reload')}
    </Button>
  </div>
</div>

<style>
  .route-error {
    min-height: 60vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
    padding: 40px 24px;
    text-align: center;
  }
  .glyph {
    width: 30px;
    height: 30px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    border: 1px solid color-mix(in srgb, var(--error) 35%, transparent);
    color: var(--error);
    margin-bottom: 8px;
  }
  .title {
    margin: 0;
    font-size: var(--text-md);
    font-weight: var(--weight-strong);
    color: var(--text);
  }
  .reason {
    margin: 0;
    max-width: 52ch;
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .details {
    margin: 0;
    max-width: 64ch;
    font-size: var(--text-xs);
    color: var(--text-faint);
    overflow-wrap: anywhere;
  }
  .details code {
    font-size: inherit;
  }
  .action {
    margin-top: 10px;
  }
</style>
