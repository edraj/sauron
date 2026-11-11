# Sauron Landing Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a pre-rendered, single-page Sauron landing site in a new `web/` git submodule. It uses the "Eye of fire" look and shows real ACME-org dashboard screenshots in the layered "C" style.

**Architecture:**
- SvelteKit 2 with `adapter-static` pre-renders one route (`/`) to plain HTML.
- All copy, links and code snippets live in one typed content module. Components only render it, and parity tests pin that module against the main repo's `INSTALL.md` and SDK READMEs.
- Screenshots come from a reproducible Node script that seeds an ACME org into a local Sauron stack and captures 8 dashboard pages with headless Chromium.

**Tech Stack:**
- SvelteKit 2.70.3, `@sveltejs/adapter-static` 3.0.10, `@sveltejs/vite-plugin-svelte` 6.2.4, Vite 7.3.7, Svelte 5.57.3 (runes)
- Tailwind CSS 4.3.3 (`@tailwindcss/vite`), TypeScript 5.9.3
- Vitest 3.2.7 + jsdom, `@fontsource-variable/inter` 5.3.0
- `playwright-core` (already in the yarn cache) driving `/usr/bin/chromium-browser`

All versions are exact pins, each published at least 2 weeks ago.

**Spec:** `docs/superpowers/specs/2026-11-11-landing-page-design.md`

## Global Constraints

- **NEVER commit, in either repo, and never create branches or `git stash`.** The owner commits.
  Each task ends with a "leave uncommitted" check instead of a commit step.
- **No link or text may reference the wiki.** Install help links to
  `https://github.com/edraj/sauron/blob/main/INSTALL.md`.
- The star/repo link is exactly `https://github.com/edraj/sauron`.
- **No invented numbers:** no star counts, user counts or perf figures.
- **Never name Sentry or PostHog** on the page.
- **Screenshots may not contain** "Zain", "Soheyb", "Merah", `@iq.zain.com`, or any DSN public key.
- **Dark only.** Background `#0c0a09`, surface `#120e0b`/`#15110e`, hairline `#2a1d15`, text
  `#f5f0eb`, muted `#a8a29e`.
- Fire gradient stops: `#fff7d6 #ffd451 #ff8f22 #f14e0b #a81a03`.
- **Headline:** "See every error through your users' eyes." The fire gradient goes on "through
  your users' eyes."
- **Sub:** "Error tracking and product analytics in one self-hosted stack. Every crash lands on the timeline of the person who hit it."
- **Fine print:** "Open source · AGPL-3.0 · Rust + Postgres + Redis". SDKs are LGPL-3.0.
- **C# SDK is not on NuGet.** Its tab must show the project-reference install from
  `sdks/csharp/README.md`, not `dotnet add package Sauron`.
- **Base path** comes from the `BASE_PATH` env var at build time (default `''`).
- **No deploy workflow.**
- **Layout:** no horizontal page scroll at 390px. All motion is off under
  `prefers-reduced-motion: reduce`.

## Review Focus

1. **Deep load under a base path:** `BASE_PATH=/sauron-web npm run build` must produce asset,
   image and anchor URLs that all carry `/sauron-web`. A root-relative `/screenshots/x.webp`
   404s on GitHub Pages. Pinned in Task 1 and Task 4.
2. **Phone width (390px):** layered windows must collapse to the front window only. Wide code
   blocks scroll inside their own box, not the page. Pinned in Task 6.
3. **JavaScript disabled or still hydrating:** pre-rendered HTML must already contain every
   section's copy and the default tab's code, because tabs are progressive. Pinned in Task 4.
4. **Copy button without clipboard permission** (http, or the permission denied): it must not
   throw. It shows "Copy failed" and leaves the text selectable. Pinned in Task 3.
5. **Drift from the main repo:** if `INSTALL.md` or an SDK README changes its commands, a parity
   test fails. It must not pass silently when the parent repo files are absent: it reports a
   *skip* with the reason. Pinned in Task 2.

---

### Task 1: Submodule and SvelteKit/Tailwind scaffold

