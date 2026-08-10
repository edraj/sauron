# Devices Grouped by Model and OS — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse the Devices inventory into one row per `(family, model, os_name, os_version)` tuple, aggregated in SQL, with a drill-down to the flat device list filtered to that tuple.

**Architecture:** A new `repo::list_device_groups` reuses `list_devices`' qualifying-devices subquery unpaged, joins the same environment-scoped LATERALs, and `GROUP BY`s the four key columns with `LIMIT/OFFSET` moved to the group level. `list_devices` gains an optional exact-match group filter for the drill-down. `/devices` stays one route with two querystring-driven modes.

**Tech Stack:** Rust (axum, diesel-async, raw `sql_query`), Postgres, Svelte 5 (runes), TypeScript, vitest.

**Spec:** [`docs/superpowers/specs/2026-08-09-devices-grouped-by-model-and-os-design.md`](../specs/2026-08-09-devices-grouped-by-model-and-os-design.md)

## Global Constraints

- **Do not commit, and do not create branches.** This repository has a standing no-commit rule. The usual "Step N: Commit" from the plan template is deliberately replaced with a verification step in every task below. Leave all work in the working tree. Do not reintroduce `git commit` / `git checkout -b` steps.
- **Environment scoping is a security boundary, not a filter.** Every read added here goes through `ReadScope`. `environment_id` is read from the raw query string via `RawQuery` + `scope::authorized_read_scope`, **never** as a field on a `Query<T>` extractor struct — see the module docs in `backend/bins/sauron-api/src/routes/scope.rs` for the extractor trap.
- **`EnvFilter` has four variants:** `All`, `One`, `Subset`, `Unattributed`. The "durable columns vs LATERAL" split keys on `matches!(env, EnvFilter::All)` only — `Subset` belongs on the scoped side with `One`.
- **Bind indices:** `EnvFilter::sql_fragment(n)` consumes index `n` only for `One` and `Subset`. Use `EnvFilter::consumes_bind()` to compute what comes after it. Getting this wrong shifts every later bind.
- **Rust tests skip, not fail, without a database.** Every `sauron-db` integration test begins `let Some(db) = TestDb::setup().await else { return; };`. CI has no Postgres service.
- **`os_version` granularity is full, not major.** `iOS 17.4.1` and `iOS 17.4.0` are separate groups.
- **`browser` and `arch` are not part of the grouping key** and are not columns on the grouped table.

---

## File Structure

**Backend**

| File | Responsibility |
|---|---|
| `backend/crates/sauron-db/src/repo.rs` | Add `DeviceGroupRow`, `DeviceGroupKey`, `list_device_groups`; extend `list_devices` with the group filter. |
| `backend/bins/sauron-api/src/routes/devices.rs` | Add the `device_groups` handler; add group filter params to `ListQuery`. |
| `backend/bins/sauron-api/src/main.rs:635` | Register `GET /v1/apps/{app_id}/device-groups`. |
| `backend/crates/sauron-db/tests/device_groups.rs` | **New.** Dedicated fixture + tests for grouping and the drill-down filter. |
| `backend/bins/sauron-api/tests/http_env_scoping.rs` | Append two route-level tests — permission gate, env scoping, and the `group=1` sentinel. |

A **new** test file rather than extending `crates/sauron-db/tests/env_scoping.rs`: that file's `TestDb::seed_two_envs` fixture is asserted on by roughly a dozen tests, including exact row counts (`assert_eq!(count_rows(&mut conn, "devices", ids.app_id).await, 8)`). Grouping tests need devices with *controlled, colliding* `family`/`model`/`os_name`/`os_version` values, and adding them to the shared fixture would break those counts. `crates/sauron-db/tests/common/mod.rs` already establishes the precedent for a deliberately separate fixture — see the doc comment on `CrossEnvSessionIds`.

**Frontend**

| File | Responsibility |
|---|---|
| `dashboard/src/lib/models/device-groups.ts` | **New.** `DeviceGroupKey` + pure `encodeGroupKey`/`decodeGroupKey`/`groupLabel`. |
| `dashboard/src/lib/models/device-groups.test.ts` | **New.** URL round-trip unit tests, NULL and empty-string cases. |
| `dashboard/src/lib/models/index.ts:610` | Add `DeviceGroupRow` beside `DeviceRow`. |
| `dashboard/src/lib/api/devices.ts` | Add `listDeviceGroups`; extend `ListDevicesParams`. |
| `dashboard/src/lib/components/devices/DeviceFlatTable.svelte` | **New.** Today's device rows, extracted verbatim. |
| `dashboard/src/lib/components/devices/DeviceGroupTable.svelte` | **New.** Grouped rows. |
| `dashboard/src/pages/DevicesInventory.svelte` | Mode selection, fetching, loading/error/empty states. |

---

## Task 1: `list_device_groups` in the repo layer

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (add after `list_devices`, which ends at line 5824)
- Test: `backend/crates/sauron-db/tests/device_groups.rs` (create)

**Interfaces:**
- Consumes: `ReadScope`, `EnvFilter`, `like_contains`, `bind_env!` — all existing in `sauron-db`.
- Produces:
  ```rust
  // Extracted from list_devices in Step 0 and shared by both queries.
  fn device_membership_sql(env: &EnvFilter, bind_index: usize) -> String;

  pub struct DeviceGroupRow {
      pub family: Option<String>,
      pub model: Option<String>,
      pub os_name: Option<String>,
      pub os_version: Option<String>,
      pub device_count: i64,
      pub events_count: i64,
      pub errors_count: i64,
      pub sessions_count: i64,
      pub first_seen: DateTime<Utc>,
      pub last_seen: DateTime<Utc>,
  }

  pub async fn list_device_groups(
      conn: &mut AsyncPgConnection,
      scope: ReadScope,
      since: DateTime<Utc>,
      limit: i64,
      offset: i64,
      search: Option<&str>,
  ) -> QueryResult<Vec<DeviceGroupRow>>
  ```

- [ ] **Step 0: Extract the membership fragment (pure refactor, under existing green tests)**

`list_device_groups` needs the same membership `EXISTS` block `list_devices` already builds inline. Copying it would be verbatim duplication of a logic block; extract it instead, matching how [`device_last_distinct_id_join`] is already factored in this same file.

First confirm the existing device tests are green, so the refactor has a baseline:

```bash
cd backend && cargo test -p sauron-db --test env_scoping list_devices
```

Add above `list_devices` in `backend/crates/sauron-db/src/repo.rs`:

```rust
/// `devices` carries no `environment_id`, so a device's membership of an
/// environment is derived from activity keyed by `device_key` in the three
/// tables that do carry one. Shared by [`list_devices`] and
/// [`list_device_groups`], which need the identical predicate over the
/// identical bind index.
///
/// Empty under `All` — every device qualifies, so the whole clause is omitted
/// rather than emitted as a tautology.
///
/// Each leg aliases its subquery and qualifies the correlated column with that
/// alias (`ae.device_key`, not bare `device_key`). Demonstrated live during
/// review: with no alias, an unqualified name that happens to also exist on
/// the inner table resolves there only by luck — if a future copy of this
/// pattern targets a table with no `device_key` column, Postgres silently
/// binds the bare name to the *outer* `devices` row instead, collapsing the
/// whole `EXISTS` into `devices.device_key = devices.device_key` (always true,
/// no error). Qualifying turns that mistake into a hard query error instead.
///
/// The sessions leg carries `started_at >= $2`, matching the `se` LATERAL at
/// both call sites. Without it, a device whose only env_a session is older
/// than `since` — but whose `devices.last_seen` is recent from unrelated env_b
/// activity — would still pass membership and render an all-zero row under
/// `One(env_a)`, the exact bug this filter exists to prevent.
///
/// Takes `&EnvFilter`, unlike the older [`device_last_distinct_id_join`] next
/// to it: that one keeps its pre-existing owned signature rather than being
/// reshaped, but a new function has no such constraint.
fn device_membership_sql(env: &EnvFilter, bind_index: usize) -> String {
    if matches!(env, EnvFilter::All) {
        return String::new();
    }
    let ae_env = env.sql_fragment_for("ae", bind_index);
    let ee_env = env.sql_fragment_for("ee", bind_index);
    let se_env = env.sql_fragment_for("se", bind_index);
    format!(
        " AND ( \
            EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.device_key = devices.device_key{ae_env}) \
            OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.device_key = devices.device_key{ee_env}) \
            OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.device_key = devices.device_key AND se.started_at >= $2{se_env}) \
          )"
    )
}
```

