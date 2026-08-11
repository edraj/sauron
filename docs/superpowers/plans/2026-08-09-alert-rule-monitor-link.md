# Alert Rule → Monitor Link Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an alert rule target one specific monitor instead of every monitor in its project.

**Architecture:** A nullable `alert_rules.monitor_id` foreign key, where `NULL` keeps today's "all monitors in scope" behaviour. The prober's rule lookup gains one filter; the API derives the rule's `project_id` from the monitor so the existing authorization gate tightens automatically; the dashboard gains one conditional dropdown.

**Tech Stack:** Rust (axum, diesel-async, Postgres), Svelte 5 (runes), Vitest.

## Global Constraints

- **Never commit.** No task in this plan ends in `git commit`, `git add`, or branch creation. Leave every change in the working tree.
- **Another agent is working in this repository concurrently.** Prefer surgical `Edit` over whole-file `Write`. Before creating the migration directory, re-check the highest existing number under `backend/migrations/` and use the next free one — the plan says `2026-08-09-000048`, but bump it if that is taken.
- `NULL` semantics are load-bearing: `monitor_id IS NULL` must continue to mean "every monitor in scope". No existing row may change behaviour.
- Integration tests skip (not fail) when `TEST_DATABASE_URL` / `TEST_REDIS_URL` are unset. Follow that pattern.
- Spec: `docs/superpowers/specs/2026-08-09-alert-rule-monitor-link-design.md`.

---

### Task 1: Schema — migration, `schema.rs`, models

**Files:**
- Create: `backend/migrations/2026-08-09-000048_alert_rule_monitor/up.sql`
- Create: `backend/migrations/2026-08-09-000048_alert_rule_monitor/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs:468-486` (the `alert_rules` `table!` block)
- Modify: `backend/crates/sauron-db/src/models.rs:994-1010` (`AlertRule`), `:1012-1026` (`NewAlertRule`)

**Interfaces:**
- Consumes: nothing.
- Produces: `AlertRule.monitor_id: Option<Uuid>` and `NewAlertRule.monitor_id: Option<Uuid>`; the column `alert_rules.monitor_id`.

- [ ] **Step 1: Write `up.sql`**

```sql
-- Pin a monitor alert rule to ONE monitor. NULL keeps the existing meaning:
-- every monitor in the rule's scope, exactly as every stored row behaves today.
ALTER TABLE alert_rules
  ADD COLUMN monitor_id UUID REFERENCES monitors(id) ON DELETE CASCADE;

-- CASCADE, not SET NULL, deliberately. SET NULL would silently WIDEN a rule:
-- delete the one monitor a critical-severity pager rule watches and it would
-- quietly begin firing for every monitor in the project. A rule that exists
-- only to watch one monitor should be removed with it.

-- A monitor_id on any other trigger is dead configuration nothing ever reads.
ALTER TABLE alert_rules ADD CONSTRAINT alert_rules_monitor_trigger_chk
  CHECK (monitor_id IS NULL OR trigger_type IN ('monitor_down','monitor_up'));

CREATE INDEX alert_rules_monitor_idx ON alert_rules (monitor_id)
  WHERE monitor_id IS NOT NULL;
```

- [ ] **Step 2: Write `down.sql`**

```sql
DROP INDEX IF EXISTS alert_rules_monitor_idx;
ALTER TABLE alert_rules DROP CONSTRAINT IF EXISTS alert_rules_monitor_trigger_chk;
ALTER TABLE alert_rules DROP COLUMN IF EXISTS monitor_id;
```

- [ ] **Step 3: Add the column to `schema.rs`**

In the `alert_rules` `table!` block, add `monitor_id` immediately after `app_id`:

```rust
        project_id -> Nullable<Uuid>,
        app_id -> Nullable<Uuid>,
        monitor_id -> Nullable<Uuid>,
        name -> Text,
```

Then add the joinable next to the existing `alert_rules` joinable near `schema.rs:785`:

```rust
diesel::joinable!(alert_rules -> monitors (monitor_id));
```

**Critical:** `Queryable` decodes positionally. The `table!` column order and the struct field order must match, and the ALTER TABLE appends the physical column at the END. Diesel's `Selectable`/`as_select()` names columns explicitly, so declaration order in `table!` is what matters — but the struct field order below must match the `table!` order, not the physical order.

- [ ] **Step 4: Add the field to both structs**

`AlertRule`, after `app_id`:

```rust
    pub app_id: Option<Uuid>,
    pub monitor_id: Option<Uuid>,
```

`NewAlertRule`, after `app_id`:

```rust
    pub app_id: Option<Uuid>,
    pub monitor_id: Option<Uuid>,
```

- [ ] **Step 5: Verify it compiles and the migration round-trips**

```bash
cd backend && cargo check -p sauron-db
```

Expected: clean. Any error naming a *different* `NewAlertRule` construction site means Task 3's file also needs the field — note it, it is covered there.

```bash
cd backend && cargo test -p sauron-db --test schema_drift
```

Expected: PASS (or "skipping" if `TEST_DATABASE_URL` is unset). This gate is what catches a `schema.rs` block that disagrees with the migrations.

---

### Task 2: Dispatch — `alert_rules_for_monitor` honours `monitor_id`

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs:9684-9710`
- Modify: `backend/bins/sauron-monitor/src/main.rs:317`
- Test: `backend/crates/sauron-db/tests/notifications.rs`

**Interfaces:**
- Consumes: `AlertRule.monitor_id` from Task 1.
- Produces: `repo::alert_rules_for_monitor(conn, project_id: Uuid, monitor_id: Uuid, trigger_type: &str) -> QueryResult<Vec<AlertRule>>` — note the **new third-position `monitor_id` parameter**, with `trigger_type` moving to fourth.

- [ ] **Step 1: Write the failing test**

Append to `backend/crates/sauron-db/tests/notifications.rs`. It needs a monitor and two rules, so it inserts them directly rather than through the API.

```rust
/// The harness seeds no monitors, and `monitor_id` is a foreign key, so the
/// test has to make its own. Inserted directly rather than through any repo
/// helper: the write path is not what is under test here.
async fn insert_monitor(
    conn: &mut sauron_db::AsyncPgConnection,
    project_id: uuid::Uuid,
    name: &str,
) -> uuid::Uuid {
    diesel::insert_into(sauron_db::schema::monitors::table)
        .values((
            sauron_db::schema::monitors::project_id.eq(project_id),
            sauron_db::schema::monitors::name.eq(name),
            sauron_db::schema::monitors::kind.eq("http"),
            sauron_db::schema::monitors::target.eq("https://example.test/health"),
        ))
        .returning(sauron_db::schema::monitors::id)
        .get_result(conn)
        .await
        .expect("insert monitor")
}