**Files:**
- Modify (staged by git, uncommitted): `.gitmodules`, gitlink `web`
- Create:
  - `web/package.json`, `web/svelte.config.js`, `web/vite.config.ts`, `web/tsconfig.json`
  - `web/src/app.html`, `web/src/app.css`, `web/src/routes/+layout.ts`,
    `web/src/routes/+layout.svelte`, `web/src/routes/+page.svelte` (placeholder)
  - `web/.gitignore`, `web/.nvmrc` (`22`)
- Test: `web/tests/build.test.ts`

**Interfaces:**
- Produces:
  - `npm run dev|build|preview|check|test` in `web/`
  - Tailwind theme tokens in `app.css` under `@theme`: `--color-ink: #0c0a09`,
    `--color-surface: #120e0b`, `--color-surface-2: #15110e`, `--color-line: #2a1d15`,
    `--color-fg: #f5f0eb`, `--color-muted: #a8a29e`, `--color-fire-1..5` (the five gradient
    stops), `--font-sans: 'Inter Variable', ui-sans-serif, system-ui, sans-serif`
  - Utility class `.text-fire`: gradient text, `#ffd451 → #f14e0b`
  - `export const prerender = true` and `export const trailingSlash = 'always'` in `+layout.ts`

- [ ] **Step 1:** Add the submodule:
  `git submodule add https://github.com/splimter/sauron-web.git web`.
  - Expected: `.gitmodules` lists `path = web` with the HTTPS URL, and `web/README.md` exists.
  - Run `git status --short` and confirm only `.gitmodules` and `web` are new or staged.
- [ ] **Step 2:** Write `package.json` with the exact versions from Tech Stack, plus `jsdom`
  (latest version at least 2 weeks old) and `"type": "module"`. Scripts:
  - `build: "vite build"`, `check: "svelte-kit sync && svelte-check --tsconfig ./tsconfig.json"`
  - `test: "vitest run"`, `shots: "node scripts/screenshots/capture.mjs"`
  - Then run `npm install` in `web/`.
- [ ] **Step 3: Write the failing test** `tests/build.test.ts`. It runs `vite build` twice via
  `execSync`, with env `BASE_PATH=''` and then `BASE_PATH='/sauron-web'`. Assertions:
  - `build/index.html` exists and contains `<html lang="en"`.
  - With the base path set, every `src="` and `href="` that starts with `/` in
    `build/index.html` starts with `/sauron-web/`.
- [ ] **Step 4:** Run `npx vitest run tests/build.test.ts`. Expected: FAIL (no config yet).
- [ ] **Step 5:** Implement the config.
  - `svelte.config.js`: `adapter-static({ fallback: undefined })` and
    `paths.base = process.env.BASE_PATH ?? ''`.
  - `vite.config.ts`: `tailwindcss()` before `sveltekit()`; vitest `environment: 'jsdom'`.
  - `app.css`: `@import 'tailwindcss'`, the `@theme` tokens above, body background
    `var(--color-ink)`, `color-scheme: dark`.
  - Import `@fontsource-variable/inter` in `+layout.svelte`.
- [ ] **Step 6:** Run `npx vitest run tests/build.test.ts`. Expected: PASS.
- [ ] **Step 7:** Run `npm run check`. Expected: 0 errors.
- [ ] **Step 8:** Leave uncommitted. `git -C web status` shows the new files; nothing is
  committed in either repo.

### Task 2: Content module and parity tests

**Files:**
- Create: `web/src/lib/content.ts`
- Test: `web/tests/content.test.ts`, `web/tests/parity.test.ts`

**Interfaces:**
- Produces, from `content.ts`:
  - **Constants:**
    - `REPO_URL = 'https://github.com/edraj/sauron'`
    - `INSTALL_URL = 'https://github.com/edraj/sauron/blob/main/INSTALL.md'`
    - `LICENSE_URL = 'https://github.com/edraj/sauron/blob/main/LICENSE'`
  - **`hero`:** `{ headlineLead: string; headlineFire: string; sub: string; finePrint: string }`
  - **`stories: Story[]`**, where
    `Story = { id: string; kicker: string; title: string; body: string; back: Shot; front: Shot; callout: string; mirrored: boolean }`
    and `Shot = { src: string; alt: string; label: string }`.
    - `src` is a file name under `static/screenshots/`, e.g. `'overview.webp'`.
    - Ids are `'hero'`, `'errors-events'`, `'analytics'`, `'alerts'`.
  - **`moreFeatures: string[]`:** the 8 items from the spec, each verified against code.
  - **`sdks: SdkTab[]`**, where
    `SdkTab = { id: 'browser'|'flutter'|'node'|'python'|'csharp'; label: string; install: string; lang: 'bash'|'ts'|'dart'|'python'|'csharp'; code: string; note?: string }`
  - **`installs: InstallTab[]`**, where
    `InstallTab = { id: 'compose'|'rpm'|'deb'; label: string; steps: { caption: string; code: string }[]; warning?: string }`
  - **`architectureLine: string`**

