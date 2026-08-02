# Notifications

Sauron sends two kinds of email. **Alerts** are configured by an organization
admin and go to org-wide channels. **Personal notifications** — this page — are
configured by you, go to your own address, and nobody else can see or change
them.

## What you can subscribe to

| Kind | Scope | Environment filter | Fires when |
|---|---|---|---|
| Uptime | A project | Not applicable | A monitor in that project goes down or recovers |
| Error rate increasing | A project or one app | Yes | Errors in the last window are at least `min_count` **and** either the previous window was empty or the count is at least `factor` times it |
| New issue | A project or one app | Yes | An issue is seen for the first time |
| Issue regressed | A project or one app | Yes | A resolved or ignored issue starts erroring again |

Uptime has no environment filter and no app scope because a monitor belongs to a
whole project — it has no app or environment of its own.

### "Error rate increasing" in detail

With a window `W`:

- `C` = error count over the last `W`
- `B` = error count over the `W` before that
- It fires when `C >= min_count` **and** (`B = 0` **or** `C >= B × factor`).

The `B = 0` case matters: an app that was silent and is now on fire is exactly
the situation you want to hear about. The `min_count` floor matters too — without
it, 1 error becoming 3 is a 3× spike and would wake you up.

Defaults: window 15 minutes, factor 3×, minimum 10 errors. Ranges: window
300–86400 seconds, factor 1.5–100, minimum 1–100000.

## Environments

The environment chips list your **project's** environments — `prod`, `staging`,
and so on. Ticking `prod` means "prod, in every app in scope", and it keeps
meaning that when a new app is added to the project later.

Leaving every chip unticked means all environments, **including** events that
arrived with no environment attached.

## Noise control

| Control | What it does |
|---|---|
| **Throttle** (default 15 minutes) | The same notification is not repeated inside this window |
| **Delivery** | *As it happens*, *hourly summary*, or *daily summary* |
| **Quiet hours** | Notifications raised inside the window are **held**, not dropped, and arrive when it ends |
| Per-hour cap | Above 20 emails an hour, the rest are merged into one digest — never discarded. The card shows the delivery you are actually getting |
| Maximum subscriptions | 50 per person |

Quiet hours defer rather than drop on purpose: a night-time outage is still an
outage, and "quiet" must never be indistinguishable from "broken".

## Unsubscribing

Every notification email carries an unsubscribe link. Opening it turns off that
one subscription and sends you a short confirmation email saying so — that
confirmation is deliberate, so a silencing is never invisible to you. Links stay
valid for 90 days and a fresh one is minted with every message.

## Why a subscription can turn itself off

A subscription only ever delivers telemetry you are already allowed to read. If
your access to its project or app is removed, the subscription disables itself
and the card says "Off — access removed". Getting access back does not silently
turn it on again: you turn it back on yourself.

## What this does not do

- It does not deliver to Slack, Discord, a webhook or Telegram. Those are
  org-level alert channels an admin configures.
- It does not narrow uptime below a project.
- It does not notify on analytics event volume or latency percentiles. Those are
  team dashboards, not personal inboxes.