Then replace `list_devices`' inline `let membership_sql = if matches!(...) { ... };` block (currently `repo.rs:5694-5707`) with:

```rust
    let membership_sql = device_membership_sql(&scope.env, 6);
```

Move the doc-comment prose that was attached to the inline block into the new function (it is reproduced above) rather than deleting it — it records two live-demonstrated review findings.

Re-run the same tests. They must still pass, unchanged:

```bash
cd backend && cargo test -p sauron-db --test env_scoping list_devices
```

Expected: PASS, with the same test names as before this step. This is a refactor: a behaviour change here is a defect.

- [ ] **Step 1: Write the fixture and the failing collapse test**

Create `backend/crates/sauron-db/tests/device_groups.rs`:

```rust
//! `list_device_groups` and `list_devices`' group filter against a real Postgres.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset, mirroring
//! `env_scoping.rs` and `sessions.rs`. CI has no database service.
//!
//! Deliberately does NOT use `TestDb::seed_two_envs`: that fixture is asserted
//! on by a dozen tests in `env_scoping.rs`, including exact `devices` row
//! counts, and grouping needs devices with controlled, deliberately colliding
//! descriptor tuples.

mod common;

use chrono::{DateTime, Duration, Utc};
use common::{far_past, seed_env, TestDb};
use sauron_db::repo;
use sauron_db::scope::{EnvFilter, ReadScope};
// Task 2 adds: use sauron_db::repo::DeviceGroupKey;
use uuid::Uuid;

/// Ids from [`seed_device_fleet`].
struct FleetIds {
    app_id: Uuid,
    env_a: Uuid,
    env_b: Uuid,
    /// The two `iPhone / iPhone15,2 / iOS / 17.4.1` devices — the collapse case.
    iphone_a: String,
    iphone_b: String,
    /// Same model, one patch version apart — must NOT collapse into the above.
    iphone_older: String,
    /// Every descriptor column NULL — the "Unknown device" group.
    unknown: String,
    /// Only ever active in `env_b`.
    pixel_b_only: String,
    pinned_now: DateTime<Utc>,
}

/// Five devices across two environments, with deliberately colliding
/// descriptors. Counts are asymmetric per device so a summed aggregate cannot
/// accidentally match a wrong one.
async fn seed_device_fleet(db: &TestDb) -> FleetIds {
    let mut conn = db.conn().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let now = Utc::now();

    let org = repo::create_org(&mut conn, "fleet org", &format!("fleet-org-{suffix}"))
        .await
        .expect("create org");
    let project = repo::create_project(&mut conn, org.id, "fleet project", &format!("fleet-proj-{suffix}"))
        .await
        .expect("create project");
    let app = repo::create_app(&mut conn, project.id, "fleet app", &format!("fleet-app-{suffix}"), "web")
        .await
        .expect("create app");
    let env_a = seed_env(&mut conn, project.id, app.id, "env_a", &format!("pk_fleet_a_{suffix}"), true).await;
    let env_b = seed_env(&mut conn, project.id, app.id, "env_b", &format!("pk_fleet_b_{suffix}"), false).await;

    let iphone_a = format!("fleet-{suffix}-iphone-a");
    let iphone_b = format!("fleet-{suffix}-iphone-b");
    let iphone_older = format!("fleet-{suffix}-iphone-older");
    let unknown = format!("fleet-{suffix}-unknown");
    let pixel_b_only = format!("fleet-{suffix}-pixel-b");

    // (device_key, family, model, os_name, os_version, events, errors)
    let fleet: [(&str, Option<&str>, Option<&str>, Option<&str>, Option<&str>, i64, i64); 5] = [
        (&iphone_a,     Some("iPhone"), Some("iPhone15,2"), Some("iOS"), Some("17.4.1"), 3, 1),
        (&iphone_b,     Some("iPhone"), Some("iPhone15,2"), Some("iOS"), Some("17.4.1"), 5, 2),
        (&iphone_older, Some("iPhone"), Some("iPhone15,2"), Some("iOS"), Some("17.4.0"), 7, 4),
        (&unknown,      None,           None,               None,        None,           2, 1),
        (&pixel_b_only, Some("Pixel"),  Some("Pixel 8"),    Some("Android"), Some("14"), 9, 3),
    ];

    for (key, family, model, os_name, os_version, events, errors) in fleet {
        repo::bump_device(
            &mut conn, app.id, key, family, model, os_name, os_version,
            None, None, None, now - Duration::seconds(30), events, errors,
        )
        .await
        .expect("bump_device");
    }

    drop(conn);
    FleetIds {
        app_id: app.id,
        env_a,
        env_b,
        iphone_a,
        iphone_b,
        iphone_older,
        unknown,
        pixel_b_only,
        pinned_now: now,
    }
}

#[tokio::test]
async fn devices_sharing_model_and_os_collapse_into_one_group() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    let rows = repo::list_device_groups(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        50,
        0,
        None,
    )
    .await
    .expect("list_device_groups");

    let collapsed = rows
        .iter()
        .find(|r| r.os_version.as_deref() == Some("17.4.1"))
        .expect("the iOS 17.4.1 group");
    assert_eq!(collapsed.device_count, 2, "iphone_a and iphone_b are one group");
    assert_eq!(collapsed.model.as_deref(), Some("iPhone15,2"));
    assert_eq!(collapsed.events_count, 8, "3 + 5 summed across the group");
    assert_eq!(collapsed.errors_count, 3, "1 + 2 summed across the group");

    // A one-patch-version difference is its own group (locked decision 2).
    let older = rows
        .iter()
        .find(|r| r.os_version.as_deref() == Some("17.4.0"))
        .expect("the iOS 17.4.0 group");
    assert_eq!(older.device_count, 1);
    assert_eq!(older.events_count, 7);

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn devices_with_null_descriptors_form_one_unknown_group() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    let rows = repo::list_device_groups(&mut conn, ReadScope::all(ids.app_id), far_past(), 50, 0, None)
        .await
        .expect("list_device_groups");

    let unknown = rows
        .iter()
        .find(|r| r.model.is_none() && r.os_name.is_none())
        .expect("the all-NULL group");
    assert_eq!(unknown.device_count, 1);
    assert_eq!(unknown.events_count, 2);
    assert!(unknown.family.is_none());
    assert!(unknown.os_version.is_none());

    // Five seeded devices, four distinct descriptor tuples.
    assert_eq!(rows.len(), 4, "17.4.1, 17.4.0, Android 14, and the NULL group");

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd backend && TEST_DATABASE_URL="$TEST_DATABASE_URL" cargo test -p sauron-db --test device_groups
```

Expected: FAIL to compile — `cannot find function list_device_groups in module repo`.

- [ ] **Step 3: Add `DeviceGroupRow`**

In `backend/crates/sauron-db/src/repo.rs`, immediately after `list_devices` (which ends line 5824):