- [ ] **Step 1: Write the failing tests** in `tests/content.test.ts`:
  - `no_wiki_anywhere`: `JSON.stringify` of every export matches neither `/wiki/i` nor
    `/sentry|posthog/i`.
  - `repo_url_exact`: `REPO_URL === 'https://github.com/edraj/sauron'`.
  - `four_stories_in_order`: ids are
    `['hero','errors-events','analytics','alerts']`, and `analytics.mirrored === true` while
    the others are `false`.
  - `csharp_not_nuget`: the csharp tab `install` contains `reference` and does not contain
    `dotnet add package Sauron`.
  - `compose_warns_secrets`: the compose tab text includes `JWT_SECRET`, `NOTIFY_SECRET_KEY`
    and `10002`.
  - `every_shot_has_alt`: every `Shot.alt.length > 20`.
- [ ] **Step 2: Write the failing tests** in `tests/parity.test.ts`. They read
  `../INSTALL.md` and `../sdks/{js,flutter,node,python,csharp}/README.md` via `node:fs`, and
  `describe.skipIf(!existsSync('../INSTALL.md'))` with a reason string when the parent repo is
  absent. For each `installs[].steps[].code` and `sdks[].code`/`install`:
  - every non-blank line, after trimming, appears verbatim in the corresponding source file.
  - **Exception:** lines the content module marks with a trailing `  # site-only` /
    `// site-only` comment. Strip the marker before rendering. Allowed only for the
    `git clone` line and `cd sauron`.
- [ ] **Step 3:** Run `npx vitest run tests/content.test.ts tests/parity.test.ts`. Expected:
  FAIL.
- [ ] **Step 4: Fill `content.ts`.**
  - Story callouts and copy come from the spec's sections 1–4. For analytics, `back` is
    `funnel.webp` and `front` is `retention.webp`.
  - SDK code is copied from each README's quick start, trimmed to init + identify + track +
    one capture:
    - Browser: `Sauron.init/identify/track/captureException`
    - Flutter: `Sauron.init` with `appRunner`, then the identify/track block
    - Node: `init/track/captureException/close`
    - Python: `init/track/identify/capture_exception/close`
    - C#: `SauronSdk.Init/Track` plus the catch block
  - Install steps come from `INSTALL.md` §A/§B/§C:
    - Compose: `git clone https://github.com/edraj/sauron.git` (site-only),
      `cd sauron` (site-only), `cp .env.example .env`, `docker compose up --build`
    - RPM: build, install, configure, start
    - .deb: toolchain, build, install, configure, start
  - Export a helper `stripSiteOnly(code: string): string`.
- [ ] **Step 5:** Run the tests. Expected: PASS, with parity running (not skipped) in this
  checkout.
- [ ] **Step 6:** Verify each `moreFeatures` item by grep. Drop any without code backing:
  - releases: `release` routes
  - source maps: `source-maps`
  - saved views: `saved_views`
  - RBAC: `role:manage`
  - transactions
  - tiering: `sauron-tier`
  - MCP server: `mcp`
  - Arabic: `ar.json` or i18n
  - Also confirm the alert channel list from `backend/` (Slack, Discord, email, PagerDuty,
    webhook, plus a 6th?) and use the verified list in the alerts story body.
- [ ] **Step 7:** Leave uncommitted.

### Task 3: Interactive pieces (WindowFrame, StoryShot, Tabs, CodeBlock/CopyButton)

**Files:**
- Create in `web/src/lib/components/`: `WindowFrame.svelte`, `StoryShot.svelte`,
  `Tabs.svelte`, `CodeBlock.svelte`
- Test: `web/tests/components.test.ts`

