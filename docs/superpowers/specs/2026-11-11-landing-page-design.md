# Sauron landing page (`web/` submodule)

Date: 2026-11-11 · Status: design approved in conversation, pending plan

## Intent

A public landing page for Sauron, aimed at **developers deciding whether to self-host it**.
The page should leave them with three things, in order:

1. **Self-host it.** They reach the install commands and believe it's a short path.
2. **Star the repo** on GitHub.
3. **Know more about the project.** They understand what it is and why it's different.

**The pitch** is the owner's own words: **one stack instead of two, with errors linked to user
events.** Sauron is error tracking and product analytics in one self-hosted platform, and
every crash sits on the timeline of the person who hit it.

## Goals

- One long, single-page site that tells the pitch with **real dashboard screenshots**,
  presented professionally.
- Install instructions short enough to copy-paste from the page.
- Pre-rendered static HTML, so search engines and link previews see the content without
  running JavaScript.

## Non-goals

- No docs site, blog, pricing, sign-up, hosted offering or contact form.
- **No links to the wiki, anywhere.** This is an explicit owner instruction. Install help points
  to `INSTALL.md` on GitHub.
- **No competitor comparison section.** It was proposed and cut, so Sentry and PostHog are not
  named on the page.
- No deploy pipeline. Hosting is undecided (see "Left to the builder").

## Anti-goals (would be a failure even if it works)

- **Any screenshot showing real personal or company data.** That means the owner's name, the
  "Zain" org, real emails, or real DSNs and keys. The existing captures in
  `.superpowers/screenshots/quiet-refresh/` contain all of these and **must not ship**.
- **Invented facts.** No made-up star counts, user counts, performance numbers or features. Every
  feature claim must be verified against the code. Every number must be measured, with its
  source noted in the plan, or left out.
- **A generic SaaS template.** The look is the owner's chosen "Eye of fire" direction, not a
  stock gradient-blob landing page.
- **Alarming screenshot data with no story behind it.** For example, the current 41.5%
  crash-free rate, or 3 days of sparse bars. The one deliberate spike is the `shop@3.3.0`
  incident described below.

## Decided by the owner

| Topic | Decision |
|---|---|
| Location | `web/` in this repo is a **git submodule** of `splimter/sauron-web`. That repo exists and holds only a README (1 commit). |
| Stack | Svelte + Tailwind |
| Visual direction | **A · "Eye of fire"**: near-black background with the favicon's fire gradient as the brand moment. Mockups: `docs/superpowers/specs/2026-11-11-landing-page/` (`hero-direction.html`, `page-v2.html`; their images are deliberately not copied). |
| Screenshot treatment | **"C" style everywhere**: two layered app windows (a dimmer one behind, a sharp one in front, offset) plus **one glowing callout chip** that ties them together, e.g. "● add_to_cart → StateError · same user". |
| Screenshots | **Real** dashboard captures, re-shot in a demo org named **ACME**. |
| Sections | Listed below. The competitor section was cut. |

## Decided by Claude (owner said "sounds good" to these)

- **Framework:** SvelteKit with `@sveltejs/adapter-static`, fully pre-rendered. Svelte 5
  (runes), TypeScript, **Tailwind CSS v4**, Vite.
- **Submodule URL:** the HTTPS form `https://github.com/splimter/sauron-web.git` in
  `.gitmodules`, so anyone can clone without SSH keys. The owner may push via SSH locally.
- **No commits in either repo.** The owner commits. Also: never create branches and never
  `git stash`, because another session edits this checkout live. `git submodule add` stages
  `.gitmodules` and the gitlink; leave them staged and uncommitted.
- **GitHub star target:** `https://github.com/edraj/sauron`, the public repo and this checkout's
  `origin`. The README's badge URLs point at `splimter/sauron`, which does not resolve publicly.
- **Screenshot user:** a neutral demo user in the top bar, not the owner's name.

## Page content, top to bottom

**Nav (sticky):** the eye mark plus the "SAURON" wordmark. Anchors: Features · SDKs · Install.
A "★ GitHub" link on the right.

