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
  reloading. So the affordance is one primary action: it recovers when the
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

<div class="route-error" role="alert">
  <Icon name="triangle-alert" size={26} />
  <h1>This page didn’t load</h1>
  <p class="lede">
    Sauron loads each page on demand and this one’s code could not be downloaded. If Sauron was
    updated while this tab was open, reloading picks up the new version.
  </p>
  <p class="detail"><code>{message}</code></p>
  <div class="actions">
    <Button variant="primary" onclick={reload}>
      <Icon name="refresh" size={14} />
      Reload Sauron
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
    gap: 12px;
    padding: 24px;
    text-align: center;
    color: var(--error);
  }
  h1 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
    color: var(--text);
  }
  .lede {
    margin: 0;
    max-width: 46ch;
    font-size: 13px;
    color: var(--text-muted);
  }
  .detail {
    margin: 0;
    max-width: 60ch;
  }
  .detail code {
    font-size: 12px;
    word-break: break-all;
    color: var(--error);
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 38%, transparent);
    border-radius: var(--radius);
    padding: 6px 10px;
    display: inline-block;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: center;
    margin-top: 4px;
  }
</style>
