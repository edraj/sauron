<!--
  Placeholder shown while a lazily-imported route chunk is in flight.

  Not cosmetic. Without a loading state the app renders a BLANK page for the
  duration of the chunk fetch — on a cold cache over mobile, a visible white
  flash on every first visit to a page. Mounted by LazyRoute.svelte.

  It carries TEXT, deliberately. A text-free spinner is unreadable to a screen
  reader beyond "Loading", and — more importantly — it is indistinguishable from
  a wedged app. A stuck state has to at least be legible: `aria-live="polite"`
  announces it, and the visible label plus the reassurance line tell a user
  staring at it what is supposedly happening. (The wedge itself is fixed by
  LazyRoute/RouteError; this is the part that makes a slow load legible.)
-->
<script lang="ts">
  import { t } from '../i18n';
  import Spinner from './ui/Spinner.svelte';
</script>

<div class="route-loading" role="status" aria-live="polite">
  <!-- `aria-hidden` on the wrapper: Spinner carries its own role="status", and
       two nested live regions announce the same event twice. The text below is
       the one that should be read. -->
  <span aria-hidden="true"><Spinner size={22} /></span>
  <p class="label">{t('ui.route.loading')}</p>
</div>

<style>
  .route-loading {
    min-height: 60vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
  }
  .label {
    margin: 0;
    font-size: 13px;
    color: var(--text-muted);
  }
</style>