```rust
/// One row per `(family, model, os_name, os_version)` tuple — the Devices
/// inventory's default shape. See
/// `docs/superpowers/specs/2026-08-09-devices-grouped-by-model-and-os-design.md`.
///
/// No `last_distinct_id`: it is a per-device value with no meaningful aggregate
/// over a group, and reproducing it would drag
/// [`device_last_distinct_id_join`]'s per-device `UNION ALL ... LIMIT 1` into a
/// query that — unlike [`list_devices`] — runs its joins over every qualifying
/// device rather than one page of 50.
///
/// `browser`/`arch` are likewise absent: they are not part of the grouping key
/// (a locked decision — every browser on Windows 11 folds into one row), so
/// they have no single value per group. Both survive on the drill-down, which
/// returns [`DeviceRow`].
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct DeviceGroupRow {
    #[diesel(sql_type = Nullable<Text>)]
    pub family: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub model: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub os_name: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub os_version: Option<String>,
    #[diesel(sql_type = BigInt)]
    pub device_count: i64,
    #[diesel(sql_type = BigInt)]
    pub events_count: i64,
    #[diesel(sql_type = BigInt)]
    pub errors_count: i64,
    #[diesel(sql_type = BigInt)]
    pub sessions_count: i64,
    #[diesel(sql_type = Timestamptz)]
    pub first_seen: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub last_seen: DateTime<Utc>,
}
```

- [ ] **Step 4: Implement `list_device_groups`**

Directly below `DeviceGroupRow`:

```rust
/// [`list_devices`], but paged over descriptor groups instead of devices.
///
/// The qualifying-devices subquery is `list_devices`' verbatim — same
/// `last_seen >= $2` window, same escaped `ILIKE`, same membership `EXISTS`
/// legs — minus its `ORDER BY ... LIMIT/OFFSET`, because every qualifying
/// device must be visible to the aggregate. Paging moves to the outer query,
/// after `GROUP BY`.
///
/// Cost, stated rather than discovered: the count LATERALs run for every
/// qualifying device in the window, not just the 50 on screen. Each is an index
/// probe — `sessions_app_device_started_idx`, `analytics_events_app_device_idx`,
/// `error_events_app_device_idx` — but this is strictly more work per request
/// than `list_devices`' page-then-count, and is the accepted price of paging
/// over groups.
///
/// The `All`-vs-scoped source split is `list_devices`' unchanged: durable
/// `devices` columns under `All`, environment-scoped LATERALs otherwise,
/// inheriting the same `sauron-tier` blind spot documented there.
///
/// NULL grouping is intended, not incidental: Postgres `GROUP BY` treats NULLs
/// as equal, so devices reporting no descriptors collapse into one honest
/// "Unknown" row rather than scattering into singletons.
pub async fn list_device_groups(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
    offset: i64,
    search: Option<&str>,
) -> QueryResult<Vec<DeviceGroupRow>> {
    let pattern = search.map(like_contains).unwrap_or_else(|| "%".to_string());

    // $1 app_id, $2 since, $3 pattern, $4 limit, $5 offset, env takes $6.
    // Identical layout to `list_devices`, so the shared SQL fragments below can
    // be copied across without renumbering.
    let env_sql = scope.env.sql_fragment(6);

    // Shared with `list_devices` — see Step 0. Same predicate, same bind index.
    let membership_sql = device_membership_sql(&scope.env, 6);

    // The aggregate wraps whichever source `list_devices` would have selected.
    // `device_last_distinct_id_join` is deliberately NOT joined here — see
    // `DeviceGroupRow`'s doc comment.
    let (scoped_select, scoped_join) = if matches!(scope.env, EnvFilter::All) {
        (
            "sum(d.events_count)::bigint AS events_count, \
             sum(d.errors_count)::bigint AS errors_count, \
             min(d.first_seen) AS first_seen, \
             max(d.last_seen) AS last_seen"
                .to_string(),
            String::new(),
        )
    } else {
        (
            "COALESCE(sum(ae.cnt), 0)::bigint AS events_count, \
             COALESCE(sum(ee.cnt), 0)::bigint AS errors_count, \
             min(LEAST(ae.min_occurred, ee.min_occurred, se.min_started)) AS first_seen, \
             max(GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event)) AS last_seen"
                .to_string(),
            format!(
                " LEFT JOIN LATERAL ( \
                     SELECT count(*) AS cnt, min(occurred_at) AS min_occurred, \
                            max(occurred_at) AS max_occurred FROM analytics_events \
                     WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
                 ) ae ON TRUE \
                 LEFT JOIN LATERAL ( \
                     SELECT count(*) AS cnt, min(occurred_at) AS min_occurred, \
                            max(occurred_at) AS max_occurred FROM error_events \
                     WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
                 ) ee ON TRUE"
            ),
        )
    };

    // `ORDER BY last_seen`, the OUTPUT column, not `max(d.last_seen)`. The two
    // coincide only under `All`; under a scoped filter the selected `last_seen`
    // is derived from the LATERALs while `d.last_seen` is the app-wide column,
    // which can be newer because of activity this scope cannot see. Postgres
    // resolves a bare ORDER BY name against the select list's output aliases
    // first. If it ever resolved the other way the query would raise "column
    // d.last_seen must appear in the GROUP BY clause" — a hard error, not a
    // silently mis-sorted page.
    let q = format!(
        "SELECT d.family, d.model, d.os_name, d.os_version, \
                count(*)::bigint AS device_count, \
                {scoped_select}, \
                COALESCE(sum(se.cnt), 0)::bigint AS sessions_count \
         FROM ( \
             SELECT * FROM devices \
             WHERE app_id = $1 AND last_seen >= $2 \
               AND (COALESCE(family,'') || ' ' || COALESCE(model,'') || ' ' || \
                    COALESCE(os_name,'') || ' ' || COALESCE(device_key,'')) ILIKE $3{membership_sql} \
         ) d{scoped_join} \
         LEFT JOIN LATERAL ( \
             SELECT count(*) FILTER (WHERE started_at >= $2) AS cnt, \
                    min(started_at) AS min_started, max(last_event_at) AS max_last_event \
             FROM sessions \
             WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
         ) se ON TRUE \
         GROUP BY d.family, d.model, d.os_name, d.os_version \
         ORDER BY last_seen DESC \
         LIMIT $4 OFFSET $5"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Text, _>(pattern)
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test device_groups
```

Expected: both tests PASS. If `TEST_DATABASE_URL` is unset they print the skip line and pass trivially — that is **not** a green result. Set it and re-run before continuing.

- [ ] **Step 6: Add the environment-scoping test**

Append to `backend/crates/sauron-db/tests/device_groups.rs`:

```rust
/// A group's counts must not include a device active only in another
/// environment. `pixel_b_only` gets its only signals in `env_b`; under
/// `One(env_a)` its group must not appear at all.
#[tokio::test]
async fn groups_exclude_devices_from_other_environments() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    // Give iphone_a signals in env_a and pixel_b_only signals in env_b, so
    // membership differs by environment.
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), &ids.iphone_a, ids.pinned_now - Duration::seconds(20)).await;
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_b), &ids.pixel_b_only, ids.pinned_now - Duration::seconds(20)).await;

    let rows_a = repo::list_device_groups(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        50,
        0,
        None,
    )
    .await
    .expect("list_device_groups env_a");

    assert!(
        rows_a.iter().all(|r| r.os_name.as_deref() != Some("Android")),
        "pixel_b_only is env_b-only and must not surface under One(env_a)"
    );
    let iphone = rows_a
        .iter()
        .find(|r| r.os_version.as_deref() == Some("17.4.1"))
        .expect("the iOS 17.4.1 group under env_a");
    assert_eq!(
        iphone.device_count, 1,
        "only iphone_a has env_a activity; iphone_b must not be counted"
    );

    drop(conn);
    db.cleanup().await;
}
```

**Correction, found during execution:** `common::seed_signal_event` cannot be used here. It hard-codes `device_key: None`, and `device_membership_sql` correlates on `ae.device_key = devices.device_key`, so a row it inserts satisfies membership for no device — the test fails with an empty result set. Insert `NewAnalyticsEvent` directly with `device_key` set instead, modelled on `env_scoping.rs`'s `seed_cross_env_session_child_rows`, via a small local helper in this file. Do not reshape the shared fixture.

