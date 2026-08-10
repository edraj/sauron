# Linking an alert rule to a specific monitor

Date: 2026-08-09
Status: approved, not yet implemented

## Problem

An alert rule with `trigger_type` `monitor_down` / `monitor_up` fires for **every
monitor in its project** (or in the whole org, when un-narrowed). There is no way
to say "page oncall on Slack when the payments API goes down, but only email me
about the other eleven monitors".

The current linkage is entirely scope-based:

- `alert_rules` (migration `2026-07-24-000019_alerting`) carries `org_id` plus
  optional `project_id` / `app_id` narrowing. No monitor dimension.
- The prober dispatches on a status transition
  (`backend/bins/sauron-monitor/src/main.rs:317`) through
  `repo::alert_rules_for_monitor(conn, m.project_id, trigger)`
  (`backend/crates/sauron-db/src/repo.rs:9684`), which matches every enabled rule
  in the monitor's org where `project_id IS NULL OR project_id = <monitor project>`.
- `Filters` (`backend/crates/sauron-alerts/src/rule.rs:107`) exposes
  level/environment/event_name/tag/op and is read only by the metric evaluator,
  never by the monitor path.

## Decision

Add a nullable `alert_rules.monitor_id` foreign key. `NULL` means "every monitor
in scope", mirroring how `project_id` / `app_id` already widen when NULL, so every
stored rule keeps its present behaviour.

### Alternatives rejected

- **`alert_rule_monitors` join table** (one rule → N monitors). Mirrors
  `alert_rule_channels`, but costs an extra table, a join in the hot dispatch
  path, and a multi-select UI. The point of per-monitor targeting is usually
  *differing* severity/channels per monitor, which is one rule per monitor anyway.
- **`conditions.filters.monitor_id` in JSONB.** No migration, but no foreign key:
  deleting a monitor leaves a rule that silently matches nothing forever, and the
  linkage is invisible to any query that reasons about monitors.

## Schema

New migration `backend/migrations/2026-08-09-000048_alert_rule_monitor`.

```sql
ALTER TABLE alert_rules
  ADD COLUMN monitor_id UUID REFERENCES monitors(id) ON DELETE CASCADE;

ALTER TABLE alert_rules ADD CONSTRAINT alert_rules_monitor_trigger_chk
  CHECK (monitor_id IS NULL OR trigger_type IN ('monitor_down','monitor_up'));

CREATE INDEX alert_rules_monitor_idx ON alert_rules (monitor_id)
  WHERE monitor_id IS NOT NULL;
```

`down.sql` drops the index, the constraint, and the column.

**`ON DELETE CASCADE`, not `SET NULL`.** `SET NULL` silently *widens* a rule:
delete the payments monitor and a critical-severity pager rule quietly begins
firing for all twelve monitors. A rule that exists only to watch one monitor
should be removed with it.

**The CHECK constraint** prevents a `monitor_id` being attached to a trigger that
will never read it (e.g. `issue_new`) — the dead-configuration failure mode that
disqualified the JSONB alternative.

Cross-table consistency (`monitor_id`'s project must equal `project_id`) cannot be
expressed as a CHECK; it is enforced in the API by deriving `project_id` from the
monitor, exactly as `app_id` derives its project today.

## Backend

1. `AlertRule` and `NewAlertRule` (`backend/crates/sauron-db/src/models.rs:994`)
   and `backend/crates/sauron-db/src/schema.rs` gain
   `monitor_id: Option<Uuid>`.

2. `repo::alert_rules_for_monitor` takes an additional `monitor_id: Uuid` and adds
   one filter: `monitor_id IS NULL OR monitor_id = $monitor`. The prober call site
   passes `m.id`. This is the whole dispatch change.

3. `CreateRuleReq` (`backend/bins/sauron-api/src/routes/notifications.rs:516`)
   gains `monitor_id: Option<Uuid>`. `check_rule_scope` grows a monitor arm that
   verifies the monitor's project belongs to `org_id` and returns that project as
   the rule's `project_id`. A supplied `project_id` that disagrees with the
   monitor's is a 400, matching the existing app/project mismatch arm.

4. Authorization needs no new gate. `authorize_rule_target` already drops `app_id`
   for monitor triggers and checks `perm::MONITOR_READ`. Because step 3 derives
   the project, a monitor-narrowed rule lands on the `(None, Some(project))` arm —
   strictly narrower than the org arm it would otherwise hit, never looser.

   The explanatory comment at `notifications.rs:617` currently states that
   `alert_rules_for_monitor` "never looks at" per-monitor narrowing and that an
   app-narrowed monitor rule still fires for every monitor in its project. The
   first half stops being true; the comment must be updated so it does not
   mis-describe the gate it guards.

5. `UpdateRuleReq` is unchanged. Scope is fixed at creation — the established
   convention, and what the UI already tells the user.

6. `enqueue_personal_uptime` is untouched. It runs before rule lookup and is
   deliberately independent of whether any rule exists.

## Dashboard

`dashboard/src/pages/Alerts.svelte`:

- A **Monitor** field rendered only for `monitor_down` / `monitor_up`, gated
  through the existing `triggerNeeds` derived.
- Options come from `listMonitors(projectId)` using the session's already-selected
  project. No new endpoint: `/v1/projects/{id}/monitors` already exists and is
  already in the API client.
- Default option "All monitors in this project" submits no `monitor_id`.
- Disabled while editing an existing rule, like the Trigger select.
- The rules table shows the pinned monitor's name alongside the trigger label.

If no project is selected in the session, the field renders disabled with an
explanatory hint rather than an empty dropdown; the rule can still be created
un-narrowed.

## Testing

- `alert_rules_for_monitor` against real Postgres: a rule pinned to monitor A does
  not match monitor B; a `NULL` rule matches both; a disabled pinned rule matches
  neither.
- HTTP: creating a rule whose `monitor_id` belongs to another org returns 400.
- HTTP: a user holding `MONITOR_READ` on project X only cannot pin a rule to a
  monitor in project Y (403).
- HTTP: `monitor_id` on a non-monitor trigger is rejected (400 from the API, with
  the CHECK constraint as the backstop).
- Migration up/down round-trips cleanly.

## Out of scope

- Multi-monitor rules (see rejected alternatives).
- Re-pointing a rule's scope after creation.
- Back-filling existing rules — they keep `NULL` and behave exactly as today.
