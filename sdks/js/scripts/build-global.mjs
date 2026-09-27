/**
 * Script-tag builds: the two files a page can load without a bundler. Both run
 * `src/global.ts`, which sets `window.Sauron`.
 *
 *   dist/sauron.min.js      ES2020, minified, with a source map. What
 *                           jsDelivr/unpkg serve for the bare package URL.
 *   dist/sauron.es5.min.js  ES5 syntax, minified. For pasting inline into a
 *                           host that validates scripts as ES5 — a Google Tag
 *                           Manager Custom HTML tag rejects arrow functions,
 *                           classes, `const`, ...
 *
 * Runs after tsup (`npm run build`), which builds the ESM/CJS entry points but
 * can't build the ES5 file: its `target: 'es5'` path runs SWC over the finished
 * IIFE, which puts SWC's helper functions at the top level of the file. In a
 * classic script that is `window` — measured: 32 one-letter globals (`t`, `e`,
 * `_`, ...), and `window._` is lodash/underscore on the pages that load them.
 * Here the ES5 output is wrapped in a function before it is minified, so
 * `Sauron` is the only global either file creates.
 *
 * Usage: node scripts/build-global.mjs [--outdir <dir>]   (default: dist)
 */

import { minify, transform } from '@swc/core';
import { parse } from 'acorn';
import { build } from 'esbuild';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { gzipSync } from 'node:zlib';

const pkgDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const pkg = JSON.parse(await readFile(join(pkgDir, 'package.json'), 'utf8'));
const { values } = parseArgs({ options: { outdir: { type: 'string', default: 'dist' } } });
const outdir = resolve(pkgDir, values.outdir);

const banner = `/*! ${pkg.name} v${pkg.version} | ${pkg.license} | ${pkg.homepage} */`;

/**
 * `compress.ts` loads its gzip fallback with `await import('fflate')`. The
 * namespace a dynamic import returns can't be tree-shaken, so all of fflate
 * (zip, inflate, streams: 32 KB, half the file) came along for `gzipSync`.
 * Resolve that import to a module that re-exports only `gzipSync` (6.5 KB).
 */
const fflateGzipOnly = {
  name: 'fflate-gzip-only',
  setup(b) {
    b.onResolve({ filter: /^fflate$/ }, (args) =>
      // The re-export's own import of fflate resolves normally.
      args.namespace === 'fflate-gzip-only' ? undefined : { path: 'fflate', namespace: 'fflate-gzip-only' },
    );
    b.onLoad({ filter: /.*/, namespace: 'fflate-gzip-only' }, () => ({
      contents: "export { gzipSync } from 'fflate';",
      resolveDir: pkgDir,
    }));
  },
};

/** Bundle src/global.ts into one ES2020 IIFE. */
async function bundle({ minified }) {
  const result = await build({
    absWorkingDir: pkgDir,
    entryPoints: ['src/global.ts'],
    outfile: join(outdir, 'sauron.min.js'),
    bundle: true,
    format: 'iife',
    platform: 'browser',
    target: 'es2020',
    minify: minified,
    sourcemap: minified ? 'linked' : false,
    banner: minified ? { js: banner } : undefined,
    plugins: [fflateGzipOnly],
    write: false,
    logLevel: 'warning',
  });
  return result.outputFiles;
}

/**
 * Both files are also pasted inline, so they must survive being embedded in
 * HTML and in a GTM Custom HTML tag.
 */
function assertInlineSafe(name, code) {
  // GTM reads `{{...}}` in a Custom HTML tag as a variable reference.
  if (code.includes('{{')) throw new Error(`${name}: contains "{{" (a GTM variable reference)`);
  // Inside <script>...</script> the HTML parser ends the element at the first
  // "</script", and "<!--" switches it into escaped-script parsing.
  const html = /<\/script|<!--/i.exec(code);
  if (html) throw new Error(`${name}: contains "${html[0]}", which breaks an inline <script>`);
}

await mkdir(outdir, { recursive: true });

// 1. ES2020 — esbuild alone.
const written = [];
for (const file of await bundle({ minified: true })) {
  await writeFile(file.path, file.contents);
  written.push(file.path);
}
const modern = await readFile(join(outdir, 'sauron.min.js'), 'utf8');
assertInlineSafe('sauron.min.js', modern);

// 2. ES5 — the unminified bundle through SWC, wrapped, then minified. The
//    wrapper is what keeps SWC's helpers off `window`.
const [unminified] = await bundle({ minified: false });
const lowered = await transform(unminified.text, {
  filename: 'sauron.es5.js',
  isModule: false,
  swcrc: false,
  configFile: false,
  jsc: { target: 'es5', parser: { syntax: 'ecmascript' } },
});
const minifiedEs5 = await minify(`(function () {\n${lowered.code}\n})();\n`, {
  compress: true,
  mangle: true,
  ecma: 5,
  module: false,
  format: { comments: false },
});
const es5 = `${banner}\n${minifiedEs5.code}\n`;
// Throws on anything newer than ES5, with the offending line and column.
parse(es5, { ecmaVersion: 5, sourceType: 'script' });
assertInlineSafe('sauron.es5.min.js', es5);
const es5Path = join(outdir, 'sauron.es5.min.js');
await writeFile(es5Path, es5);
written.push(es5Path);

for (const path of written) {
  if (path.endsWith('.map')) continue;
  const bytes = await readFile(path);
  const kb = (n) => `${(n / 1024).toFixed(1)} KB`;
  console.log(`${relative(process.cwd(), path)}  ${kb(bytes.length)} (${kb(gzipSync(bytes).length)} gzip)`);
}