The same trap applies to any later test that needs device-attributed signal rows: check whether the shared helper sets `device_key` before reaching for it.

- [ ] **Step 7: Run and verify**

```bash
cd backend && cargo test -p sauron-db --test device_groups && cargo clippy -p sauron-db --all-targets -- -D warnings
```

Expected: three tests PASS, clippy clean. **Do not commit** (see Global Constraints).

---

## Task 2: The drill-down filter on `list_devices`

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs:5652` (`list_devices`)
- Test: `backend/crates/sauron-db/tests/device_groups.rs` (append)

**Interfaces:**
- Consumes: `DeviceRow`, `list_devices` from Task 1's neighbourhood.
- Produces:
  ```rust
  #[derive(Debug, Clone, Default)]
  pub struct DeviceGroupKey<'a> {
      pub family: Option<&'a str>,
      pub model: Option<&'a str>,
      pub os_name: Option<&'a str>,
      pub os_version: Option<&'a str>,
  }

  // list_devices gains a trailing parameter:
  //   group: Option<DeviceGroupKey<'_>>
  ```
  `Some(key)` applies all four predicates with `IS NOT DISTINCT FROM`; `None` filters nothing. Every existing call site passes `None`.

- [ ] **Step 1: Write the failing drill-down tests**

Append to `backend/crates/sauron-db/tests/device_groups.rs`:

```rust
/// The drill-down returns exactly the members of one group.
#[tokio::test]
async fn group_filter_returns_only_that_groups_devices() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    let rows = repo::list_devices(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        50,
        0,
        None,
        Some(DeviceGroupKey {
            family: Some("iPhone"),
            model: Some("iPhone15,2"),
            os_name: Some("iOS"),
            os_version: Some("17.4.1"),
        }),
    )
    .await
    .expect("list_devices with group filter");

    let keys: Vec<&str> = rows.iter().map(|r| r.device_key.as_str()).collect();
    assert_eq!(keys.len(), 2, "exactly the two 17.4.1 devices");
    assert!(keys.contains(&ids.iphone_a.as_str()));
    assert!(keys.contains(&ids.iphone_b.as_str()));
    assert!(
        !keys.contains(&ids.iphone_older.as_str()),
        "17.4.0 is a different group"
    );

    drop(conn);
    db.cleanup().await;
}

/// The NULL case: an all-NULL group must drill down to its member, not to
/// nothing. `=` would return zero rows here; `IS NOT DISTINCT FROM` is what
/// makes this work.
#[tokio::test]
async fn group_filter_matches_the_all_null_group() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    let rows = repo::list_devices(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        50,
        0,
        None,
        Some(DeviceGroupKey::default()),
    )
    .await
    .expect("list_devices with all-NULL group filter");

    assert_eq!(rows.len(), 1, "only the descriptor-less device");
    assert_eq!(rows[0].device_key, ids.unknown);

    drop(conn);
    db.cleanup().await;
}

/// `None` must leave the pre-existing behaviour byte for byte.
#[tokio::test]
async fn no_group_filter_returns_every_device() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    let rows = repo::list_devices(&mut conn, ReadScope::all(ids.app_id), far_past(), 50, 0, None, None)
        .await
        .expect("list_devices unfiltered");
    assert_eq!(rows.len(), 5, "all five seeded devices");

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd backend && cargo test -p sauron-db --test device_groups
```

Expected: FAIL to compile — `this function takes 6 arguments but 7 arguments were supplied`.

- [ ] **Step 3: Add `DeviceGroupKey`**

In `backend/crates/sauron-db/src/repo.rs`, directly above `list_devices`:

```rust
/// The four descriptor columns [`list_device_groups`] groups by, used as an
/// exact-match filter to drill from one grouped row down to its member devices.
///
/// `Option<DeviceGroupKey>` — not four loose `Option<&str>` parameters — because
/// the two nestings mean different things and collapsing them loses the
/// distinction: `None` is "do not filter at all", while `Some(key)` with
/// `key.model == None` is "filter to devices whose model IS NULL". Four loose
/// options cannot express the second, and the all-NULL group is a real group
/// (any SDK that reports no descriptors lands in it).
#[derive(Debug, Clone, Default)]
pub struct DeviceGroupKey<'a> {
    pub family: Option<&'a str>,
    pub model: Option<&'a str>,
    pub os_name: Option<&'a str>,
    pub os_version: Option<&'a str>,
}
```

- [ ] **Step 4: Thread the filter through `list_devices`**

Add the parameter to the signature at `repo.rs:5652`:

```rust
pub async fn list_devices(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
    offset: i64,
    search: Option<&str>,
    group: Option<DeviceGroupKey<'_>>,
) -> QueryResult<Vec<DeviceRow>> {
```

Immediately after the existing `let env_sql = scope.env.sql_fragment(6);`, add:

```rust
    // The group binds follow env, not precede it, so env keeps index 6 and
    // every fragment above is untouched. `consumes_bind()` is load-bearing:
    // `sql_fragment` reserves its index only for `One`/`Subset` — `All` emits
    // nothing and `Unattributed` emits a literal `IS NULL` — so assuming the
    // index is always consumed would shift all four group binds by one.
    let group_base = if scope.env.consumes_bind() { 7 } else { 6 };

    // `IS NOT DISTINCT FROM`, not `=`: the all-NULL group is a real group, and
    // `model = NULL` is NULL (never true), which would silently return zero
    // rows for it. Applied inside the paging subquery, alongside the search
    // predicate, so LIMIT still applies to the filtered set.
    let group_sql = if group.is_some() {
        format!(
            " AND family IS NOT DISTINCT FROM ${} \
              AND model IS NOT DISTINCT FROM ${} \
              AND os_name IS NOT DISTINCT FROM ${} \
              AND os_version IS NOT DISTINCT FROM ${}",
            group_base,
            group_base + 1,
            group_base + 2,
            group_base + 3,
        )
    } else {
        String::new()
    };
```

In the `format!` for `q`, append `{group_sql}` immediately after `{membership_sql}` — inside the paging subquery's `WHERE`, before `ORDER BY last_seen DESC LIMIT $4 OFFSET $5`:

```rust
                    COALESCE(os_name,'') || ' ' || COALESCE(device_key,'')) ILIKE $3{membership_sql}{group_sql} \
             ORDER BY last_seen DESC LIMIT $4 OFFSET $5 \
```

Finally, after the existing `stmt = crate::bind_env!(stmt, &scope.env);`, add the group binds and keep the return:

```rust
    stmt = crate::bind_env!(stmt, &scope.env);
    if let Some(k) = group {
        stmt = stmt
            .bind::<Nullable<Text>, _>(k.family.map(str::to_owned))
            .bind::<Nullable<Text>, _>(k.model.map(str::to_owned))
            .bind::<Nullable<Text>, _>(k.os_name.map(str::to_owned))
            .bind::<Nullable<Text>, _>(k.os_version.map(str::to_owned));
    }
    stmt.get_results(conn).await
```

- [ ] **Step 5: Update every existing `list_devices` call site**

Find them and pass `None`:

```bash
cd backend && grep -rn "list_devices(" --include=*.rs .
```

At minimum: `bins/sauron-api/src/routes/devices.rs:61` and the existing test at `crates/sauron-db/tests/env_scoping.rs:2669` (plus its siblings at 2701, 2741, 2753). Every one gets a trailing `None,`.

- [ ] **Step 6: Run the full db test suite**

```bash
cd backend && cargo test -p sauron-db && cargo clippy -p sauron-db --all-targets -- -D warnings
```

Expected: the three new tests PASS and every pre-existing `env_scoping.rs` device test still PASSES — that regression check is the point of Step 5. **Do not commit.**

---

## Task 3: The HTTP route

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/devices.rs`
- Modify: `backend/bins/sauron-api/src/main.rs:635`
- Test: `backend/bins/sauron-api/tests/http_env_scoping.rs` (append)

