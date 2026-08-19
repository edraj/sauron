import { isLocale, isRtl, intlTag, type Locale } from './types';

const STORAGE_KEY = 'sauron.locale';

function initialLocale(): Locale {
  if (typeof window === 'undefined') return 'en';
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY);
    if (isLocale(stored)) return stored;
  } catch {
    /* storage unavailable — fall through to the browser's preference */
  }
  // No stored choice yet: honour what the browser asks for before defaulting.
  // `navigator.language` is a full tag ("ar-DZ", "ar-EG"), so match the prefix.
  const preferred = window.navigator?.language ?? '';
  return preferred.toLowerCase().startsWith('ar') ? 'ar' : 'en';
}

class LocaleStore {
  /**
   * The active UI language.
   *
   * App-wide rather than per-page, and persisted, so a reload does not drop
   * the user back into English. `$state` is what makes every `t()` call in
   * markup reactive without a subscription of its own.
   */
  locale = $state<Locale>('en');

  constructor() {
    this.locale = initialLocale();
    this.apply();
  }

  /** Whether the active locale reads right-to-left. */
  get rtl(): boolean {
    return isRtl(this.locale);
  }

  /** The BCP-47 tag for `Intl`, with Arabic's numbering system pinned. */
  get tag(): string {
    return intlTag(this.locale);
  }

  /**
   * Push the locale onto `<html>`.
   *
   * `lang` drives font selection, hyphenation, and screen-reader pronunciation;
   * `dir` is what actually flips the layout. Both belong on the root element so
   * that portalled content — modals, toasts, dropdowns rendered outside the app
   * subtree — inherits them too.
   */
  private apply(): void {
    if (typeof document === 'undefined') return;
    const root = document.documentElement;
    root.setAttribute('lang', this.locale);
    root.setAttribute('dir', isRtl(this.locale) ? 'rtl' : 'ltr');
  }

  set(next: Locale): void {
    this.locale = next;
    if (typeof window !== 'undefined') {
      // Private-mode Safari throws on setItem against a full quota. Losing
      // persistence must not break the switch itself.
      try {
        window.localStorage.setItem(STORAGE_KEY, next);
      } catch {
        /* keep the in-memory value */
      }
    }
    this.apply();
  }
}

export const localeStore = new LocaleStore();
