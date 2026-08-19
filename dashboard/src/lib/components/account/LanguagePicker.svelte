<!--
  The dashboard's language switcher.

  A segmented control rather than a `<select>`: there are exactly two options,
  both are worth showing at once, and each label has to render in its own
  script — "العربية" is how a reader who has landed in the wrong language finds
  their way back, which a collapsed select hides behind a click.

  The switch takes effect immediately and is not confirmed. It is reversible in
  one click, and the control itself is the clearest possible undo — the button
  the user just left is still sitting there, now labelled in a script they can
  read.
-->
<script lang="ts">
  import { localeStore, LOCALES, LOCALE_LABELS, t, type Locale } from '../../i18n';

  const active = $derived(localeStore.locale);

  function choose(next: Locale): void {
    if (next !== localeStore.locale) localeStore.set(next);
  }
</script>

<div class="lang">
  <div class="seg" role="group" aria-label={t('account.language.label')}>
    {#each LOCALES as code (code)}
      <button
        type="button"
        class="opt"
        class:on={active === code}
        lang={code}
        aria-pressed={active === code}
        title={t('account.language.switchTo', { language: LOCALE_LABELS[code] })}
        onclick={() => choose(code)}
      >
        {LOCALE_LABELS[code]}
      </button>
    {/each}
  </div>
  <p class="hint faint">{t('account.language.hint')}</p>
</div>

<style>
  .lang {
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: flex-start;
  }

  .seg {
    display: inline-flex;
    padding: 3px;
    gap: 2px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .opt {
    padding: 5px 14px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    font-weight: 550;
    line-height: 1.4;
    transition: background 0.12s ease, color 0.12s ease;
  }

  .opt:hover:not(.on) {
    color: var(--text);
    background: var(--surface-3);
  }

  .opt.on {
    background: var(--primary);
    color: var(--primary-contrast);
  }

  .hint {
    font-size: 12px;
    margin: 0;
  }
</style>
