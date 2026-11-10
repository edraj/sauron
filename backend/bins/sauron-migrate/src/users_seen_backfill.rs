//! `sauron-migrate backfill-users-seen`: repair `issues.users_seen` from Redis.
//!
//! `users_seen` is the Postgres copy of each issue's HyperLogLog
//! (`keys::issue_users`). Before the ingest pending set existed, its throttled
//! write-back silently dropped updates, so an issue could be left at a count
//! far below the HyperLogLog for good (eight users, `users_seen = 1`). Ingest
//! now converges every issue it touches; this repairs the ones it never will
//! touch again. Run it once after upgrading.
//!
//! Operator-run only, like every backfill here: never part of the no-argument
//! migrate path, and nothing calls it automatically.
//!
//! Safe to run against a live ingest, and idempotent:
//! - every write is `repo::set_issue_users_seen`, ONE row per autocommit
//!   statement, so it holds no row lock while waiting for another and cannot
//!   form a lock cycle with the ingest's issue upsert;
//! - that write is monotonic (`AND users_seen < $n`), so it only ever RAISES a
//!   count to the current estimate, and a second run raises nothing.
//!
//! It can raise a count that a data purge lowered: the purge recomputes
//! `users_seen` exactly from the surviving rows, but a HyperLogLog cannot
//! forget a member, so its estimate still includes the purged users. That is
//! an overcount, the direction the purge itself treats as the safe failure.
//!
//! Issues with no HyperLogLog at all are counted separately and reported: if
//! that is most of them, `REDIS_URL` (or its db index) most likely points at
//! the wrong Redis, and the run repaired nothing.
//!
//! Issues are visited by keyset on `id`, a page at a time, with one pipelined
//! round trip of `PFCOUNT`s per page.

use sauron_db::{repo, PgPool};
use sauron_redis::{keys, RedisStore};

/// Issues per page: one id query and one pipelined Redis round trip each.
pub const PAGE: i64 = 1_000;

/// Running totals, reported after every page.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Issues visited.
    pub checked: u64,
    /// Issues whose stored count was below the HyperLogLog and was raised.
    pub raised: u64,
    /// Issues with no user sketch in Redis (`PFCOUNT` 0): nothing to repair
    /// from. A diagnostic, see the module docs.
    pub no_sketch: u64,
}

