<!--
  Drives `DateRange` + `DateRangePicker` directly, with the emitted value
  printed as JSON beside it.

  The JSON is the point: a calendar that LOOKS right and emits the wrong
  interval is the failure mode here, and only reading the value back catches it.
  Every assertion the browser pass makes is against `#emitted`, not against
  pixels.
-->
<script lang="ts">
  import DateRange from '../src/lib/components/DateRange.svelte';
  import { lastDays, spanDays, type DateRangeValue } from '../src/lib/models/date-range';
  import { localeStore } from '../src/lib/i18n';

  let value = $state<DateRangeValue>(lastDays(30));
  let rtl = $state(false);

  $effect(() => {
    document.documentElement.dir = rtl ? 'rtl' : 'ltr';
  });
</script>

<main dir={rtl ? 'rtl' : 'ltr'}>
  <h1>Custom date range harness</h1>

  <div class="row">
    <button id="toggle-rtl" type="button" onclick={() => (rtl = !rtl)}>
      dir = {rtl ? 'rtl' : 'ltr'}
    </button>
    <button
      id="toggle-locale"
      type="button"
      onclick={() => localeStore.set(localeStore.locale === 'en' ? 'ar' : 'en')}
    >
      locale = {localeStore.locale}
    </button>
  </div>

  <div class="row">
    <DateRange {value} onchange={(v) => (value = v)} />
  </div>

  <pre id="emitted">{JSON.stringify(value)}</pre>
  <pre id="span">{spanDays(value)}</pre>
</main>

<style>
  main {
    padding: 24px;
    font-family: system-ui, sans-serif;
  }
  .row {
    margin-bottom: 16px;
    display: flex;
    gap: 8px;
    align-items: center;
  }
  pre {
    background: var(--surface-2);
    padding: 8px;
    border-radius: 6px;
    font-size: 12px;
  }
</style>