**Interfaces:**
- Consumes: `repo::list_device_groups`, `repo::DeviceGroupKey` (Tasks 1–2), `super::scope::authorized_read_scope`, `super::clamp_offset`.
- Produces: `GET /v1/apps/{app_id}/device-groups` → `Json<Vec<DeviceGroupRow>>`; `GET /v1/apps/{app_id}/devices` additionally accepts `group=1&family=&model=&os_name=&os_version=`.

- [ ] **Step 1: Write the failing route tests**

Append to the **existing** `backend/bins/sauron-api/tests/http_env_scoping.rs`, rather than creating a new `http_*` file. Each `http_*` test binary carries its own ~400-line duplicated `TestServer` (spawns a real `sauron-api` process, migrates an ephemeral database); this file already has one, plus `seed_env_scoped_fixture` with exactly the owner/member tokens and `granted_env` / `other_env` pair these two assertions need. Duplicating that harness to test two routes is a large cost for no coverage gain.

```rust
/// The grouped Devices read is a new route on the same `EVENT_READ` +
/// `environment_id` contract as `/devices`. Both halves matter: a missing
/// permission gate and a silently-ignored `environment_id` both present as a
/// perfectly normal 200, so status alone cannot tell them from correct
/// behaviour — the env assertion below compares two environments' bodies.
#[tokio::test]
async fn device_groups_is_permission_gated_and_env_scoped() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;
    let granted = f.granted_env;
    let other = f.other_env;

    // The member holds one env-scoped grant, on `granted_env` only.
    let ok = srv
        .get_status(
            &format!("/v1/apps/{app}/device-groups?environment_id={granted}&since_days=3650"),
            &f.member_token,
        )
        .await;
    assert_eq!(ok, 200, "device-groups must be readable in the granted env");

    let denied = srv
        .get_status(
            &format!("/v1/apps/{app}/device-groups?environment_id={other}&since_days=3650"),
            &f.member_token,
        )
        .await;
    assert_eq!(
        denied, 403,
        "device-groups must refuse an environment the member holds no grant on"
    );

    // The route reads `environment_id` from the raw query string. If that
    // wiring were missing, both requests would return the same app-wide body
    // and the status assertions above would still pass.
    let granted_body = srv
        .get_json(
            &format!("/v1/apps/{app}/device-groups?environment_id={granted}&since_days=3650"),
            &f.owner_token,
        )
        .await;
    let other_body = srv
        .get_json(
            &format!("/v1/apps/{app}/device-groups?environment_id={other}&since_days=3650"),
            &f.owner_token,
        )
        .await;
    assert!(granted_body.is_array() && other_body.is_array());
    assert_ne!(
        granted_body, other_body,
        "two environments must not return identical grouped bodies — the \
         fixture seeds device activity in granted_env only"
    );

    srv.shutdown().await;
}

/// The drill-down sentinel. Without `group=1` the four descriptor parameters
/// are ignored entirely, which is what keeps every existing `/devices` caller
/// working unchanged.
#[tokio::test]
async fn devices_group_filter_applies_only_behind_the_sentinel() {
    let Some(mut srv) = TestServer::start().await else {
        return;
    };
    let f = srv.seed_env_scoped_fixture().await;
    let app = f.app_id;

    let unfiltered = srv
        .get_json(
            &format!("/v1/apps/{app}/devices?since_days=3650"),
            &f.owner_token,
        )
        .await;

    // Same descriptor parameters, no sentinel: must be byte-identical to the
    // unfiltered read.
    let ignored = srv
        .get_json(
            &format!("/v1/apps/{app}/devices?since_days=3650&model=no-such-model"),
            &f.owner_token,
        )
        .await;
    assert_eq!(
        unfiltered, ignored,
        "without group=1 the descriptor params must not filter anything"
    );

    // With the sentinel, a model that matches nothing must return nothing.
    let filtered = srv
        .get_json(
            &format!("/v1/apps/{app}/devices?since_days=3650&group=1&model=no-such-model"),
            &f.owner_token,
        )
        .await;
    assert_eq!(
        filtered.as_array().map(|a| a.len()),
        Some(0),
        "group=1 with an unmatched model must return an empty list"
    );

    srv.shutdown().await;
}
```

Before running, confirm `EnvScopedFixture` exposes `other_env` and `member_token` under those names (declared around `http_env_scoping.rs:429`) and adjust if they differ. If the fixture seeds no `devices` rows in `granted_env`, the `assert_ne!` on the two bodies will fail on equal empty arrays — in that case extend `seed_env_scoped_fixture` with one `repo::bump_device` call plus one analytics event in `granted_env`, and say so in the task report.

- [ ] **Step 2: Run to verify they fail**

```bash
cd backend && cargo test -p sauron-api --test http_env_scoping device_groups
```

Expected: FAIL — the `device-groups` requests return 404, because the route is not registered.

- [ ] **Step 3: Add the handler**

In `backend/bins/sauron-api/src/routes/devices.rs`, add `DeviceGroupRow` to the `repo` import and append after `list`:

```rust
/// The Devices inventory's default read: one row per
/// `(family, model, os_name, os_version)`. Same scope handling as [`list`] —
/// `environment_id` comes from the raw query string, never from `ListQuery`.
pub async fn groups(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<DeviceGroupRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let limit = q.limit.clamp(1, 200);
    let search = q.search.as_deref().filter(|s| !s.is_empty());
    Ok(Json(
        repo::list_device_groups(
            &mut conn,
            scope,
            since,
            limit,
            super::clamp_offset(q.offset),
            search,
        )
        .await?,
    ))
}
```

- [ ] **Step 4: Add the group filter params to `ListQuery` and `list`**

Extend `ListQuery`:

```rust
    /// Sentinel for the drill-down. Present means all four descriptor fields
    /// below apply, with an ABSENT field meaning SQL NULL. Without it the four
    /// are ignored entirely and `list` behaves exactly as it always has.
    ///
    /// The sentinel exists because absent and "filter to NULL" are the same
    /// wire shape otherwise — an omitted query parameter — and the all-NULL
    /// group is a real group that must be drillable.
    pub group: Option<String>,
    pub family: Option<String>,
    pub model: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
```

In `list`, before the `repo::list_devices` call:

```rust
    // Any non-empty `group` value turns the filter on; the dashboard sends "1".
    let group = q.group.as_deref().filter(|s| !s.is_empty()).map(|_| {
        repo::DeviceGroupKey {
            family: q.family.as_deref(),
            model: q.model.as_deref(),
            os_name: q.os_name.as_deref(),
            os_version: q.os_version.as_deref(),
        }
    });
```

and pass `group` as the new trailing argument.

- [ ] **Step 5: Register the route**

In `backend/bins/sauron-api/src/main.rs`, beside line 635:

```rust
        .route("/v1/apps/{app_id}/device-groups", get(routes::devices::groups))
```

- [ ] **Step 6: Run and verify**

```bash
cd backend && cargo test -p sauron-api --test http_env_scoping && cargo clippy --workspace --all-targets -- -D warnings
```

Expected: the two new tests PASS **and every pre-existing test in `http_env_scoping.rs` still passes** — this file is shared, so run the whole binary, not just the two new names. Clippy clean across the workspace. **Do not commit.**

---

## Task 4: Frontend types, group-key helpers, and API client

**Files:**
- Create: `dashboard/src/lib/models/device-groups.ts`
- Create: `dashboard/src/lib/models/device-groups.test.ts`
- Modify: `dashboard/src/lib/models/index.ts:610` (after `DeviceRow`)
- Modify: `dashboard/src/lib/api/devices.ts`

