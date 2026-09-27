/**
 * Minimal ambient declarations for the Node builtins the TEST suite uses.
 *
 * This is a BROWSER SDK: `tsconfig.json` deliberately sets `"types": []` and a
 * DOM-only `lib`, so `@types/node` is not (and should not become) a dependency
 * just because the wire-fixture emitter has to write a file to disk. Only the
 * handful of members the tests actually touch are declared here — anything else
 * from `node:*` is intentionally a type error.
 */

declare const process: { execPath: string };

declare module 'node:child_process' {
  export function execFileSync(file: string, args: string[], options?: { stdio?: 'pipe' }): unknown;
}

declare module 'node:fs' {
  export function mkdirSync(path: string, options?: { recursive?: boolean }): void;
  export function mkdtempSync(prefix: string): string;
  export function readFileSync(path: string, encoding: 'utf8'): string;
  export function rmSync(path: string, options?: { recursive?: boolean; force?: boolean }): void;
  export function writeFileSync(path: string, data: string, encoding?: string): void;
}

declare module 'node:os' {
  export function tmpdir(): string;
}

declare module 'node:path' {
  export function dirname(path: string): string;
  export function join(...paths: string[]): string;
}

declare module 'node:url' {
  export function fileURLToPath(url: URL | string): string;
}

declare module 'node:vm' {
  export function createContext(sandbox: object): object;
  export function runInContext(code: string, context: object): unknown;
}

declare module 'node:zlib' {
  export function gunzipSync(data: Uint8Array): { toString(encoding?: string): string };
}
