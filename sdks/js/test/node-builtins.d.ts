/**
 * Minimal ambient declarations for the Node builtins the TEST suite uses.
 *
 * This is a BROWSER SDK: `tsconfig.json` deliberately sets `"types": []` and a
 * DOM-only `lib`, so `@types/node` is not (and should not become) a dependency
 * just because the wire-fixture emitter has to write a file to disk. Only the
 * handful of members the tests actually touch are declared here — anything else
 * from `node:*` is intentionally a type error.
 */

declare module 'node:fs' {
  export function mkdirSync(path: string, options?: { recursive?: boolean }): void;
  export function writeFileSync(path: string, data: string, encoding?: string): void;
}

declare module 'node:path' {
  export function dirname(path: string): string;
}

declare module 'node:url' {
  export function fileURLToPath(url: URL | string): string;
}

declare module 'node:zlib' {
  export function gunzipSync(data: Uint8Array): { toString(encoding?: string): string };
}