**Interfaces:**
- Consumes: `DeviceGroupRow`'s wire shape from Task 1.
- Produces:
  ```ts
  export interface DeviceGroupKey {
    family: string | null; model: string | null;
    os_name: string | null; os_version: string | null;
  }
  export function encodeGroupKey(k: DeviceGroupKey): string;   // "group=1&family=…"
  export function decodeGroupKey(qs: string | null): DeviceGroupKey | null;
  export function groupLabel(k: DeviceGroupKey): string;       // "iPhone iPhone15,2 · iOS 17.4.1"
  export async function listDeviceGroups(appId: string, params?: ListDevicesParams): Promise<DeviceGroupRow[]>;
  ```

- [ ] **Step 1: Write the failing unit tests**

Create `dashboard/src/lib/models/device-groups.test.ts`:

```ts
import { describe, it, expect } from 'vitest';
import { encodeGroupKey, decodeGroupKey, groupLabel } from './device-groups';

describe('group key URL round-trip', () => {
  it('round-trips a fully populated key', () => {
    const key = { family: 'iPhone', model: 'iPhone15,2', os_name: 'iOS', os_version: '17.4.1' };
    expect(decodeGroupKey(encodeGroupKey(key))).toEqual(key);
  });

  it('round-trips NULL components as null, not as empty string', () => {
    const key = { family: null, model: null, os_name: null, os_version: null };
    const qs = encodeGroupKey(key);
    expect(qs).toBe('group=1');
    expect(decodeGroupKey(qs)).toEqual(key);
  });

  // The case that decides whether a device with os_version = '' drills down to
  // itself or falls into the NULL group. Absent and empty must stay distinct.
  it('keeps an empty-string component distinct from an absent one', () => {
    const key = { family: 'Web', model: null, os_name: 'Windows', os_version: '' };
    const decoded = decodeGroupKey(encodeGroupKey(key));
    expect(decoded).toEqual(key);
    expect(decoded!.os_version).toBe('');
    expect(decoded!.model).toBeNull();
  });

  it('preserves values needing URL escaping', () => {
    const key = { family: 'Mac & PC', model: 'a/b c', os_name: 'iOS', os_version: '17.4.1' };
    expect(decodeGroupKey(encodeGroupKey(key))).toEqual(key);
  });

  it('returns null when the sentinel is absent, so the page stays in grouped mode', () => {
    expect(decodeGroupKey('')).toBeNull();
    expect(decodeGroupKey(null)).toBeNull();
    expect(decodeGroupKey('family=iPhone')).toBeNull();
    expect(decodeGroupKey('since_days=30')).toBeNull();
  });
});

describe('groupLabel', () => {
  it('joins device and OS halves', () => {
    expect(groupLabel({ family: 'iPhone', model: 'iPhone15,2', os_name: 'iOS', os_version: '17.4.1' }))
      .toBe('iPhone iPhone15,2 · iOS 17.4.1');
  });

  it('names the all-null group rather than rendering an empty string', () => {
    expect(groupLabel({ family: null, model: null, os_name: null, os_version: null }))
      .toBe('Unknown device');
  });

  it('falls back to one half when the other is missing', () => {
    expect(groupLabel({ family: null, model: null, os_name: 'Android', os_version: '14' }))
      .toBe('Android 14');
  });
});
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd dashboard && npx vitest run src/lib/models/device-groups.test.ts
```

Expected: FAIL — `Failed to resolve import "./device-groups"`.

- [ ] **Step 3: Implement the helpers**

Create `dashboard/src/lib/models/device-groups.ts`:

```ts
/**
 * The four descriptor columns the Devices inventory groups by, and the
 * querystring encoding that carries one group into the drill-down URL.
 *
 * Absent and empty are kept distinct on the wire: a null component is omitted
 * from the querystring entirely, while an empty-string component is emitted as
 * `os_version=`. `URLSearchParams.get` returns `null` for the former and `''`
 * for the latter, so the backend's `IS NOT DISTINCT FROM` sees the value the
 * device actually stores. Collapse the two and a device whose `os_version` is
 * `''` drills down into the NULL group instead of its own.
 */
export interface DeviceGroupKey {
  family: string | null;
  model: string | null;
  os_name: string | null;
  os_version: string | null;
}

const KEY_FIELDS = ['family', 'model', 'os_name', 'os_version'] as const;

/** `group=1&family=iPhone&…` — the querystring for a group's drill-down URL. */
export function encodeGroupKey(k: DeviceGroupKey): string {
  const p = new URLSearchParams();
  p.set('group', '1');
  for (const f of KEY_FIELDS) {
    const v = k[f];
    if (v !== null) p.set(f, v);
  }
  return p.toString();
}

/** The key a drill-down URL carries, or null when the page is in grouped mode. */
export function decodeGroupKey(qs: string | null): DeviceGroupKey | null {
  const p = new URLSearchParams(qs ?? '');
  if (p.get('group') !== '1') return null;
  return {
    family: p.get('family'),
    model: p.get('model'),
    os_name: p.get('os_name'),
    os_version: p.get('os_version'),
  };
}

/** Human label for a group — the header chip on the drill-down. */
export function groupLabel(k: DeviceGroupKey): string {
  const device = [k.family, k.model].filter(Boolean).join(' ').trim();
  const os = [k.os_name, k.os_version].filter(Boolean).join(' ').trim();
  const parts = [device, os].filter(Boolean);
  return parts.length > 0 ? parts.join(' · ') : 'Unknown device';
}
```

- [ ] **Step 4: Run to verify they pass**

```bash
cd dashboard && npx vitest run src/lib/models/device-groups.test.ts
```

Expected: all 8 tests PASS.

- [ ] **Step 5: Add the wire type and API client functions**

In `dashboard/src/lib/models/index.ts`, after `DeviceRow` (ends line 625):

```ts
/**
 * One row per (family, model, os_name, os_version) — the Devices inventory's
 * default shape. No `last_distinct_id`, `browser` or `arch`: none has a single
 * value across a group. All four are on `DeviceRow`, in the drill-down.
 */
export interface DeviceGroupRow {
  family: string | null;
  model: string | null;
  os_name: string | null;
  os_version: string | null;
  device_count: number;
  events_count: number;
  errors_count: number;
  sessions_count: number;
  first_seen: string;
  last_seen: string;
}
```

In `dashboard/src/lib/api/devices.ts`, extend the params and add the call:

```ts
import type { DeviceRow, DeviceGroupRow, DeviceDetail } from '../models';

export interface ListDevicesParams {
  since_days?: number;
  limit?: number;
  offset?: number;
  search?: string;
  // The drill-down filter. `group: '1'` is the sentinel that turns the four
  // descriptor fields on; without it the backend ignores them. An omitted
  // field means SQL NULL, which is how the all-NULL group is addressed.
  group?: string;
  family?: string;
  model?: string;
  os_name?: string;
  os_version?: string;
}

export async function listDeviceGroups(
  appId: string,
  params: ListDevicesParams = {},
): Promise<DeviceGroupRow[]> {
  const { data } = await api.get<DeviceGroupRow[]>(`/v1/apps/${appId}/device-groups`, { params });
  return data;
}
```

- [ ] **Step 6: Run and verify**

```bash
cd dashboard && npm run test && npm run check
```

Expected: the full vitest suite PASSES and `svelte-check` reports no new errors. **Do not commit.**

---

## Task 5: Extract the flat device table (pure refactor)

**Files:**
- Create: `dashboard/src/lib/components/devices/DeviceFlatTable.svelte`
- Modify: `dashboard/src/pages/DevicesInventory.svelte:153-200`

**Interfaces:**
- Consumes: `DeviceRow` from `lib/models`.
- Produces: `<DeviceFlatTable rows={DeviceRow[]} />` — renders the `DataTable` and its rows, including row-click navigation to `/devices/:key`.

This task changes **no behaviour**. Its whole value is that Task 6's diff is then only about grouping.

- [ ] **Step 1: Create the component**