/// Visit every issue once, raising `users_seen` to the HyperLogLog's
/// `PFCOUNT` wherever it is lower. `progress` is called after each page.
pub async fn backfill_all(
    pool: &PgPool,
    redis: &RedisStore,
    page: i64,
    mut progress: impl FnMut(&Outcome),
) -> anyhow::Result<Outcome> {
    let mut conn = sauron_db::conn(pool).await?;
    let mut out = Outcome::default();
    let mut after = None;
    loop {
        let ids = repo::issue_ids_after(&mut conn, after, page).await?;
        let Some(&last) = ids.last() else {
            break;
        };
        let hll_keys: Vec<String> = ids
            .iter()
            .map(|id| keys::issue_users(&id.to_string()))
            .collect();
        let counts = redis.pf_count_each(&hll_keys).await?;
        for (&issue_id, &n) in ids.iter().zip(&counts) {
            // A missing HyperLogLog counts 0, which can never raise anything:
            // skip the round trip, but count it.
            if n > 0 {
                out.raised += repo::set_issue_users_seen(&mut conn, issue_id, n).await? as u64;
            } else {
                out.no_sketch += 1;
            }
        }
        out.checked += ids.len() as u64;
        progress(&out);
        if (ids.len() as i64) < page {
            break;
        }
        after = Some(last);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sauron_db::models::NewIssue;

    fn swap_database(url: &str, new_db: &str) -> String {
        let (scheme, rest) = url
            .split_once("://")
            .expect("TEST_DATABASE_URL must be scheme://...");
        let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..auth_end];
        let after = &rest[auth_end..];
        let query = after.find('?').map(|i| &after[i..]).unwrap_or("");
        format!("{scheme}://{authority}/{new_db}{query}")
    }

    /// Three issues across two pages (page size 2, so the keyset cursor is
    /// exercised): one stale (stored 1, HyperLogLog 5), one ahead of its
    /// HyperLogLog (stored 10, HyperLogLog 3: must NOT be lowered), and one
    /// with no HyperLogLog at all. A second run must change nothing.
    #[tokio::test]
    async fn raises_stale_counts_only_and_is_idempotent() {
        let Ok(admin_url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };
        let Ok(redis_url) = std::env::var("TEST_REDIS_URL") else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let redis = RedisStore::connect(&redis_url).await.expect("redis");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock");
        // `sauron_test_<unix secs>_…`: the shape sauron-db's stale-database
        // reaper recognises, should this test panic before cleanup.
        let db_name = format!(
            "sauron_test_{}_mg{}x{}",
            now.as_secs(),
            std::process::id(),
            now.subsec_nanos()
        );
        sauron_db::create_test_database(&admin_url, &db_name)
            .await
            .expect("create test database");
        let pool =
            sauron_db::build_pool(&swap_database(&admin_url, &db_name), 2).expect("test pool");

        let mut conn = sauron_db::conn(&pool).await.expect("checkout");
        let org = repo::create_org(&mut conn, "mg org", &format!("mg-org-{}", now.as_nanos()))
            .await
            .expect("org");
        let project = repo::create_project(&mut conn, org.id, "mg project", "mg-project")
            .await
            .expect("project");
        let app = repo::create_app(&mut conn, project.id, "mg app", "mg-app", "web")
            .await
            .expect("app");
        let mut ids = Vec::new();
        for fp in ["stale", "ahead", "no-hll"] {
            ids.push(
                repo::upsert_issue(
                    &mut conn,
                    NewIssue {
                        app_id: app.id,
                        fingerprint: fp,
                        type_: "TypeError",
                        title: fp,
                        culprit: "",
                        level: "error",
                        first_seen: Utc::now(),
                        last_seen: Utc::now(),
                        times_seen: 1,
                    },
                )
                .await
                .expect("issue"),
            );
        }
        let (stale, ahead, no_hll) = (ids[0], ids[1], ids[2]);
        repo::set_issue_users_seen(&mut conn, stale, 1)
            .await
            .unwrap();
        repo::set_issue_users_seen(&mut conn, ahead, 10)
            .await
            .unwrap();
        let stale_key = keys::issue_users(&stale.to_string());
        let ahead_key = keys::issue_users(&ahead.to_string());
        redis
            .pf_add_many(&stale_key, &["a", "b", "c", "d", "e"])
            .await
            .unwrap();
        redis
            .pf_add_many(&ahead_key, &["a", "b", "c"])
            .await
            .unwrap();

        let mut pages = Vec::new();
        let first = backfill_all(&pool, &redis, 2, |o| pages.push(*o))
            .await
            .expect("backfill");
        assert_eq!(
            first,
            Outcome {
                checked: 3,
                raised: 1,
                no_sketch: 1,
            },
            "one raised, one ahead (left alone), one with no sketch (counted)"
        );
        assert_eq!(pages.len(), 2, "three issues at two per page is two pages");

        let rows = repo::issues_by_ids(&mut conn, app.id, &ids).await.unwrap();
        let stored = |id| rows.iter().find(|i| i.id == id).expect("issue").users_seen;
        assert_eq!(stored(stale), 5, "a stale count is raised to PFCOUNT");
        assert_eq!(stored(ahead), 10, "a higher count is never lowered");
        assert_eq!(stored(no_hll), 0, "no HyperLogLog, nothing to write");

        let second = backfill_all(&pool, &redis, 2, |_| {})
            .await
            .expect("re-run");
        assert_eq!(
            second,
            Outcome {
                checked: 3,
                raised: 0,
                no_sketch: 1,
            },
            "a second run changes nothing"
        );

        drop(conn);
        drop(pool);
        let _ = redis.del(&stale_key).await;
        let _ = redis.del(&ahead_key).await;
        sauron_db::drop_database(&admin_url, &db_name)
            .await
            .expect("drop test database");
    }
}
