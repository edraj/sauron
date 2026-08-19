import type { Permission } from '../models';
import { lockTitle } from '../models/page-access';

/**
 * Marks a control as locked by a missing permission: announces it, explains it,
 * and neutralises it — on any element, from a one-line call site.
 *
 * ```svelte
 * <button use:lockTip={manageLock} onclick={remove}>Remove</button>
 * ```
 *
 * Replaces the `disabled={lock !== null}` + `title={lockTitle(lock)}` pair that
 * this codebase used at every raw-element lock. That pair defeats its own
 * purpose: `disabled` removes the control from the tab order (HTML spec), so
 * keyboard and screen reader users — the people most dependent on an explicit
 * reason — could never focus it to receive one, and `title` never appears on
 * touch at all.
 *
 * Instead the element keeps its place in the tab order, reports
 * `aria-disabled`, and describes itself through a tooltip shown on hover AND
 * focus, dismissed on Escape (WCAG 1.4.13).
 *
 * `aria-disabled` prevents nothing on its own, so activation is suppressed
 * here, in the CAPTURE phase — that is what stops the element's own `onclick`
 * from running without every call site having to guard it, and
 * `preventDefault` is what stops a `type="submit"` control from submitting its
 * form, including via Enter on a focused button.
 *
 * Pass `null` when the user may act: the action then does nothing at all, so a
 * call site passes its lock straight through with no ternary.
 */
export function lockTip(node: HTMLElement, reason: Permission | null) {
  let bubble: HTMLDivElement | null = null;
  let current = reason;

  function position(): boolean {
    if (!bubble) return false;
    const r = node.getBoundingClientRect();
    // A tooltip for something the user cannot see has nothing to point at.
    if (r.bottom < 0 || r.top > window.innerHeight || r.right < 0 || r.left > window.innerWidth) {
      return false;
    }
    bubble.style.top = `${r.bottom + 6}px`;
    bubble.style.left = `${r.left + r.width / 2}px`;
    return true;
  }

  function show(): void {
    if (!current || bubble) return;
    bubble = document.createElement('div');
    bubble.className = 'lock-tip';
    bubble.setAttribute('role', 'tooltip');
    bubble.id = `lock-tip-${++counter}`;
    bubble.textContent = lockTitle(current);
    document.body.appendChild(bubble);
    if (!position()) {
      hide();
      return;
    }
    // Set only once the bubble is really in the document: an
    // `aria-describedby` naming an element that was never rendered is a
    // dangling reference, which is worse than no description at all.
    node.setAttribute('aria-describedby', bubble.id);
    window.addEventListener('scroll', reposition, true);
    window.addEventListener('resize', reposition);
    document.addEventListener('keydown', onkey);
  }

  function hide(): void {
    if (!bubble) return;
    bubble.remove();
    bubble = null;
    node.removeAttribute('aria-describedby');
    window.removeEventListener('scroll', reposition, true);
    window.removeEventListener('resize', reposition);
    document.removeEventListener('keydown', onkey);
  }

  // Re-anchored rather than dismissed. Dismissing looks right for a mouse but
  // breaks the keyboard path: focusing an element inside a scroll container
  // makes the browser scroll it into view, and that scroll would dismiss the
  // tooltip the focus had just opened.
  function reposition(): void {
    if (!position()) hide();
  }

  function onkey(e: KeyboardEvent): void {
    if (e.key === 'Escape') hide();
  }

  function suppress(e: Event): void {
    if (!current) return;
    e.preventDefault();
    e.stopImmediatePropagation();
  }

  function apply(next: Permission | null): void {
    current = next;
    if (next) {
      node.setAttribute('aria-disabled', 'true');
      node.classList.add('is-locked');
    } else {
      node.removeAttribute('aria-disabled');
      node.classList.remove('is-locked');
      hide();
    }
  }

  apply(reason);
  node.addEventListener('click', suppress, true);
  node.addEventListener('mouseenter', show);
  node.addEventListener('mouseleave', hide);
  node.addEventListener('focus', show);
  node.addEventListener('blur', hide);

  return {
    update(next: Permission | null) {
      apply(next);
    },
    destroy() {
      hide();
      node.removeEventListener('click', suppress, true);
      node.removeEventListener('mouseenter', show);
      node.removeEventListener('mouseleave', hide);
      node.removeEventListener('focus', show);
      node.removeEventListener('blur', hide);
    },
  };
}

let counter = 0;