1. **Hero**
   - Headline: "See every error **through your users' eyes.**" The second clause carries the
     fire gradient.
   - Sub: "Error tracking and product analytics in one self-hosted stack. Every crash lands on
     the timeline of the person who hit it."
   - Buttons: **Self-host it** (filled fire gradient, jumps to #install) and **★ Star on
     GitHub** (outline).
   - Fine print: "Open source · AGPL-3.0 · Rust + Postgres + Redis".
   - Visual: Overview window behind, Session timeline window in front. Callout: same user,
     add_to_cart → error.

2. **Errors ↔ user events** (the core story)
   - "From a crash to the person who hit it."
   - Issue detail window behind, Person profile timeline in front.
   - Callout: "N users affected → u_…".
   - Copy: from an issue you see who hit it and what they did first. From a person you see
     their errors inline with their events.

3. **Product analytics** (mirrored: visual on the left)
   - Headline: "Did the crash cost you the customer?"
   - Body: "Retention splits every cohort by whether they hit an error. Funnels show where
     checkout leaks."
   - **Retention with the error split turned on is the front window; the Funnel is behind.**
     The builder check found that funnels have no link to errors in the code, while
     retention has a "hit an error / no error" split and an errors column in its churn table.
   - The callout states what the retention shot actually shows.

4. **Alerts & uptime**
   - "Know before your users tell you."
   - Monitors window behind, alert rule editor in front.
   - Copy lists the alert channels **verified from the code**. Slack, Discord, email,
     PagerDuty and webhook were seen; memory says there are 6, so confirm the sixth.

5. **And the rest:** a text-only grid, no screenshots. Releases & crash-free rate · Source maps /
   symbolication · Search & saved views · Fine-grained RBAC · Performance & transactions ·
   Parquet cold-storage tiering · MCP server for AI agents · Arabic UI / RTL. Verify each
   against the code and drop any that doesn't hold.

6. **One SDK, both signals:** tabs for Browser · Flutter · Node · Python · C#.
   - Each tab shows install + init + identify + track, **taken from that SDK's README
     quickstart** (`sdks/*/README.md`).
   - Packages: `@edraj/sauron-browser`, `sauron_flutter`, `@edraj/sauron-node`, `sauron-sdk`
     (PyPI).
   - **C# is not on NuGet yet.** Its tab shows the project-reference install from its README
     with a "not on NuGet yet" note.
   - Note that SDKs are LGPL-3.0 while the server is AGPL-3.0.

7. **Self-host it** (`#install`): tabs for Docker Compose · Fedora/RHEL (RPM) · Debian/Ubuntu (.deb).
   - Commands come from `INSTALL.md` sections A, B and C. RPM and .deb are **trimmed to the
     key steps** (build → install → configure → start), with lines kept verbatim. They don't
     use the release packages, since `INSTALL.md` doesn't document them.
   - Compose must carry the "set `JWT_SECRET` and `NOTIFY_SECRET_KEY`" warning and the
     `:10002` dashboard port.
   - Every block gets a copy button.
   - "Full guide" links to `INSTALL.md` on GitHub.

8. **Under the hood:** a small diagram or one line: SDK → ingest (Rust/axum) → Redis stream →
   workers → Postgres ← API ← dashboard. "Two things to run: Postgres and Redis."

9. **Final call to action:** "Open source. Yours to run." plus the **★ Star on GitHub** button.

**Footer:** AGPL-3.0 · GitHub · `INSTALL.md`.

## Visual system

- **Background** `#0c0a09`; raised surfaces around `#120e0b` / `#15110e`; hairlines `#2a1d15`.
  Body text `#f5f0eb`, muted `#a8a29e`.
- **Fire gradient**, from `dashboard/static/favicon.svg`: `#fff7d6 → #ffd451 → #ff8f22 → #f14e0b
  → #a81a03`. Use it on the headline accent, the primary button, callout chip borders and glows,
  and a few soft radial glows behind visuals. Use it sparingly: it is the accent, not the page.
- **Dark only.** No light theme.
- **Font:** Inter (variable), self-hosted. The dashboard already uses `@fontsource-variable/inter`.
- **Logo:** reuse the favicon's eye mark.
- **Window frame** (the "C" component): rounded window, dark title bar with three dots and a
  label, deep shadow, faint orange outer ring. The back window is dimmed (~70% opacity) and
  offset up and left of the front one.
  - **Mobile:** below ~768px, show only the front window, with the callout above it. No
    horizontal page scroll at 390px.
- **Motion:** subtle fade/rise on scroll into view and a slow glow drift, all off under
  `prefers-reduced-motion`.
- **Accessibility:** WCAG AA contrast for text, including the primary button. The primary button uses **dark `#0c0a09` text** on the orange gradient (~8:1), as in the mockup. Screenshots get descriptive `alt`. Tabs are keyboard-operable.

## Screenshots

Capture with the existing Playwright rig: headless `/usr/bin/chromium-browser` plus
`playwright-core`. The recipe is in memory notes `quiet-refresh`, `local-e2e-api-drive` and
`dashboard-browser-drive`.

**Requirements:**
- **Org "ACME"**, a neutral project and app name, and a neutral demo user (name and email) in the
  top bar.
- No real DSNs or keys visible.
- **Believable data over ~30 days:** steady volumes, a healthy-looking crash-free rate, and
  realistic event and error names. The checkout/cart story from the current shots works well.
- Dark theme, viewport **1440×900 at 2× device scale**, exported as WebP/AVIF with a PNG
  fallback, sized for their slot.

**Shots needed (8):**

| Page | Section |
|---|---|
| Overview | hero, back |
| Session detail timeline | hero, front |
| Issue detail | errors↔events, back |
| Person profile timeline | errors↔events, front |
| Retention | analytics, back |
| Funnel | analytics, front |
| Uptime monitors list | alerts, back |
| Alert rule editor | alerts, front |

**Known traps:**
- Ingest clamps timestamps to **30 days in the past and 15 minutes in the future**, so
  backdating beyond 30 days via ingest won't work.
- Several dashboard pages read rollups that need **operator-run backfills** before they show
  data. See memory `rollup-backfills-are-operator-run` and `rollup-gate-closed-in-tests`.
- `dashboard/static/config.js` pins the API URL; it outranks `VITE_API_BASE_URL`.

**Owner requirements (added mid-build):**
- Screenshots must be **comprehensive**.
- Shots must be **freshly captured**. Nothing from the existing private captures may be used,
  not even as a temporary placeholder.
- The seeded data must tell **one convincing use case**: a visitor should see Sauron *solving
  a real problem*, not just showing tables.

### The ACME story (the seed is built to tell this)

ACME Shop is a Flutter storefront app with ~400 active users. Release **`shop@3.3.0`** ships
two weeks ago (moved from 7 days during the build, so the weekly retention cohorts after the release have had time to elapse). It contains a cart refactor that throws
**`StateError: Bad state: No element`** in `CartRepository.first` when a user removes a coupon
and their cart is briefly empty. The checkout screen then fails to render.

Every screenshot is one beat of that story:

| Beat | Shot | What the visitor reads in it |
|---|---|---|
| 1. You get told | **Alert history**: "New fatal issue" → #shop-alerts and the on-call pager, then "Checkout errors spike" | Sauron paged the team 40 s after the first crash. |
| 2. Something changed | **Overview** (30d) | Errors jump on the release day; events keep flowing; crash-free dips from ~99% to ~95%. |
| 3. What broke | **Issue detail** for `StateError` | First seen in `shop@3.3.0`; affected-users count; stack frame in `cart_repository.dart` marked *In app*. |
| 4. What the user did | **Session timeline** | `product_viewed → add_to_cart → coupon_removed → StateError`, then the session ends. **This is the money shot.** |
| 5. Who it hurt | **Person profile** (a named demo customer) | Their events and errors interleaved across sessions: tried twice, then didn't buy. |
| 6. What it cost | **Funnel** Product → add_to_cart → checkout_started → checkout_completed (back window) | Checkout leaks at the last step. |
| 7. Did they come back | **Retention, error split ON** (front window) | People who hit an error retain clearly worse than those who didn't. |
| 8. Not the backend | **Uptime monitors** | API and shop health checks green, so the bug is client-side. |

**Background data stays healthy and believable:**
- `DioException` timeouts at a low rate.
- A few `FormatException`s.
- Steady weekday rhythm with mild growth.
- The pre-release crash-free rate is around 99%.

Numbers shown in callouts are whatever the capture actually shows. They are read from the
captured page, never typed in by hand.

Keep the capture script and seed in the `web/` repo (e.g. `web/scripts/screenshots/`), so the
shots can be regenerated when the dashboard changes.

## Meta

- `<title>` and meta description carry the pitch.
- Open Graph / Twitter card image: a 1200×630 render of the hero.
- Canonical URL set from config once hosting is known.

## Left to the builder

- **Hosting is undecided.** Make the base path configurable at build time (e.g. a `BASE_PATH`
  env var mapped to `kit.paths.base`), so GitHub Pages under `/sauron-web/` or a custom domain
  both work. Do not add a deploy workflow.
- How to seed the ACME demo data: an ingest-driven script, SQL, or a mix, within the traps
  above.
- Component structure, exact copy polish beyond the lines above, and animation details.
- Whether section 8 is a small SVG diagram or a single line.
- **`web/` must build standalone** from a fresh clone of `sauron-web`, so content is copied in, not read from `../`. Parity tests and the screenshot script use the parent repo when present.
- `web/` gets its own `README.md` covering dev, build and screenshot regeneration. It does
  **not** link the wiki.

## Build notes (2026-11-11)

- **The alerts shot is the alert History tab, not the rule editor.** History shows the story
  beat ("you get told"), and its rows are derived from the seeded errors.
- **Known dashboard bug, worked around in capture only:** on the Funnels page, preset ranges
  never leave "computing". The cache key includes a window resolved against "now", so it moves
  every poll. Details are in `web/scripts/screenshots/README.md`. It is not fixed in
  `sauron-api` as part of this work.
