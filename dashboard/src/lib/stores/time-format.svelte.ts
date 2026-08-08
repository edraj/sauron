export type TimeFormat = 'relative' | 'absolute';

const STORAGE_KEY = 'sauron.timeFormat';

function initialFormat(): TimeFormat {
  if (typeof window === 'undefined') return 'relative';
  const stored = window.localStorage.getItem(STORAGE_KEY);
  // Anything else — absent, corrupt, or written by an older build — falls back
  // rather than throwing. A bad preference must not break every timestamp in
  // the app.
  return stored === 'absolute' ? 'absolute' : 'relative';
}

class TimeFormatStore {
  /**
   * Relative ("3 minutes ago") or absolute ("2026-08-06 14:05:07").
   *
   * App-wide rather than per-instance: the intent is a mode — "I am
   * correlating timestamps right now" — not a property of one row. Toggling a
   * fifty-row table one cell at a time is not a feature.
   */
  mode = $state<TimeFormat>('relative');

  constructor() {
    this.mode = initialFormat();
  }

  set(next: TimeFormat): void {
    this.mode = next;
    if (typeof window !== 'undefined') {
      // Private-mode Safari throws on setItem with a full quota. The
      // preference is cosmetic; losing persistence must not break the click.
      try {
        window.localStorage.setItem(STORAGE_KEY, next);
      } catch {
        /* keep the in-memory value */
      }
    }
  }

  toggle(): void {
    this.set(this.mode === 'relative' ? 'absolute' : 'relative');
  }
}

export const timeFormatStore = new TimeFormatStore();