/// A rule pinned to one monitor must not fire for a sibling monitor in the same
/// project — the whole point of the column. The un-pinned rule in the same
/// fixture is the control: it proves the filter narrows rather than just
/// breaking the query, which a single-rule test cannot distinguish.
#[tokio::test]
async fn a_monitor_pinned_rule_matches_only_that_monitor() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let monitor_a = insert_monitor(&mut conn, ids.project_id, "mon-a").await;
    let monitor_b = insert_monitor(&mut conn, ids.project_id, "mon-b").await;

    let conditions = json!({});
    let pinned = sauron_db::repo::create_alert_rule(
        &mut conn,
        sauron_db::models::NewAlertRule {
            org_id: ids.org_id,
            project_id: Some(ids.project_id),
            app_id: None,
            monitor_id: Some(monitor_a),
            name: "pinned to A",
            trigger_type: "monitor_down",
            conditions: &conditions,
            severity: "critical",
            throttle_seconds: 300,
            message_template: None,
            last_evaluated_at: None,
            created_by: None,
        },
    )
    .await
    .expect("create pinned rule");

    let wide = sauron_db::repo::create_alert_rule(
        &mut conn,
        sauron_db::models::NewAlertRule {
            org_id: ids.org_id,
            project_id: Some(ids.project_id),
            app_id: None,
            monitor_id: None,
            name: "all monitors",
            trigger_type: "monitor_down",
            conditions: &conditions,
            severity: "warning",
            throttle_seconds: 300,
            message_template: None,
            last_evaluated_at: None,
            created_by: None,
        },
    )
    .await
    .expect("create wide rule");

    let for_a =
        sauron_db::repo::alert_rules_for_monitor(&mut conn, ids.project_id, monitor_a, "monitor_down")
            .await
            .expect("rules for A");
    let mut a_ids: Vec<_> = for_a.iter().map(|r| r.id).collect();
    a_ids.sort();
    let mut expected = vec![pinned.id, wide.id];
    expected.sort();
    assert_eq!(a_ids, expected, "monitor A gets both the pinned and wide rule");

    let for_b =
        sauron_db::repo::alert_rules_for_monitor(&mut conn, ids.project_id, monitor_b, "monitor_down")
            .await
            .expect("rules for B");
    assert_eq!(
        for_b.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![wide.id],
        "monitor B must NOT receive the rule pinned to monitor A"
    );
}
```

Add `use sauron_db::schema::monitors;` only if the file does not already glob it; the snippet above uses fully-qualified paths so no new import is required.

- [ ] **Step 2: Run it and confirm it fails**

```bash
cd backend && cargo test -p sauron-db --test notifications a_monitor_pinned_rule_matches_only_that_monitor
```

Expected: FAIL to **compile** — `alert_rules_for_monitor` takes 3 arguments, not 4. That is the correct first failure.

- [ ] **Step 3: Add the filter to the repo function**

In `repo.rs:9684`, change the signature and add one filter clause:

```rust
pub async fn alert_rules_for_monitor(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    monitor_id: Uuid,
    trigger_type: &str,
) -> QueryResult<Vec<AlertRule>> {
```

and, after the existing `project_id` filter, before `.select(...)`:

```rust
        // A rule with a NULL `monitor_id` covers every monitor in its scope —
        // the same widening `project_id` already uses, so every rule stored
        // before this column existed keeps firing exactly as it did.
        .filter(
            alert_rules::monitor_id
                .is_null()
                .or(alert_rules::monitor_id.eq(monitor_id)),
        )
```

- [ ] **Step 4: Update the prober call site**

`backend/bins/sauron-monitor/src/main.rs:317`:

```rust
    let rules = match repo::alert_rules_for_monitor(&mut conn, m.project_id, m.id, trigger).await {
```

- [ ] **Step 5: Update the pre-existing caller in the test file**

`backend/crates/sauron-db/tests/notifications.rs:1193` currently passes three arguments. It asserts the harness configures no rules, so any monitor id satisfies it:

```rust
    let rules = sauron_db::repo::alert_rules_for_monitor(
        &mut conn,
        ids.project_id,
        uuid::Uuid::from_u128(7),
        "monitor_down",
    )
    .await
    .expect("load rules");
```

- [ ] **Step 6: Run the tests**

```bash
cd backend && cargo test -p sauron-db --test notifications
```

Expected: PASS, including `a_project_with_zero_alert_rules_still_has_uptime_subscribers`.

```bash
cd backend && cargo check -p sauron-monitor
```

Expected: clean.

---

### Task 3: API — accept and validate `monitor_id`

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/notifications.rs` — `CreateRuleReq` (`:516`), `check_rule_scope` (`:538`), the `authorize_rule_target` doc comment (`:617-650`), `create_rule` (`:746`)
- Test: `backend/bins/sauron-api/tests/http_alerting.rs`

**Interfaces:**
- Consumes: `NewAlertRule.monitor_id` (Task 1), the dispatch semantics from Task 2.
- Produces: `POST /v1/orgs/{org_id}/alert-rules` accepts an optional `monitor_id`; the rule JSON returned by `rule_view` now carries `monitor_id` (it serialises `AlertRule` directly, so this is automatic).
- New helper signature: `check_rule_scope(conn, org_id, project_id, app_id, monitor_id) -> Result<(Option<Uuid>, Option<Uuid>, Option<Uuid>), ApiError>` returning `(project_id, app_id, monitor_id)`.

- [ ] **Step 1: Write the failing tests**

Append to `backend/bins/sauron-api/tests/http_alerting.rs`. Helper first — the fixture has no monitor:

```rust
/// The `Fixture` seeds projects and apps but no monitors, and a pinned rule
/// needs a real one because the column is a foreign key.
async fn create_monitor(server: &TestServer, token: &str, project_id: Uuid, name: &str) -> Uuid {
    let (_text, body) = server
        .post_ok(
            &format!("/v1/projects/{project_id}/monitors"),
            token,
            json!({
                "name": name,
                "kind": "http",
                "target": "https://example.test/health",
            }),
        )
        .await;
    body["id"].as_str().unwrap().parse().unwrap()
}

/// A monitor from another org must never become a rule's target: the rule's
/// `project_id` is DERIVED from the monitor, so accepting a foreign one would
/// hand the caller a rule scoped outside the org they authorized against.
#[tokio::test]
async fn a_rule_cannot_be_pinned_to_a_monitor_outside_the_org() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "pinorg").await;
    let other = seed(&server, "pinorg-other").await;

    let foreign = create_monitor(&server, &other.owner_token, other.project_a, "foreign").await;

    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.owner_token),
            json!({
                "name": "cross-org",
                "trigger_type": "monitor_down",
                "monitor_id": foreign,
            }),
        )
        .await;
    assert_eq!(status, 400, "monitor from another org must be rejected: {text}");

    server.shutdown().await;
}

/// `monitor_id` is meaningless on a trigger that never reads it. Rejecting at
/// the API keeps the CHECK constraint as a backstop rather than the only guard
/// — a 500 from a constraint violation is not an answer a caller can act on.
#[tokio::test]
async fn a_monitor_id_on_a_non_monitor_trigger_is_rejected() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "pintrigger").await;
    let mon = create_monitor(&server, &fx.owner_token, fx.project_a, "api").await;

    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.owner_token),
            json!({
                "name": "wrong trigger",
                "trigger_type": "issue_new",
                "monitor_id": mon,
            }),
        )
        .await;
    assert_eq!(status, 400, "monitor_id on issue_new must be rejected: {text}");

    server.shutdown().await;
}

/// Pinning must narrow, never widen: the derived `project_id` is what the
/// existing `authorize_rule_target` gate then checks `monitor:read` against.
#[tokio::test]
async fn pinning_a_monitor_derives_the_project_and_is_authorized_there() {
    let Some(mut server) = TestServer::start().await else {
        return;
    };
    let fx = seed(&server, "pinderive").await;
    let mon_b = create_monitor(&server, &fx.owner_token, fx.project_b, "b-health").await;

    // Mallory can read project A only, so a rule pinned to a project B monitor
    // must be refused even though she never named project B in the request.
    let (status, text, _) = server
        .post_raw(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            Some(&fx.mallory_token),
            json!({
                "name": "sneaky",
                "trigger_type": "monitor_down",
                "monitor_id": mon_b,
            }),
        )
        .await;
    assert_eq!(status, 403, "pinning must be authorized at the monitor's project: {text}");

    // And the owner's pinned rule stores both the monitor and the derived project.
    let (_text, body) = server
        .post_ok(
            &format!("/v1/orgs/{}/alert-rules", fx.org_id),
            &fx.owner_token,
            json!({
                "name": "b down",
                "trigger_type": "monitor_down",
                "monitor_id": mon_b,
            }),
        )
        .await;
    assert_eq!(body["monitor_id"].as_str().unwrap().parse::<Uuid>().unwrap(), mon_b);
    assert_eq!(
        body["project_id"].as_str().unwrap().parse::<Uuid>().unwrap(),
        fx.project_b,
        "the project must be derived from the monitor, not left NULL"
    );

    server.shutdown().await;
}
```

- [ ] **Step 2: Run them and confirm they fail**

```bash
cd backend && cargo test -p sauron-api --test http_alerting pinn
```

Expected: FAIL. `a_monitor_id_on_a_non_monitor_trigger_is_rejected` and the cross-org test return 200 because `monitor_id` is an unknown field silently dropped by serde; `pinning_a_monitor_derives_the_project_and_is_authorized_there` fails on the 403 assertion for the same reason.

- [ ] **Step 3: Add the request field**

In `CreateRuleReq` (`:516`), after `app_id`:

```rust
    #[serde(default)]
    pub app_id: Option<Uuid>,
    /// Narrow a monitor trigger to ONE monitor. `None` = every monitor in scope.
    #[serde(default)]
    pub monitor_id: Option<Uuid>,
```

- [ ] **Step 4: Extend `check_rule_scope`**

Replace the signature and add the monitor arm ahead of the existing match. The monitor case derives the project the same way the app case does:

```rust
async fn check_rule_scope(
    conn: &mut sauron_db::AsyncPgConnection,
    org_id: Uuid,
    project_id: Option<Uuid>,
    app_id: Option<Uuid>,
    monitor_id: Option<Uuid>,
) -> Result<(Option<Uuid>, Option<Uuid>, Option<Uuid>), ApiError> {
    // A monitor pins the rule to exactly one target, and `monitors` carries only
    // `project_id` — so the monitor DERIVES the project, exactly as an app does
    // below. Deriving rather than trusting the caller's `project_id` is what
    // makes `authorize_rule_target` check `monitor:read` at the radius the rule
    // will actually fire over.
    if let Some(m) = monitor_id {
        let Some(proj) = repo::monitor_project(conn, m).await? else {
            return Err(ApiError::BadRequest("monitor not found".into()));
        };
        if repo::project_org(conn, proj).await? != Some(org_id) {
            return Err(ApiError::BadRequest("monitor is not in this org".into()));
        }
        if let Some(p) = project_id {
            if p != proj {
                return Err(ApiError::BadRequest(
                    "monitor does not belong to the given project".into(),
                ));
            }
        }
        // A monitor trigger has no app dimension; carrying one would be a
        // narrowing that never applies.
        return Ok((Some(proj), None, Some(m)));
    }
    match (project_id, app_id) {
        (None, None) => Ok((None, None, None)),
        (Some(p), None) => {
            if repo::project_org(conn, p).await? != Some(org_id) {
                return Err(ApiError::BadRequest("project is not in this org".into()));
            }
            Ok((Some(p), None, None))
        }
        (maybe_p, Some(a)) => match repo::app_ancestry(conn, a).await? {
            Some((proj, o)) if o == org_id => {
                if let Some(p) = maybe_p {
                    if p != proj {
                        return Err(ApiError::BadRequest(
                            "app does not belong to the given project".into(),
                        ));
                    }
                }
                Ok((Some(proj), Some(a), None))
            }
            _ => Err(ApiError::BadRequest("app is not in this org".into())),
        },
    }
}
```

- [ ] **Step 5: Wire `create_rule`**

In `create_rule` (`:746`), after the `trigger` is parsed and before the DB work, reject the wrong-trigger case:

```rust
    if req.monitor_id.is_some()
        && !matches!(
            trigger,
            TriggerType::MonitorDown | TriggerType::MonitorUp
        )
    {
        return Err(ApiError::BadRequest(
            "monitor_id applies only to monitor_down / monitor_up triggers".into(),
        ));
    }
```

Then change the scope call and the insert:

```rust
    let (project_id, app_id, monitor_id) =
        check_rule_scope(&mut conn, org_id, req.project_id, req.app_id, req.monitor_id).await?;
```

and inside `NewAlertRule { .. }`, after `app_id`:

```rust
            app_id,
            monitor_id,
```

- [ ] **Step 6: Correct the stale security comment**

`authorize_rule_target`'s doc comment (`:617-650`) currently asserts that `repo::alert_rules_for_monitor` "never looks at" per-monitor narrowing and that an app-narrowed monitor rule fires for every monitor in its project. Half of that is no longer true. Replace the paragraph beginning "Monitor triggers have no app dimension at FIRING time" with:

```rust
    // Monitor triggers have no APP dimension at firing time, so authorizing one
    // at app scope would be narrower than what the rule actually delivers.
    // `monitors` carries only `project_id` (no `app_id`, no `environment_id`),
    // so the app narrowing is dropped here to check at the radius that applies.
    //
    // Monitor narrowing is different and IS honoured: `alert_rules.monitor_id`
    // is filtered by `repo::alert_rules_for_monitor`, and `check_rule_scope`
    // derives `project_id` from the pinned monitor — so a pinned rule arrives
    // here on the `(None, Some(project))` arm. That is strictly narrower than
    // the org arm it would otherwise take, never looser, which is why pinning
    // needs no additional gate of its own.
    //
    // Same fact `SubKind::allows_app_scope` encodes for personal uptime
    // subscriptions, but the remedy differs: subscriptions refuse app scope
    // outright, while rules accept-and-widen. Refusing would 400 every
    // app-narrowed monitor rule already stored — including on an unrelated
    // rename — and the widened check is already the strict reading.
    //
    // A hand-inserted row with `app_id` but a NULL `project_id` (nothing the
    // API can produce: `check_rule_scope` derives the project from the app)
    // falls through to the org arm, which is stricter still. Fail-safe.
```

- [ ] **Step 7: Run the tests**

```bash
cd backend && cargo test -p sauron-api --test http_alerting
```

Expected: PASS, all cases including the pre-existing D6/D7/D9 ones (or "skipping" without `TEST_DATABASE_URL` / `TEST_REDIS_URL`).

```bash
cd backend && cargo clippy --workspace --all-targets -- -D warnings
```

Expected: clean.

---

### Task 4: Dashboard — the Monitor dropdown

**Files:**
- Modify: `dashboard/src/pages/Alerts.svelte`
- Modify: `dashboard/src/lib/api/alerts.ts` (declares the rule request/response types)

**Interfaces:**
- Consumes: `POST /v1/orgs/{id}/alert-rules` accepting `monitor_id` (Task 3); `listMonitors(projectId): Promise<MonitorListItem[]>` from `dashboard/src/lib/api/monitors.ts:10`.
- Produces: no exports other tasks depend on.

- [ ] **Step 1: Add `monitor_id` to the rule types**

In the API client module, add `monitor_id?: string | null` to the `AlertRule` (response) type and to the create-rule request type, alongside the existing `project_id` / `app_id`.

- [ ] **Step 2: Add state and loading to `Alerts.svelte`**

Next to the existing `rTrigger` declaration (`:88`):

```ts
  let rMonitor = $state<string>('');
  let monitorOptions = $state<MonitorListItem[]>([]);
```

Import `listMonitors` and the `MonitorListItem` type from `../lib/api/monitors`. Load the list when the form opens, guarded on the session's selected project:

```ts
  /**
   * `/v1/projects/{id}/monitors` is project-scoped, and the Alerts page has no
   * project selector of its own — it uses the session's. With no project
   * selected the field is disabled rather than empty-and-clickable, and the
   * rule can still be created un-narrowed.
   */
  async function loadMonitorOptions() {
    const pid = projectId;
    if (!pid) { monitorOptions = []; return; }
    try {
      monitorOptions = await listMonitors(pid);
    } catch {
      monitorOptions = [];
    }
  }
```

Call it from `openNewRule` and reset `rMonitor = ''` there (`:173`) and in the edit path (`:192`), where it becomes `rMonitor = r.monitor_id ?? ''`.

- [ ] **Step 3: Gate the field through `triggerNeeds`**

In `triggerNeeds` (`:220`) add a key:

```ts
    monitor: t === 'monitor_down' || t === 'monitor_up',
```

- [ ] **Step 4: Render the field**

Inside the form grid, immediately after the Trigger field's closing `</div>`:

```svelte
            {#if needs.monitor}
              <div class="field">
                <label class="lbl" for="r-monitor">Monitor</label>
                <div class="control select">
                  <select
                    id="r-monitor"
                    bind:value={rMonitor}
                    disabled={editingRuleId !== null || !projectId}
                  >
                    <option value="">All monitors in this project</option>
                    {#each monitorOptions as m (m.id)}
                      <option value={m.id}>{m.name}</option>
                    {/each}
                  </select>
                  <span class="affix"><Icon name="chevron-down" size={15} /></span>
                </div>
                {#if !projectId}
                  <p class="muted small">Select a project to pin this rule to one monitor.</p>
                {/if}
              </div>
            {/if}
```

- [ ] **Step 5: Send it on create only**

In the create branch (`:424`), alongside `trigger_type: rTrigger`:

```ts
          monitor_id: rMonitor || undefined,
```

Do **not** add it to the update branch: scope is immutable after creation, which the form already states at `:794`.

- [ ] **Step 6: Show it in the rules table**

At the trigger cell (`:981`):

```svelte
                <td>
                  {TRIGGER_LABELS[r.trigger_type] ?? r.trigger_type}
                  {#if r.monitor_id}
                    <span class="muted small">
                      · {monitorOptions.find((m) => m.id === r.monitor_id)?.name ?? 'pinned monitor'}
                    </span>
                  {/if}
                </td>
```

- [ ] **Step 7: Verify**

```bash
cd dashboard && npm run check && npm test
```

Expected: type-check clean, existing suite green. Then drive the real page per the harness verify pattern: open the Alerts page, create a `monitor_down` rule pinned to one monitor, confirm the created rule's `monitor_id` and derived `project_id` in the network response, and confirm the field disappears when the trigger is switched to `issue_new`.

---

## Verification (whole feature)

- [ ] `cd backend && cargo clippy --workspace --all-targets -- -D warnings` — clean
- [ ] `cd backend && cargo test -p sauron-db -p sauron-api -p sauron-alerts` — green
- [ ] `cd dashboard && npm run check && npm test` — green
- [ ] Migration rolls back and forward: `sauron-migrate` down then up against a scratch database
- [ ] End-to-end: two monitors in one project, a rule pinned to the first, take the first down, confirm exactly one delivery and that the second monitor going down does not fire the pinned rule
