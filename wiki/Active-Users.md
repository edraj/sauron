# Active Users

**Active users** answers "how many distinct people used these apps", per UTC
calendar day, across as many apps in one project as you pick — each with its own
environment.

Open it from **Analyze → Active users**. It needs the `event:read` permission.

## What the three numbers mean

Every day carries three figures, and they always add up:

| Figure | Meaning |
|---|---|
| **Active users** | Distinct identities active that day, across every selected app+environment |
| **Identified** | The part of that total your app told us is a real person — via `identify()`, or an event whose `context.user.id` equals the `distinct_id` it was sent with |
| **Guests** | Everyone else: anonymous, SDK-minted ids |

`Active users = Identified + Guests` is exact, by construction.

## How people are matched across apps

**Identified users merge across apps by exact string equality on the distinct
ID your SDK sends.** If your web app calls someone `u-42` and your mobile app
calls them `auth0|abc`, they count as **two people**, not one. There is no
server-side fix for this — make your apps send the same identifier.

**Guests never merge across apps at all.** An anonymous id in app A and the
identical string in app B are two different guests, deliberately: the number for
{A, B} must not change depending on whether you also tick C.

A large guest share therefore tells you how much of your total was never a
candidate for merging in the first place.

## The window

- Days are **UTC calendar days**, everywhere: the chart, the CSV and the file
  name. There is no per-user or per-organisation display timezone.
- The maximum window is **92 days**, and `apps × days` may not exceed 1200.
- **Your visible window is usually shorter than you asked for.** Data older than
  `TIER_HOT_DAYS` (default 30) is moved to cold Parquet storage by
  `sauron-tier`, which runs by default in both shipped topologies, so a 90-day
  request typically returns about 30 days. When that happens the page shows a
  banner naming the date the report actually starts from, and the CSV's file
  name carries the real range. Raising `TIER_HOT_DAYS` buys a longer window at
  the cost of hot Postgres storage.
- The last bar on the chart is **today**, which is still filling. The headline
  tiles read from the last **complete** day, so they never dip at midnight. A
  window containing only today shows an em-dash, not `0` — zero active users is
  a real answer and this is not it.

## "2 of 5 environments"

If your grants reach only some of an app's environments, selecting "All
environments" for that app quietly means "all the ones you can read". The picker
says `2 of 5 environments` when that happens, because the number is genuinely
not comparable with a colleague's who has app-wide access. (An app-wide reader
also sees rows that belong to no environment at all; a partial reader does not.)

## Export

**Export CSV** downloads exactly what is on screen: one row per displayed day,
columns `day,active_total,active_identified,active_guest`. The file name carries
the project id and the effective date range, so a download in a shared folder
still says what it is. Both halves are exported, not just the total — a
spreadsheet is where someone re-derives a figure months later with no page
around it to carry the matching caveat above.

## Two things that silently change these numbers

**A PII mask on an identity-bearing key dismantles cross-app matching.** The
mask enforcer runs before identification, so once `context.user.id` — or
whatever key your app uses as its `distinct_id`; an email address is both a
common choice and exactly the kind of value a PII policy flags — is masked, no
future person can ever be marked identified through it. Nobody already
identified loses the flag, so nothing moves on the day the mask lands: instead
**Identified** plateaus and then decays as the existing population churns, while
**Guests** climbs to meet it. Nothing labels the cause and nothing can
reconstruct it afterwards. Decide before you apply the mask, not after.

**A skipped migration loses the signal permanently.** RPM upgrades do not re-run
`sauron-migrate` (see the upgrade section of the RPM setup guide). Until
migration `000038` is applied, this page returns `503
schema_migration_required` and the ingest worker records no identification at
all — and that gap **cannot** be backfilled later, because the backfill can only
see stored traits and alias rows. Everyone first active during an un-migrated
window is filed under **Guests** forever.

## Known limitation

Reported numbers for a browser app drop sharply and permanently the day you
adopt **`@edraj/sauron-browser` 1.4.0 or later**. Before 1.4.0 the SDK re-minted
an anonymous id on every page load, so the count was page loads rather than
people — typically a 5-10x inflation, all of it in **Guests**. The drop is the
fix landing, not a regression.

Nothing restates the history: days before the upgrade keep their inflated
numbers, so any chart spanning the upgrade has a step in it. Note the upgrade
date somewhere your readers will find it, and treat year-on-year comparisons
across that date as comparing two different metrics — because they are.