Create `dashboard/src/lib/components/devices/DeviceFlatTable.svelte`, moving the `deviceName`/`osLabel` helpers (currently `DevicesInventory.svelte:95-103`) and the `DataTable` block (lines 153-198) verbatim:

```svelte
<script lang="ts">
  import { push } from 'svelte-spa-router';
  import DataTable from '../DataTable.svelte';
  import TimeValue from '../TimeValue.svelte';
  import type { DeviceRow } from '../../models';

  interface Props {
    rows: DeviceRow[];
  }
  let { rows }: Props = $props();

  function deviceName(d: DeviceRow): string {
    return [d.family, d.model].filter(Boolean).join(' ').trim();
  }

  function osLabel(d: DeviceRow): string {
    return [d.os_name, d.os_version].filter(Boolean).join(' ').trim() || '—';
  }
</script>

<DataTable>
  {#snippet head()}
    <tr>
      <th>Device</th>
      <th>OS</th>
      <th>Browser / Arch</th>
      <th>Last user</th>
      <th class="num">Sessions</th>
      <th class="num">Events</th>
      <th class="num">Errors</th>
      <th>Last seen</th>
    </tr>
  {/snippet}
  {#each rows as d (d.device_key)}
    <tr class="clickable" onclick={() => push('/devices/' + encodeURIComponent(d.device_key))}>
      <td>
        {#if deviceName(d)}
          <span class="dev-name">{deviceName(d)}</span>
        {:else}
          <span class="cell-mono truncate key">{d.device_key}</span>
        {/if}
      </td>
      <td class="cell-muted">{osLabel(d)}</td>
      <td class="cell-muted">{d.browser ?? d.arch ?? '—'}</td>
      <td>
        {#if d.last_distinct_id}
          <a
            class="lnk mono truncate"
            href={`#/persons/${encodeURIComponent(d.last_distinct_id)}`}
            onclick={(e) => e.stopPropagation()}
          >
            {d.last_distinct_id}
          </a>
        {:else}
          <span class="cell-muted">—</span>
        {/if}
      </td>
      <td class="num">{d.sessions_count.toLocaleString()}</td>
      <td class="num">{d.events_count.toLocaleString()}</td>
      <td class="num">
        <span class:err={d.errors_count > 0}>{d.errors_count.toLocaleString()}</span>
      </td>
      <td><TimeValue value={d.last_seen} muted /></td>
    </tr>
  {/each}
</DataTable>

<style>
  .dev-name {
    font-weight: 560;
    color: var(--text);
  }
  .key {
    display: inline-block;
    max-width: 220px;
    color: var(--text-muted);
  }
  .lnk {
    display: inline-block;
    max-width: 200px;
    color: var(--text-muted);
    font-size: 12px;
  }
  .lnk:hover {
    color: var(--primary);
    text-decoration: underline;
  }
  .err {
    color: var(--error);
    font-weight: 600;
  }
</style>
```

Svelte scopes styles per component, so the five rules above **must** move with the markup — leaving them in the page would silently drop the styling. Delete them from `DevicesInventory.svelte`'s `<style>` block, keeping `.head`, `.sub`, `.controls` and `.center` there.

- [ ] **Step 2: Use it from the page**

In `DevicesInventory.svelte`: delete the `deviceName`/`osLabel` helpers, drop the now-unused `DataTable` and `TimeValue` imports, add `import DeviceFlatTable from '../lib/components/devices/DeviceFlatTable.svelte';`, and replace the whole `<DataTable>…</DataTable>` block with:

```svelte
    <DeviceFlatTable rows={devices} />
```

- [ ] **Step 3: Verify nothing changed**

```bash
cd dashboard && npm run check && npm run test
```

Then drive the real page — a passing type-check does not prove a table still renders:

1. `preview_start` the dashboard dev server from `.claude/launch.json`.
2. Navigate to `#/devices`.
3. `read_page` and confirm the eight column headers and at least one populated row are present.
4. `read_console_messages` and confirm no new errors.

Expected: the Devices page is visually and behaviourally identical to before this task. **Do not commit.**

---

## Task 6: The grouped table and two-mode page

**Files:**
- Create: `dashboard/src/lib/components/devices/DeviceGroupTable.svelte`
- Modify: `dashboard/src/pages/DevicesInventory.svelte`

**Interfaces:**
- Consumes: `DeviceGroupRow`, `DeviceGroupKey`, `encodeGroupKey`, `decodeGroupKey`, `groupLabel` (Task 4); `DeviceFlatTable` (Task 5); `listDeviceGroups`, `listDevices` (Task 4).
- Produces: the finished feature. No later task depends on it.

- [ ] **Step 1: Create the grouped table**

Create `dashboard/src/lib/components/devices/DeviceGroupTable.svelte`:

```svelte
<script lang="ts">
  import { push } from 'svelte-spa-router';
  import DataTable from '../DataTable.svelte';
  import TimeValue from '../TimeValue.svelte';
  import { encodeGroupKey } from '../../models/device-groups';
  import type { DeviceGroupRow } from '../../models';

  interface Props {
    rows: DeviceGroupRow[];
  }
  let { rows }: Props = $props();

  function deviceName(g: DeviceGroupRow): string {
    return [g.family, g.model].filter(Boolean).join(' ').trim();
  }

  function osLabel(g: DeviceGroupRow): string {
    return [g.os_name, g.os_version].filter(Boolean).join(' ').trim() || '—';
  }

  // The four descriptor columns are the identity of a row here, so they are
  // also its {#each} key — there is no id to fall back on. ` ` cannot
  // occur in a Postgres text value, so it cannot collide with a real one the
  // way a `|` or `-` separator could.
  function rowKey(g: DeviceGroupRow): string {
    return [g.family, g.model, g.os_name, g.os_version].map((v) => v ?? ' ').join(' ');
  }

  function openGroup(g: DeviceGroupRow) {
    push('/devices?' + encodeGroupKey({
      family: g.family,
      model: g.model,
      os_name: g.os_name,
      os_version: g.os_version,
    }));
  }
</script>

<DataTable>
  {#snippet head()}
    <tr>
      <th>Device</th>
      <th>OS</th>
      <th class="num">Devices</th>
      <th class="num">Sessions</th>
      <th class="num">Events</th>
      <th class="num">Errors</th>
      <th>Last seen</th>
    </tr>
  {/snippet}
  {#each rows as g (rowKey(g))}
    <tr class="clickable" onclick={() => openGroup(g)}>
      <td>
        {#if deviceName(g)}
          <span class="dev-name">{deviceName(g)}</span>
        {:else}
          <span class="cell-muted">Unknown device</span>
        {/if}
      </td>
      <td class="cell-muted">{osLabel(g)}</td>
      <td class="num">{g.device_count.toLocaleString()}</td>
      <td class="num">{g.sessions_count.toLocaleString()}</td>
      <td class="num">{g.events_count.toLocaleString()}</td>
      <td class="num">
        <span class:err={g.errors_count > 0}>{g.errors_count.toLocaleString()}</span>
      </td>
      <td><TimeValue value={g.last_seen} muted /></td>
    </tr>
  {/each}
</DataTable>

<style>
  .dev-name {
    font-weight: 560;
    color: var(--text);
  }
  .err {
    color: var(--error);
    font-weight: 600;
  }
</style>
```

- [ ] **Step 2: Make the page two-mode**

In `dashboard/src/pages/DevicesInventory.svelte`:

Add to the imports:

```ts
  import { querystring, replace } from 'svelte-spa-router';
  import DeviceGroupTable from '../lib/components/devices/DeviceGroupTable.svelte';
  import { listDevices, listDeviceGroups } from '../lib/api/devices';
  import { decodeGroupKey, encodeGroupKey, groupLabel } from '../lib/models/device-groups';
  import type { DeviceRow, DeviceGroupRow } from '../lib/models';
```

Replace the state block (lines 22-37) with:

