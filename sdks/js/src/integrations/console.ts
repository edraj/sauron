import { addBreadcrumb } from '../api/breadcrumbs.js';
import type { Level } from '../types.js';
import { isInternal, isWrapped, markWrapped, registerPatch, withInternal } from './instrument.js';

type ConsoleMethod = 'log' | 'info' | 'warn' | 'error' | 'debug';
const METHODS: ConsoleMethod[] = ['log', 'info', 'warn', 'error', 'debug'];

function toLevel(method: ConsoleMethod): Level {
  switch (method) {
    case 'warn':
      return 'warning';
    case 'error':
      return 'error';
    case 'debug':
      return 'debug';
    default:
      return 'info';
  }
}

function argToString(arg: unknown): string {
  if (typeof arg === 'string') return arg;
  if (arg instanceof Error) return `${arg.name}: ${arg.message}`;
  try {
    return JSON.stringify(arg);
  } catch {
    return String(arg);
  }
}

/** Record a breadcrumb for each `console.*` call while leaving output intact. */
export function installConsole(): void {
  const consoleObj = (globalThis as { console?: Console }).console;
  if (!consoleObj) return;
  const c = consoleObj as unknown as Record<string, unknown>;

  for (const method of METHODS) {
    const original = c[method];
    if (typeof original !== 'function' || isWrapped(original)) continue;
    const originalFn = original as (...args: unknown[]) => unknown;

    const wrapped = markWrapped(function sauronConsole(this: unknown, ...args: unknown[]): unknown {
      if (!isInternal()) {
        withInternal(() => {
          try {
            addBreadcrumb({
              type: 'default',
              category: 'console',
              level: toLevel(method),
              message: args.map(argToString).join(' ').slice(0, 512),
              data: { arguments: args.length },
            });
          } catch {
            /* never break the app's console */
          }
        });
      }
      return originalFn.apply(this, args);
    });

    c[method] = wrapped;
    registerPatch(`console.${method}`, () => {
      c[method] = originalFn;
    });
  }
}
