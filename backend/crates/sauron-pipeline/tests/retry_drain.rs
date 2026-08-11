//! The retry tier's drain, against a real Redis.
//!
//! This file exists because the backoff tier is the part of failure recovery
//! that can lose data or never terminate, and neither failure mode is visible
//! to a unit test:
//!
//! * a drain that removed members BEFORE re-enqueueing them would lose every
//!   job in flight when the process died, and the pure tests would still pass;
//! * an attempt counter that did not survive the round trip through the stream
//!   would make the retry loop INFINITE, and nothing in `retry.rs`'s unit tests
//!   could tell — they never put a job through the loop.
//!
//! ## Why a dedicated Redis
//!
//! `drain_due` re-enqueues onto `keys::INGEST_STREAM` — the real key, by
//! design, since testing against a fake one would not prove the drain reaches
//! the stream workers actually read. So this test MUST NOT run against a shared
//! Redis: it would inject synthetic payloads into a live ingest stream.
//!
//! It therefore reads `TEST_ISOLATED_REDIS_URL`, not `TEST_REDIS_URL`, and
//! skips when unset. Point it at a throwaway instance:
//!
//! ```text
//! docker run -d --name sauron-retrytest-redis -p 16399:6379 \
//!     redis:7 redis-server --save '' --appendonly no
//! export TEST_ISOLATED_REDIS_URL=redis://127.0.0.1:16399
//! ```
//!
//! Persistence off is not incidental: with `stop-writes-on-bgsave-error` a
//! failed snapshot turns into a write outage that reads exactly like the code
//! under test failing.

use sauron_pipeline::classify::MAX_ATTEMPTS;
use sauron_pipeline::retry::{self, ATTEMPT_TTL_SECS, RETRY_DRAIN_LIMIT};
use sauron_redis::{keys, RedisStore};

const MAXLEN: usize = 10_000;

/// One test's private backoff set and stream.
///
/// Per-test keys, not the `keys::` constants. Sharing them cost a real bug:
/// cargo runs these concurrently, every test saw every other test's parked
/// jobs, and the suite passed alone while failing under full-workspace load —
/// `retry_depth` came back 4 where 1 was expected. A test that passes only when
/// run by itself is worse than no test, because it certifies the tier as
/// verified while proving nothing.
struct Keys {
    retry: String,
    stream: String,
}

impl Keys {
    fn new() -> Self {
        let id = uuid::Uuid::new_v4();
        Self {
            retry: format!("sauron:test:retry:{id}"),
            stream: format!("sauron:test:stream:{id}"),
        }
    }
}

async fn store() -> Option<(RedisStore, Keys)> {
    let url = std::env::var("TEST_ISOLATED_REDIS_URL").ok()?;
    let s = RedisStore::connect(&url).await.ok()?;
    Some((s, Keys::new()))
}