**Interfaces:**
- Consumes: `Shot` and `Story` types from `content.ts`; `base` from `$app/paths`.
- Produces:
  - **`WindowFrame`**
    - Props: `{ label: string; src: string; alt: string; dim?: boolean; eager?: boolean }`
    - Renders a `<figure>` with a title bar (3 dots and the label) and an `<img>` whose `src`
      is `` `${base}/screenshots/${src}` ``.
    - Also renders `width`/`height`; `loading="lazy"` unless `eager`; `decoding="async"`.
  - **`StoryShot`**
    - Props: `{ back: Shot; front: Shot; callout: string; mirrored?: boolean; eager?: boolean }`
    - The back window carries `data-layer="back"` and `class="max-md:hidden"`. The front
      window carries `data-layer="front"`.
    - The callout chip is a `<p data-callout>`, placed above the front window on mobile.
  - **`Tabs`**
    - Props: `{ tabs: { id: string; label: string }[]; initial?: string; children: Snippet<[string]> }`
      (the snippet receives the active id).
    - Follows the WAI-ARIA tabs pattern: `role="tablist"`/`tab`/`tabpanel`,
      `aria-selected`, roving `tabindex`, and ArrowLeft/ArrowRight/Home/End.
  - **`CodeBlock`**
    - Props: `{ code: string; lang: string; caption?: string }`
    - Renders `<pre tabindex="0" class="overflow-x-auto">` plus a Copy button.
    - The button label goes `"Copy"` → `"Copied"` (2 s) → `"Copy"`. On rejection or a
      missing `navigator.clipboard` it shows `"Copy failed"`.

- [ ] **Step 1: Write the failing tests** in `tests/components.test.ts`, using `mount` from
  `svelte` + `flushSync` in jsdom:
  - `window_frame_prefixes_base`: `$app/paths` is mocked via `vi.mock` with `base: '/x'`;
    the img `src` must be `'/x/screenshots/a.webp'`.
  - `story_shot_layers`: one `[data-layer=back]` with the `max-md:hidden` class, one
    `[data-layer=front]`, and the callout text is present.
  - `tabs_keyboard`: 3 tabs. ArrowRight on the first focuses the second and sets
    `aria-selected="true"` on it. End selects the last. Home selects the first.
  - `copy_success`: with `navigator.clipboard.writeText` resolving, a click makes the button
    text `"Copied"`.
  - `copy_failure_no_throw`: with `navigator.clipboard` undefined, a click shows
    `"Copy failed"` and no error is thrown.
