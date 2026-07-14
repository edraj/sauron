import { addBreadcrumb } from '../api/breadcrumbs.js';
import { isInternal, registerPatch, withInternal } from './instrument.js';

/**
 * Build a stable CSS-ish selector `tag#id.class` for a clicked element.
 * NEVER serializes `innerText`/attribute values — that would leak PII.
 */
export function domSelector(el: unknown): string | null {
  try {
    const node = el as { tagName?: string; id?: string; className?: unknown } | null;
    if (!node || typeof node.tagName !== 'string') return null;

    let selector = node.tagName.toLowerCase();
    if (node.id && typeof node.id === 'string') {
      selector += `#${node.id}`;
    }
    if (typeof node.className === 'string' && node.className.trim()) {
      const classes = node.className.trim().split(/\s+/).slice(0, 3);
      if (classes.length) selector += `.${classes.join('.')}`;
    }
    return selector;
  } catch {
    return null;
  }
}

/** Record a `ui.click` breadcrumb (selector only, never text) for each click. */
export function installDom(): void {
  const doc = (globalThis as { document?: Document }).document;
  if (!doc || typeof doc.addEventListener !== 'function') return;

  const handler = (event: Event): void => {
    if (isInternal()) return;
    const selector = domSelector(event.target);
    if (!selector) return;
    withInternal(() => {
      addBreadcrumb({
        type: 'default',
        category: 'ui.click',
        level: 'info',
        message: selector,
        data: null,
      });
    });
  };

  const options = { capture: true, passive: true } as AddEventListenerOptions;
  doc.addEventListener('click', handler, options);
  registerPatch('dom.click', () => {
    try {
      doc.removeEventListener('click', handler, options);
    } catch {
      /* ignore */
    }
  });
}