async fn cleanup(redis: &RedisStore, k: &Keys) {
    let _ = redis.del(&k.retry).await;
    let _ = redis.del(&k.stream).await;
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

async fn stream_len(redis: &RedisStore, k: &Keys) -> u64 {
    redis
        .stream_stats_with_retry(&k.stream, keys::CONSUMER_GROUP, &k.stream, &k.retry)
        .await
        .expect("stream stats")
        .length
}

/// The happy path, end to end: a parked job waits, then comes back onto the
/// ingest stream and leaves the backoff set.
#[tokio::test]
async fn a_parked_job_returns_to_the_stream_when_due() {
    let Some((redis, k)) = store().await else {
        eprintln!("TEST_ISOLATED_REDIS_URL unset — skipping");
        return;
    };
    let now = now_ms();

    retry::park_to(&redis, &k.retry, "{\"item\":\"a\"}", now)
        .await
        .unwrap();
    assert_eq!(redis.retry_depth_of(&k.retry).await.unwrap(), 1);

    // Not yet due: the backoff must actually hold the job back, or "retry after
    // 60s" is really "retry immediately" and a database that needs a moment to
    // recover gets hammered instead.
    assert_eq!(
        retry::drain_due_between(&redis, &k.retry, &k.stream, now, MAXLEN).await,
        0
    );
    assert_eq!(redis.retry_depth_of(&k.retry).await.unwrap(), 1);
    assert_eq!(stream_len(&redis, &k).await, 0);

    // Due.
    let later = now + retry::RETRY_BACKOFF_SECS * 1000 + 1;
    assert_eq!(
        retry::drain_due_between(&redis, &k.retry, &k.stream, later, MAXLEN).await,
        1
    );
    assert_eq!(
        redis.retry_depth_of(&k.retry).await.unwrap(),
        0,
        "a re-enqueued job must leave the set, or every tick re-injects it"
    );
    assert_eq!(
        stream_len(&redis, &k).await,
        1,
        "the job must reach the stream"
    );

    cleanup(&redis, &k).await;
}

/// Two byte-identical payloads must BOTH survive the round trip.
///
/// A sorted set de-duplicates by member, so without the nonce in the parked
/// envelope the second of two identical failing events would overwrite the
/// first and vanish — a silent loss that no counter anywhere would show.
#[tokio::test]
async fn two_identical_payloads_both_come_back() {
    let Some((redis, k)) = store().await else {
        eprintln!("TEST_ISOLATED_REDIS_URL unset — skipping");
        return;
    };
    let now = now_ms();

    retry::park_to(&redis, &k.retry, "{\"same\":1}", now)
        .await
        .unwrap();
    retry::park_to(&redis, &k.retry, "{\"same\":1}", now)
        .await
        .unwrap();
    assert_eq!(
        redis.retry_depth_of(&k.retry).await.unwrap(),
        2,
        "identical payloads must occupy two members"
    );

    let later = now + retry::RETRY_BACKOFF_SECS * 1000 + 1;
    assert_eq!(
        retry::drain_due_between(&redis, &k.retry, &k.stream, later, MAXLEN).await,
        2
    );
    assert_eq!(
        stream_len(&redis, &k).await,
        2,
        "both events must be replayed"
    );

    cleanup(&redis, &k).await;
}

/// The per-tick ceiling binds, and what it leaves behind stays parked rather
/// than being dropped.
#[tokio::test]
async fn the_drain_limit_bounds_a_tick_without_losing_the_remainder() {
    let Some((redis, k)) = store().await else {
        eprintln!("TEST_ISOLATED_REDIS_URL unset — skipping");
        return;
    };
    let now = now_ms();
    let total = RETRY_DRAIN_LIMIT + 25;
    for i in 0..total {
        retry::park_to(&redis, &k.retry, &format!("{{\"i\":{i}}}"), now)
            .await
            .unwrap();
    }

    let later = now + retry::RETRY_BACKOFF_SECS * 1000 + 1;
    let moved = retry::drain_due_between(&redis, &k.retry, &k.stream, later, MAXLEN).await;
    assert_eq!(
        moved, RETRY_DRAIN_LIMIT,
        "one tick must not drain everything"
    );
    assert_eq!(
        redis.retry_depth_of(&k.retry).await.unwrap() as usize,
        total - RETRY_DRAIN_LIMIT,
        "the remainder must stay parked, not be discarded"
    );

    // The next tick picks up exactly the rest — no job is stranded.
    let moved2 = retry::drain_due_between(&redis, &k.retry, &k.stream, later, MAXLEN).await;
    assert_eq!(moved2, total - RETRY_DRAIN_LIMIT);
    assert_eq!(redis.retry_depth_of(&k.retry).await.unwrap(), 0);
    assert_eq!(stream_len(&redis, &k).await as usize, total);

    cleanup(&redis, &k).await;
}

/// A member written by an older binary, in a shape this one cannot parse, must
/// be discarded rather than left in place.
///
/// Left in place it would be re-read and re-failed on every tick forever,
/// permanently occupying part of the drain limit and starving real retries —
/// a slow wedge that presents as "retries stopped working" long after the
/// deploy that caused it.
#[tokio::test]
async fn an_unparseable_member_is_discarded_not_retried_forever() {
    let Some((redis, k)) = store().await else {
        eprintln!("TEST_ISOLATED_REDIS_URL unset — skipping");
        return;
    };
    let now = now_ms();

    redis
        .retry_schedule_to(&k.retry, "this is not the envelope shape", now - 1)
        .await
        .unwrap();
    retry::park_to(&redis, &k.retry, "{\"good\":1}", now - 61_000)
        .await
        .unwrap();

    let moved = retry::drain_due_between(&redis, &k.retry, &k.stream, now, MAXLEN).await;
    assert_eq!(moved, 1, "only the parseable job is re-enqueued");
    assert_eq!(
        redis.retry_depth_of(&k.retry).await.unwrap(),
        0,
        "the junk member must be removed, or it wedges the drain forever"
    );
    assert_eq!(stream_len(&redis, &k).await, 1);

    cleanup(&redis, &k).await;
}

/// **The infinite-loop guard.**
///
/// This is the single most important assertion in the retry tier. A drained job
/// re-enters as an ordinary stream payload, so its attempt count cannot travel
/// with it — the count lives in a separate key hashed from the job's bytes. If
/// that lookup did not survive the round trip, every re-injected retry would
/// read as attempt 1 and a permanently-broken payload would cycle through the
/// backoff set for the life of the deployment.
///
/// Simulated at the level the worker works at: bump on failure, park, drain,
/// bump again on the next failure — and assert the count keeps climbing to the
/// terminal threshold rather than resetting.
#[tokio::test]
async fn the_attempt_count_survives_the_round_trip_and_terminates() {
    let Some((redis, k)) = store().await else {
        eprintln!("TEST_ISOLATED_REDIS_URL unset — skipping");
        return;
    };
    let payload = "{\"永\":\"broken\"}";
    let hash = retry::job_hash(payload);
    let _ = redis.clear_attempt(&hash).await;

    let mut attempt = 0;
    let mut parks = 0;
    for round in 0..10 {
        attempt = redis.bump_attempt(&hash, ATTEMPT_TTL_SECS).await.unwrap();
        if attempt < MAX_ATTEMPTS {
            let now = now_ms();
            retry::park_to(&redis, &k.retry, payload, now)
                .await
                .unwrap();
            parks += 1;
            // Re-injected: the next failure must see attempt N+1, not 1.
            let moved =
                retry::drain_due_between(&redis, &k.retry, &k.stream, now + 61_000, MAXLEN).await;
            assert_eq!(moved, 1, "round {round}: the parked job must come back");
        } else {
            break;
        }
    }

    assert_eq!(
        attempt, MAX_ATTEMPTS,
        "the count must reach the terminal threshold; a reset here means the \
         retry loop NEVER ends"
    );
    assert_eq!(
        parks,
        (MAX_ATTEMPTS - 1) as usize,
        "a job must be parked exactly MAX_ATTEMPTS-1 times before going terminal"
    );

    // Going terminal clears the counter, so a later identical payload gets its
    // own full allowance rather than starting already-exhausted.
    redis.clear_attempt(&hash).await.unwrap();
    assert_eq!(
        redis.bump_attempt(&hash, ATTEMPT_TTL_SECS).await.unwrap(),
        1
    );

    redis.clear_attempt(&hash).await.unwrap();
    cleanup(&redis, &k).await;
}
