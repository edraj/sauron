//! Supervised background loops for `sauron-api`.
//!
//! **No task's initialization may `?` out of `main()`.** This is the absolute
//! rule, and the blast radius is exact: `packaging/rpm/systemd/
//! sauron-migrate.service` has no `[Install]` section, `sauron.spec` runs
//! `%systemd_postun_with_restart` on the API, and `sauron-api.service` is
//! `Restart=on-failure` with no `StartLimit` override — so a `?` against a table
//! a skipped migration never created burns systemd's five-starts-in-ten-seconds
//! budget and leaves the unit `failed` with no HTTP surface left to diagnose
//! from. Start with an empty state, log at ERROR on every failed tick, and let
//! the `/health` age make it visible.
//!
//! The `tick + last_prune` loop in `bins/sauron-alerts/src/main.rs` looks like
//! the thing to copy and is not: there the loop *is* `main()`, so a panic aborts
//! the process and `Restart=on-failure` brings it back. Here it would be a
//! detached task whose `JoinHandle` is dropped. The workspace sets no
//! `panic = "abort"` and `sauron-telemetry` installs no panic hook, so tokio
//! catches the panic and the task simply stops — the HTTP server keeps serving,
//! `/health` keeps returning 200, systemd sees a healthy unit, and the work stops
//! forever. Hence: each tick is its own `tokio::spawn` whose `JoinHandle` is
//! awaited, so a panic arrives as an `Err(JoinError)` the loop can log.

use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::error;

/// Ceiling on how far a failing task backs off. Long enough to stop hammering a
/// broken dependency, short enough that recovery is noticed within one `/health`
/// glance.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Liveness of one supervised loop.
pub struct TaskHealth {
    name: &'static str,
    last_success: Mutex<Option<Instant>>,
    consecutive_failures: AtomicU32,
}