- [ ] **Step 2:** Run `npx vitest run tests/components.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement the four components** with Tailwind classes using the theme tokens.
  - Window: `rounded-[10px] border border-line bg-[#111215]` with
    `shadow-[0_30px_80px_-20px_rgba(0,0,0,.85),0_0_0_1px_rgba(255,143,34,.08)]`. The back
    window uses `opacity-70`.
  - Desktop layout: back at `left-0 top-0 w-[68%]` and front at `right-0 top-14 w-[68%]`
    inside a `relative` box with `aspect-[16/12]`. `mirrored` swaps the sides.
  - The callout chip has a `border-fire-3` and a `shadow-[0_0_24px_rgba(255,143,34,.35)]` glow.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Leave uncommitted.

### Task 4: Page assembly, sections and meta

**Files:**
- Create in `web/src/lib/sections/`: `Nav.svelte`, `Hero.svelte`, `Story.svelte`,
  `MoreFeatures.svelte`, `Sdks.svelte`, `Install.svelte`, `UnderTheHood.svelte`,
  `FinalCta.svelte`, `Footer.svelte`
- Create: `web/src/lib/components/EyeMark.svelte`, the favicon eye as inline SVG with props
  `{ size?: number }`, and `web/static/favicon.svg`, copied from `dashboard/static/favicon.svg`
- Modify: `web/src/routes/+page.svelte`
- Test: `web/tests/page.test.ts`

**Interfaces:**
- Consumes: everything `content.ts` exports, plus the Task 3 components.
- Produces:
  - **Section anchors:** `#features` (first story after the hero), `#sdks`, `#install`.
  - **`<svelte:head>`:**
    - `<title>Sauron · Error tracking + product analytics, self-hosted</title>`
    - `meta description` = `hero.sub`
    - `og:title`, `og:description`, `og:image` = `` `${base}/og.png` ``
    - `twitter:card=summary_large_image`
    - favicon link
  - The hero `StoryShot` gets `eager`.

- [ ] **Step 1: Write the failing test** `tests/page.test.ts`. It runs `vite build` with
  `BASE_PATH=/sauron-web` once (in `beforeAll`) and reads `build/index.html`. Assertions:
  - It contains the headline text, `id="features"`, `id="sdks"` and `id="install"`.
  - It contains `docker compose up --build`, i.e. the default tab's code is pre-rendered.
  - It contains `href="https://github.com/edraj/sauron"`.
  - It does not match `/wiki/i`.
  - Every `<img` has a non-empty `alt`.
  - Every `/screenshots/` URL starts with `/sauron-web/screenshots/`.
  - `og:image` content contains `/sauron-web/og.png`.
- [ ] **Step 2:** Run it. Expected: FAIL.
- [ ] **Step 3: Implement the sections and the page in spec order.**
  - **Nav:** sticky, `backdrop-blur`. Anchors Features/SDKs/Install plus a "★ GitHub" link.
  - **Hero:** two columns at `lg`, stacked below. Buttons: "Self-host it" → `#install`
    (gradient fill, text `#0c0a09`, AA-checked) and "★ Star on GitHub" → `REPO_URL`
    (outline).
  - **Story:** wraps `StoryShot` with kicker/title/body; `mirrored` reverses the columns.
  - **MoreFeatures:** a 2/4-column text grid.
  - **Sdks / Install:** `Tabs` + `CodeBlock`. Install shows the `warning` above the code, plus
    a "Full guide →" link to `INSTALL_URL`.
  - **UnderTheHood:** `architectureLine` rendered as an inline SVG flow of 6 boxes, wrapping
    to a vertical list below `md`.
  - **FinalCta:** "Open source. Yours to run." plus the star button.
  - **Footer:** AGPL-3.0 (→ `LICENSE_URL`) · GitHub · INSTALL.md.
  - **Motion:** a CSS `@keyframes` rise-in on `[data-reveal]`, toggled by an
    IntersectionObserver action in `src/lib/reveal.ts`. Wrap it in
    `@media (prefers-reduced-motion: no-preference)`. Content is visible by default if JS
    never runs: the hidden state only applies under `html.js`, set by an inline script in
    `app.html`.
  - Until Task 5 lands, `static/screenshots/` holds **generated neutral placeholders**. Each
    is a flat `#111215` 2880×1800 WebP with the shot name drawn in grey, produced by
    `scripts/screenshots/placeholders.mjs`.
  - **Never** copy anything from `.superpowers/screenshots/quiet-refresh/`: it holds private
    data, and the owner forbade it. Task 5 overwrites every placeholder.
- [ ] **Step 4:** Run `npx vitest run`. Expected: all PASS.
- [ ] **Step 5:** Run `npm run check`. Expected: 0 errors.
- [ ] **Step 6:** Leave uncommitted.

### Task 5: ACME demo seed and screenshot capture

**Files:**
- Create in `web/scripts/screenshots/`:
  - `README.md`: how to boot the stack and run.
  - `stack.sh`: boots native PG and Valkey and the API/ingest bins in a scratch dir.
  - `seed.mjs`: ACME org, users and traffic.
  - `capture.mjs`: logs in and screenshots the 8 pages.
  - `shots.json`: route, wait-for selector, file name, and an optional `prepare` action per shot.
  - `scan.mjs`: forbidden-text check.
- Output: `web/static/screenshots/{overview,session,issue,person,retention,funnel,monitors,alert-rule}.webp`
  and `web/static/og.png`
- Test: `web/tests/screenshots.test.ts`

**Interfaces:**
- Consumes: the file names in `content.ts` stories (Task 2).
- Produces:
  - The 8 WebP files at 2880×1800 (1440×900 @2×), quality 82.
  - `og.png` at 1200×630: a capture of the built landing page's hero at a 1200×630 viewport.
  - `npm run shots`: runs seed (when the ACME org is not already present) and then capture.

- [ ] **Step 1: Write the failing test** `tests/screenshots.test.ts`. Assertions:
  - Every story `Shot.src` exists in `static/screenshots/`.
  - Each file is WebP: first bytes `RIFF....WEBP`; dimensions read from the VP8/VP8L/VP8X
    header are 2880×1800.
  - `og.png` is 1200×630.
- [ ] **Step 2:** Run it. Expected: FAIL (placeholders are PNG with the wrong dimensions).
- [ ] **Step 3: Write `stack.sh`.** It follows memory notes `local-e2e-api-drive` and
  `quiet-refresh`.
  - `initdb` into `$SCRATCH/pg` with `-c unix_socket_directories=$SCRATCH/sock`, listening
    on 127.0.0.1:55433.
  - `valkey-server --port 56380 --save '' --appendonly no`.
  - `cargo build -p sauron-api -p sauron-ingest -p sauron-migrate`, with `DUCKDB_LIB_DIR`,
    `LD_LIBRARY_PATH` and `RUSTFLAGS` set to `.cache/duckdb/1.5.6/<triple>`.
  - Run `sauron-migrate`.
  - API on :8090 and ingest on :8091, env `SAURON_DEV=1`, a 40-char `JWT_SECRET` and a
    `NOTIFY_SECRET_KEY`.
  - Run `sauron-monitor` only if the monitors shot needs live status.
  - Before starting, check `ss -ltnp` for 8090/8091/55433/56380 and abort with a message if
    any port is taken.
- [ ] **Step 4: Write `seed.mjs`** (Node, global `fetch`).
  - `POST /v1/auth/register` with `{ name: 'Alex Morgan', email: 'alex@acme.example', password, org_name: 'ACME' }`.
  - Project `Storefront`; app `ACME Shop` with `app_type: 'flutter'`; environments
    `production` and `dev`.
  - Read the production DSN from `GET /v1/apps/{id}/environments`.
  - Send ingest envelopes to `/api/<env>/envelope` with an `X-Sauron-Key` header.
    - **Envelope shape:** take it from `sdks/wire-fixtures/` and the Ingest Wire Contract in
      the code, not from memory.
    - **Spread:** ~400 distinct users over the last **29 days** (the ingest clamp is 30), with
      a weekday rhythm and gentle growth.
    - **Each session:** `$screen` views (Home, Product, Cart, Checkout, Orders); events
      `product_viewed`, `add_to_cart`, `coupon_applied`, `coupon_removed`,
      `checkout_started`, `checkout_completed`; and navigation/HTTP transactions with
      realistic durations.
    - **The story is the spec's "ACME story" table. Every beat must be visible in its shot.**
    - **Releases:** `shop@3.2.0+320` for days 29–8 (~99% crash-free, checkout conversion
      ~60%) and `shop@3.3.0+330` for days 7–0.
    - **On 3.3.0:** sessions that do `coupon_removed` after `add_to_cart` throw an
      **unhandled** `StateError: Bad state: No element`. The frames come from the issue spec:
      `CartRepository.first (lib/cart/cart_repository.dart:88)` marked in-app, then a
      `CheckoutScreen.build` frame. The session ends with no `checkout_completed`. This drops
      3.3.0 checkout conversion to ~44% and crash-free to ~95%.
    - **Background noise** on both releases: `DioException: Connection timed out after
      30000ms` at a low rate, and a few `FormatException`s.
    - **One named demo customer**, "Maya Chen" (`u_maya`), has 3 sessions on 3.3.0:
      1. Browses.
      2. `add_to_cart → coupon_removed → StateError`.
      3. The next day, retries the same and crashes again; no purchase.

      She is the person-profile shot and her crashing session is the session shot.
    - **Retention:** cohorts from before the release retain at ~35–40% in week 1. The
      3.3.0-week cohort is visibly lower.
    - **Identify everyone** with neutral display names and traits like `plan`, so the people
      pages look real.
  - Create two uptime monitors (`https://shop.acme.example/health` and
    `https://api.acme.example/health`; their status may stay pending or unknown), one Slack
    alert channel and one alert rule ("Checkout errors spike", issue trigger).
  - Run any operator rollup backfills the Overview, Retention, Persons and Funnel pages need.
    Check memory `rollup-backfills-are-operator-run` and the `sauron-api`/`sauron-migrate`
    subcommands, and run them. Confirm every page shows data before capturing.
- [ ] **Step 5: Write `capture.mjs`.**
  - Log in over the API, inject `sauron.refresh_token`, reload, then set `location.hash`
    per `shots.json`.
  - Viewport 1440×900, `deviceScaleFactor: 2`, dark theme. Wait for the selector, then
    600 ms.
  - Shots, in this order:
    - `/` (Overview, 30d)
    - `/sessions/<a session containing add_to_cart → StateError>`
    - `/issues/<StateError issue>`
    - `/persons/<that session's user>`
    - `/retention`, with the error-split checkbox (`retention.errorSplit.toggle`) turned on
      and weekly granularity
    - `/funnels` (fill steps `$screen:Product → add_to_cart → checkout_started → checkout_completed` via the setter-dispatch recipe, then run)
    - `/monitors`
    - `/admin/alerts` (the rule editor open)
  - Save PNGs to a scratch dir and convert to WebP with `cwebp -q 82` (or `sharp` if `cwebp`
    is missing; add it as a devDependency only then).
  - Afterwards, update each story `callout` in `content.ts` so it states what the captured
    shot actually shows: the real affected-user count and the real user id.
- [ ] **Step 6: Write `scan.mjs`** and run it. It OCR-free checks every captured page's
  `document.body.innerText` (saved alongside each PNG as `.txt` during capture) against
  `/zain|soheyb|merah|iq\.zain\.com|pk_[a-z0-9]{6,}/i`. Expected: no matches.
- [ ] **Step 7:** Run the stack, seed and capture. Then run
  `npx vitest run tests/screenshots.test.ts`. Expected: PASS.
- [ ] **Step 8:** Look at every captured image and check:
  - It shows its story beat from the spec table.
  - It is comprehensive, with a filled page: no empty states, no error banners, no
    "building…" chips.
  - It reads as a real product solving a real incident.

  Re-seed or re-capture any that fail. If a page can't show its beat (e.g. funnels can't be
  split by release), adjust the callout to what the page does show, and note it in the
  report.
