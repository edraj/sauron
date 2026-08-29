# Retention

**Retention** answers "did the people who arrived come back?" — the question a
growing active-user count cannot answer on its own, because acquisition can
refill a bucket that is draining just as fast.

Open it from **Analyze → Retention**. It needs the `event:read` permission, and
it honours the environment picker.

## Before it works: run the backfill

Retention reads a rollup (`person_days`) that starts recording the day the
feature is deployed. Any app that existed before then needs a one-time backfill:

```bash
sauron-migrate backfill-person-days
```

Until you run it, apps older than the deploy show a card naming this command
rather than a grid. That is deliberate: an empty grid is indistinguishable from
"nobody ever came back", and answering 0% confidently is worse than declining to
answer.

Run `ANALYZE person_days;` afterwards. The backfill ships no statistics, and the
planner misestimates the new table until it has them.

Apps created after the deploy need nothing — every row they will ever have is
recorded live.

## The cohort grid

Each **row is one cohort**: everyone whose first-ever activity in this app fell
on that day (or in that ISO week). Each **column follows that same group
forward through their own calendar**, not yours.

| Cell | Meaning |
|---|---|
| **Users** | How many people joined in that period |
| **Day 0** | `100%` — by definition, they were active in the period they arrived |
| **Day N** | The share of that cohort active again N periods later |
| **hatched** | No answer: the period has not elapsed yet, or predates recorded data |

The single most important rule: **"Day 5" is a different date on every row.**
For a cohort that started on the 1st it is the 6th; for one that started on the
10th it is the 15th. That alignment is what makes the two reading directions
meaningful:

- **Across a row** — does this group stick, or drain away?
- **Down a column** — are newer cohorts doing better than older ones *at the
  same age*? That is where a product improvement shows up first.

Click any cell to switch the whole grid between percentages and user counts, or
**Export CSV** for raw counts (unelapsed periods export as empty fields, never
`0`, so a spreadsheet average is not poisoned by them).

### Hatched is never zero

Two different things are unknowable, and both render hatched rather than `0%`:

- **the period has not elapsed** — the newest cohorts have had less time, which
  is what makes the hatching a staircase;
- **the period predates recorded data** — cohorts come from `first_seen`, which
  is never pruned, while activity comes from `person_days`, which begins at the
  backfill and is pruned at 400 days. A period with no activity data behind it
  cannot be scored.

An elapsed period with data behind it and nobody returning is a true `0%`, and
is shown as one.

## Lifecycle

Each period's active people, split into **new** (first period), **returning**
(active last period too) and **resurrected** (back after a gap). Those three
partition the active set exactly. **Dormant** — active last period, silent this
one — is drawn below the axis and is not part of that partition.

This is the chart that catches churn-and-replace: a flat active-user line made
entirely of `new` is a treadmill, not growth. Hover any bar for the exact
figures.

## Compare users who hit an error

The toggle beside the granularity picker redraws the grid twice: once for people
who hit an error **in their first period**, once for everyone else.

Exposure is measured in the first period **only**, and that is what keeps the
comparison honest rather than circular — a user who churns immediately cannot
accumulate later errors, so splitting over the whole window would sort
short-lived users into the "clean" half by construction and manufacture the very
correlation the chart claims to find.

It remains an **association, not a cause**, and the card says so.

## Identified users only

The second toggle restricts every card — grid, lifecycle and at-risk — to people
your app has **named**: `identify()`, an event whose `context.user.id` equals its
`distinct_id`, or the migration-38 backfill. It is the same
`event_users.identified_at` column Active Users splits on, so the two pages
cannot disagree about who counts as a person.

**It defaults to off, and that is deliberate.** "Did the people who arrived come
back?" is a question about everyone, and guests are most of the arrivals.

Two things to keep in mind when you switch it on:

- **It selects for people who already converted.** Retention will read higher —
  sometimes dramatically — because everyone who arrived and left without signing
  up is excluded. That is the churn a retention grid exists to show.
- **It filters people, not periods.** Identification is retroactive: someone who
  browsed anonymously and signed up later is identified for their whole history,
  so their cohort is still their first anonymous sighting.

Where it genuinely helps: an identified id comes from your app, so it is stable
even when an anonymous one is not. If you are on an SDK older than 1.4.0 (see
below), this view gives you a trustworthy number today while you upgrade.

It needs `event_users.identified_at`, which arrives in migration 38; without it
the endpoint returns 503 naming `sauron-migrate` rather than quietly answering
for everyone.

## At risk

People who were active before and have been silent for the configured number of
periods, with their lifetime events, errors and sessions. Every column sorts
server-side (the silent population is far larger than one page, so sorting only
the loaded rows would mislead), each row expands for tenure and silence detail,
and the person id links to their profile.

## Insights

A computed reading of what is on screen — day-1 level and direction, the
first-timer share, the ratio of gained to dormant, any period where everyone
went silent, and the best cohort. Each finding carries a recommended next step,
and several link to the page that answers it.

Every statement is derived from the data on the page; nothing is estimated.

## If the numbers look impossibly low

Retention is only as meaningful as the identity behind it. **Check your SDK
version first:**

```bash
npm ls @edraj/sauron-browser
```

Before **1.4.0**, the browser SDK minted a fresh anonymous id on every page
load. With that build, a returning visitor arrives wearing a new `distinct_id`,
so they are counted as a brand-new person in a brand-new cohort and can never
appear as retained — daily cohorts balloon toward your page-load count and
retention collapses toward zero. Upgrading fixes it going forward; history
already recorded under the old ids cannot be repaired.

Two more checks, in the product:

- **Users → sort by session count.** If nearly every person has exactly one
  session, ids are not persisting.
- **Users → Stickiness (DAU/MAU).** If people genuinely use the product daily
  this sits at 30–60%; a few percent means the identities are not being reused.

Guests count as people — every `distinct_id` is one. Calling `identify()` on
login aliases the guest id to the real user, and the merge rewrites history, so
a person's pre-login activity joins their profile retroactively.

## Freshness and limits

- The grid and lifecycle are served **stale-while-revalidate**: under an hour
  old they are returned as-is, between one and three hours they are returned
  immediately while a single background refresh recomputes. The "as of" chip in
  the header states the age.
- `cohorts × periods` may not exceed 400 — the product is what the query walks,
  and bounding each dimension alone does not bound it.
- Days are **UTC**, matching Active Users.