```ts
  const LIMIT = 50;

  // Hydrate the drill-down key from the URL once, at init — not inside an
  // effect, so it never re-runs and never fights the sync below. Same pattern
  // as Issues.svelte:44 and Events.svelte:33.
  let groupKey = $state(decodeGroupKey($querystring ?? null));
  const grouped = $derived(groupKey === null);

  let sinceDays = $state(30);
  let query = $state('');
  let search = $state('');
  let offset = $state(0);

  // Two cached views, one per mode. Separate instances rather than one shared:
  // the payloads are different types, and keeping them apart means switching
  // modes repaints from cache instead of re-fetching.
  const groupView = new CachedView<DeviceGroupRow[]>();
  const flatView = new CachedView<DeviceRow[]>();

  const groups = $derived(groupView.data ?? []);
  const devices = $derived(flatView.data ?? []);
  const rowCount = $derived(grouped ? groups.length : devices.length);

  const view = $derived(grouped ? groupView : flatView);
  const revalidating = $derived(view.revalidating);
  const loading = $derived(view.loading);
  const error = $derived(view.error);
  let refreshing = $state(false);
```

Replace `load` and the effect with:

```ts
  // `scopeKey` must be in the key: it carries the selected environment, which
  // the axios interceptor adds to the request but which appears in none of
  // these arguments. Omit it and one environment's rows are served as another's.
  //
  // The group key is in the cache key too, for the same reason — two drill-downs
  // differ only by it.
  async function load(appId: string, days: number, s: string, off: number, force = false) {
    const params = { since_days: days, search: s || undefined, limit: LIMIT, offset: off };
    if (groupKey === null) {
      await groupView.load(
        viewKey('devices.groups', appId, sessionStore.scopeKey, days, s, off, LIMIT),
        () => listDeviceGroups(appId, params),
        force,
      );
      return;
    }
    const k = groupKey;
    await flatView.load(
      viewKey('devices.list', appId, sessionStore.scopeKey, days, s, off, LIMIT, encodeGroupKey(k)),
      () => listDevices(appId, {
        ...params,
        group: '1',
        // `?? undefined`, so a NULL component is omitted from the request and
        // the backend reads it as SQL NULL. Sending `''` would filter to the
        // empty string instead, which is a different group.
        family: k.family ?? undefined,
        model: k.model ?? undefined,
        os_name: k.os_name ?? undefined,
        os_version: k.os_version ?? undefined,
      }),
      force,
    );
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const days = sinceDays;
    const s = search;
    const off = offset;
    // Touch groupKey so entering or leaving a drill-down refetches.
    groupKey;
    if (aid) void load(aid, days, s, off);
  });

  // The router owns the URL; the page follows it. `push`ing a drill-down URL
  // from the grouped table updates `$querystring`, and this is what turns that
  // into a mode change. Resetting `offset` here rather than at the call site
  // covers the browser Back button too, which no click handler sees.
  $effect(() => {
    const next = decodeGroupKey($querystring ?? null);
    if (encodeGroupKey(next ?? EMPTY_KEY) !== encodeGroupKey(groupKey ?? EMPTY_KEY)) {
      groupKey = next;
      offset = 0;
    }
  });

  function backToGroups() {
    replace('/devices');
  }
```

with, above the state block:

```ts
  // Sentinel for comparing two possibly-null keys by value. `$state` deep-
  // proxies objects, so `===` on the decoded key would never match even for
  // identical contents — compare the encoded strings instead.
  const EMPTY_KEY = { family: null, model: null, os_name: null, os_version: null };
```

**Note the trap being avoided:** `$state` wraps object values in a deep proxy, so a `groupKey === next` identity check never matches even when the contents are identical, and the effect would loop. Comparing `encodeGroupKey` output sidesteps it. See `svelte5-state-proxy-identity` in the project's memory notes.

Replace the markup's table section (currently the `{:else}` branch) with:

```svelte
  {:else}
    {#if !grouped && groupKey}
      <div class="crumb">
        <button class="back" onclick={backToGroups} type="button">
          <Icon name="arrow-left" size={14} />
          All devices
        </button>
        <span class="chip">{groupLabel(groupKey)}</span>
      </div>
    {/if}

    {#if grouped}
      <DeviceGroupTable rows={groups} />
    {:else}
      <DeviceFlatTable rows={devices} />
    {/if}

    <Pagination {offset} limit={LIMIT} count={rowCount} onchange={(o) => (offset = o)} />
  {/if}
```

Import `Icon` (`../lib/components/ui/Icon.svelte`) for the back arrow, and update the empty-state copy so the grouped mode does not say "No devices found" when a drill-down is empty:

```svelte
      <EmptyState
        title={grouped ? 'No devices found' : 'No devices in this group'}
        description={search
          ? `No devices match “${search}”.`
          : grouped
            ? 'No device telemetry has been reported in this window yet.'
            : 'This model and OS has no devices in the selected window.'}
        icon="monitor"
      />
```

Add the two new style rules:

```css
  .crumb {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 12px;
  }
  .chip {
    font-size: 12.5px;
    color: var(--text-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 3px 8px;
  }
```

Check `.back`'s existing definition in `DeviceDetail.svelte:87` and reuse the same rule rather than inventing a second back-button style.

- [ ] **Step 3: Type-check and unit test**

```bash
cd dashboard && npm run check && npm run test
```

Expected: no type errors, all tests pass.

- [ ] **Step 4: Drive the real page**

A green type-check proves nothing about a table that renders. Using the browser tools:

1. `preview_start` the dashboard dev server.
2. Navigate to `#/devices`. `read_page`: confirm the seven grouped headers (Device, OS, Devices, Sessions, Events, Errors, Last seen) and that no row repeats a model+OS pair.
3. Click a grouped row. Confirm the URL becomes `#/devices?group=1&…`, the crumb chip names the group, and the flat table's eight headers are back.
4. Click a device row. Confirm it reaches `#/devices/<key>` and the detail page loads.
5. Browser Back twice. Confirm it returns through the drill-down to the grouped view, and the table repaints each time.
6. Type in the search box, then change the date range. Confirm both still filter in each mode.
7. `read_console_messages` and `read_network_requests`: no errors, and `/device-groups` is requested in grouped mode while `/devices?group=1&…` is requested in the drill-down.

- [ ] **Step 5: Full verification**

```bash
cd backend && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cd dashboard && npm run check && npm run test
```

Expected: everything green. Report the actual output; do not claim completion on an unrun command. **Do not commit** — leave the work in the tree for review.

---

## Self-Review Notes

Checked against the spec:

- **Locked decision 1** (server-side) → Task 1. **2** (full `os_version`) → Task 1 Step 1's `17.4.0` vs `17.4.1` assertion. **3** (navigate, not expand) → Task 6 Step 1's `openGroup`. **4** (browser/arch out of the key) → absent from `DeviceGroupRow` (Task 1 Step 3) and from the grouped table (Task 6 Step 1).
- **`ORDER BY` output alias** → Task 1 Step 4, with the reason inline.
- **`device_last_distinct_id_join` excluded** → Task 1 Step 4's `scoped_join`, called out in `DeviceGroupRow`'s doc comment.
- **Empty-string vs NULL** → Task 4 Step 1's third test, Task 4 Step 3's `?? undefined` note, Task 6 Step 2's request mapping.
- **`group=1` sentinel** → Task 2 (`Option<DeviceGroupKey>`), Task 3 (`ListQuery.group`), Task 4 (`encodeGroupKey`).
- **Bind-index hazard** → Task 2 Step 4 uses `consumes_bind()` and places the group binds after env, so `EnvFilter::All`/`Unattributed` (which reserve no index) cannot shift them.
- **Every `list_devices` call site updated** → Task 2 Step 5, with the `grep` that finds them.
- **Out of scope** (`DeviceDetail`, descriptor normalization, the tiering blind spot) → no task touches any of them.