- [ ] **Step 9:** Stop the stack (`stack.sh stop`) and leave uncommitted.

### Task 6: Verification, README and visual review

**Files:**
- Create: `web/tests/e2e.test.ts` (Playwright against `vite preview`)
- Modify: `web/README.md`

**Interfaces:**
- Consumes: the built site from Tasks 1–5.

- [ ] **Step 1: Write the failing e2e test** `tests/e2e.test.ts`, using `playwright-core` +
  `/usr/bin/chromium-browser`. It starts `vite preview --port 4179`.
  - `no_horizontal_scroll_390`: at a 390×844 viewport,
    `document.documentElement.scrollWidth <= 390`.
  - `back_window_hidden_mobile`: at 390px, every `[data-layer=back]` has `display: none`.
  - `reduced_motion_visible`: with `reducedMotion: 'reduce'`, every `[data-reveal]` has
    computed `opacity === '1'`.
  - `no_placeholder_bytes`: no file in `static/screenshots/` is a placeholder. Placeholders
    are under 30 KB and real captures are far larger, so assert `size > 60_000` for each.
  - `contrast_primary_button`: the computed text and background of the "Self-host it" button
    reach a WCAG contrast ratio ≥ 4.5 at the gradient's lightest stop used behind the label.
- [ ] **Step 2:** Run `npx vitest run tests/e2e.test.ts`. Expected: FAIL until any layout
  issues are fixed. Then fix them.
- [ ] **Step 3: Write `web/README.md`:** what this is, `npm i`, `npm run dev`, `npm run build`
  (with `BASE_PATH` for GitHub Pages), `npm test`, and regenerating screenshots (see
  `scripts/screenshots/README.md`). No wiki link.
- [ ] **Step 4:** Run the full gate: `npm run check && npx vitest run && npm run build`.
  Expected: 0 errors, all tests pass, `build/` written.
- [ ] **Step 5:** Visual review. Screenshot the built page at 1440 and at 390, top to bottom.
  Check it against the approved mock (`page-v2.html`): fire accents are used sparingly, story
  windows layer cleanly without overlapping the next section, and the callouts are readable.
- [ ] **Step 6:** Report the outcome. In the parent repo `.gitmodules` + `web` are staged; in
  `web/` everything is uncommitted. List what the owner needs to commit and push in each repo.