impl TaskHealth {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            last_success: Mutex::new(None),
            consecutive_failures: AtomicU32::new(0),
        }
    }

    /// Seconds since the last successful tick, or `None` before the first one.
    pub fn last_success_secs(&self) -> Option<u64> {
        let guard = self.last_success.lock().ok()?;
        guard.map(|t| t.elapsed().as_secs())
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    fn record_success(&self) {
        if let Ok(mut g) = self.last_success.lock() {
            *g = Some(Instant::now());
        }
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    fn record_failure(&self) -> u32 {
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// One row of `/health`'s `tasks` array.
#[derive(serde::Serialize)]
pub struct TaskStatus {
    pub name: &'static str,
    /// `null` before the first success. It NEVER changes the status code:
    /// `packaging/rpm/SETUP.md` documents `curl -fsS .../health` and
    /// `tests/http_env_scoping.rs` polls it for readiness, and both read a non-2xx
    /// as "the API is down", which a stalled reaper is not.
    pub last_success_secs: Option<u64>,
    pub consecutive_failures: u32,
}

fn registry() -> &'static Mutex<Vec<Arc<TaskHealth>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Arc<TaskHealth>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Every supervised task's current state, for `/health`.
pub fn snapshot() -> Vec<TaskStatus> {
    let guard = match registry().lock() {
        Ok(g) => g,
        // A poisoned registry means a previous reader panicked while holding it.
        // `/health` must still answer; an empty task list is the honest report.
        Err(_) => return Vec::new(),
    };
    guard
        .iter()
        .map(|h| TaskStatus {
            name: h.name,
            last_success_secs: h.last_success_secs(),
            consecutive_failures: h.consecutive_failures(),
        })
        .collect()
}

fn backoff(interval: Duration, failures: u32) -> Duration {
    std::cmp::min(interval * failures.min(8), MAX_BACKOFF)
}

/// Run `f` every `interval`, forever, surviving panics and errors.
///
/// The returned handle is registered for `/health` synchronously, before the
/// initial jitter sleep, so a 15-minute task is visible from the first request
/// rather than only after its first tick.
pub fn supervise<F, Fut>(name: &'static str, interval: Duration, f: F) -> Arc<TaskHealth>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let health = Arc::new(TaskHealth::new(name));
    if let Ok(mut g) = registry().lock() {
        g.push(health.clone());
    }

    let h = health.clone();
    tokio::spawn(async move {
        // Per-process jitter. With N instances behind a load balancer, a rolling
        // restart otherwise makes all N fire the identical reaper within seconds
        // of each other — N times the lock contention and N times the pool
        // pressure, at the same instant, forever.
        let span_nanos = u64::try_from(interval.as_nanos())
            .unwrap_or(u64::MAX)
            .max(1);
        let jitter = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64
            % span_nanos;
        tokio::time::sleep(Duration::from_nanos(jitter)).await;

        loop {
            // Each tick is its own spawn so a panic comes back as Err(JoinError)
            // rather than unwinding this loop out of existence.
            match tokio::spawn(f()).await {
                Ok(Ok(())) => {
                    h.record_success();
                    tokio::time::sleep(interval).await;
                }
                Ok(Err(e)) => {
                    let n = h.record_failure();
                    error!(task = name, error = %e, consecutive_failures = n, "background task failed");
                    tokio::time::sleep(backoff(interval, n)).await;
                }
                Err(join) => {
                    let n = h.record_failure();
                    error!(task = name, error = %join, consecutive_failures = n, "background task panicked");
                    tokio::time::sleep(backoff(interval, n)).await;
                }
            }
        }
    });

    health
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A panicking task must not stop the loop. In this process the loop is a
    /// detached task whose `JoinHandle` is dropped; the workspace sets no
    /// `panic = "abort"` and nothing installs a panic hook, so tokio catches the
    /// panic and the task simply stops — the HTTP server keeps serving, /health
    /// keeps returning 200, systemd sees a healthy unit, and transactional email
    /// stops forever.
    #[tokio::test]
    async fn a_panicking_tick_does_not_kill_the_loop() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let health = supervise("test_panics", Duration::from_millis(10), || async {
            let n = CALLS.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                panic!("first tick explodes");
            }
            Ok(())
        });

        for _ in 0..200 {
            if health.last_success_secs().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            health.last_success_secs().is_some(),
            "the loop never recovered from a panicking tick"
        );
        assert!(CALLS.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn failures_accumulate_and_a_success_resets_them() {
        static FAIL: AtomicU32 = AtomicU32::new(1);
        let health = supervise("test_failures", Duration::from_millis(10), || async {
            if FAIL.load(Ordering::SeqCst) == 1 {
                anyhow::bail!("still broken");
            }
            Ok(())
        });

        for _ in 0..200 {
            if health.consecutive_failures() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            health.consecutive_failures() >= 2,
            "failures did not accumulate"
        );
        assert!(health.last_success_secs().is_none());

        FAIL.store(0, Ordering::SeqCst);
        for _ in 0..400 {
            if health.consecutive_failures() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            health.consecutive_failures(),
            0,
            "a success did not reset the counter"
        );
        assert!(health.last_success_secs().is_some());
    }

    #[test]
    fn backoff_grows_with_failures_and_stops_at_five_minutes() {
        let i = Duration::from_secs(60);
        assert_eq!(backoff(i, 0), Duration::from_secs(0));
        assert_eq!(backoff(i, 1), Duration::from_secs(60));
        assert_eq!(backoff(i, 4), Duration::from_secs(240));
        assert_eq!(backoff(i, 5), Duration::from_secs(300));
        assert_eq!(backoff(i, 8), Duration::from_secs(300));
        assert_eq!(backoff(i, 900), Duration::from_secs(300));
    }

    #[tokio::test]
    async fn a_supervised_task_is_visible_on_the_health_snapshot_immediately() {
        // Registration happens synchronously, before the initial jitter sleep.
        // A 15-minute hygiene task that only appeared after its first tick would
        // be indistinguishable from one that was never mounted.
        supervise("test_registered", Duration::from_secs(900), || async {
            Ok(())
        });
        let names: Vec<&str> = snapshot().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"test_registered"), "got: {names:?}");
    }
}
