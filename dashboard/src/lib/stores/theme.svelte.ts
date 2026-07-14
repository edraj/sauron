export type Theme = 'dark' | 'light';

const STORAGE_KEY = 'sauron.theme';

function initialTheme(): Theme {
  if (typeof window === 'undefined') return 'dark';
  const stored = window.localStorage.getItem(STORAGE_KEY);
  if (stored === 'dark' || stored === 'light') return stored;
  const prefersLight = window.matchMedia?.('(prefers-color-scheme: light)').matches;
  return prefersLight ? 'light' : 'dark';
}

class ThemeStore {
  theme = $state<Theme>('dark');

  constructor() {
    this.theme = initialTheme();
    this.apply();
  }

  private apply(): void {
    if (typeof document !== 'undefined') {
      document.documentElement.setAttribute('data-theme', this.theme);
    }
  }

  set(next: Theme): void {
    this.theme = next;
    if (typeof window !== 'undefined') {
      window.localStorage.setItem(STORAGE_KEY, next);
    }
    this.apply();
  }

  toggle(): void {
    this.set(this.theme === 'dark' ? 'light' : 'dark');
  }
}

export const themeStore = new ThemeStore();
