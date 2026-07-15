//! Command-line surface and its validation into a [`RunConfig`] + [`Mode`].

use std::time::Duration;

use clap::Parser;

/// crebain — a load/benchmark generator for the Sauron ingest write path.
///
/// Two modes: point at a running ingest edge with --dsn, or spin up a fully
/// isolated, self-cleaning ephemeral stack with --isolated.
#[derive(Parser, Debug)]
#[command(name = "crebain", version, about)]
pub struct Args {
    // --- target selection (exactly one) ---
    /// Direct mode: SDK-format DSN of a running ingest edge
    /// (scheme://<public_key>@host:port/<app_id>). Env: CREBAIN_DSN.
    #[arg(long)]
    pub dsn: Option<String>,

    /// Isolated mode: create + migrate + seed an ephemeral database, spawn a
    /// dedicated ingest, run, then drop everything. Mutually exclusive with --dsn.
    #[arg(long)]
    pub isolated: bool,

    // --- the four headline knobs ---
    /// Size of the fixed pool of virtual users.
    #[arg(long, default_value_t = 1000)]
    pub users: usize,

    /// Run duration, in seconds.
    #[arg(long, default_value_t = 60)]
    pub duration: u64,

    /// Analytics events emitted per user per minute (0 disables).
    #[arg(long = "events-per-min", default_value_t = 10)]
    pub events_per_min: u32,

    /// Errors (issues) emitted per user per minute (0 disables).
    #[arg(long = "issues-per-min", default_value_t = 10)]
    pub issues_per_min: u32,

    /// Send envelopes uncompressed (gzip is on by default).
    #[arg(long)]
    pub no_gzip: bool,

    // --- isolated-mode options ---
    /// Postgres admin URL for CREATE/DROP DATABASE (needs CREATEDB). Env: DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Base Redis URL; the bench uses a separate DB index of it. Env: REDIS_URL.
    #[arg(long)]
    pub redis_url: Option<String>,

    /// Redis DB index to isolate the bench stream/rate-limit on.
    #[arg(long = "redis-bench-db", default_value_t = 15)]
    pub redis_bench_db: u8,

    /// Port for the spawned bench ingest (kept off 8081 to avoid a dev ingest).
    #[arg(long = "ingest-port", default_value_t = 8091)]
    pub ingest_port: u16,

    /// Path to the sauron-ingest binary (defaults to a sibling of this exe).
    #[arg(long = "ingest-bin")]
    pub ingest_bin: Option<String>,

    /// Per-app rate limit for the bench ingest (high, so it doesn't throttle).
    #[arg(long = "rate-limit", default_value_t = 100_000_000)]
    pub rate_limit: u32,

    /// Keep the bench database after the run instead of dropping it.
    #[arg(long)]
    pub keep: bool,
}

/// Validated per-run configuration shared by both modes.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub users: usize,
    pub duration: Duration,
    /// `None` when the corresponding rate is 0 (that stream disabled).
    pub event_interval: Option<Duration>,
    pub issue_interval: Option<Duration>,
    pub gzip: bool,
    pub events_per_min: u32,
    pub issues_per_min: u32,
}

/// Where the load is sent.
#[derive(Debug, Clone)]
pub enum Mode {
    Direct { dsn: String },
    Isolated(IsolatedConfig),
}

#[derive(Debug, Clone)]
pub struct IsolatedConfig {
    pub admin_database_url: String,
    pub redis_url: String,
    pub redis_bench_db: u8,
    pub ingest_port: u16,
    pub ingest_bin: Option<String>,
    pub rate_limit: u32,
    pub keep: bool,
}

/// The workload the configuration *targets*, for the achieved-vs-target report.
#[derive(Debug, Clone, Copy)]
pub struct Expected {
    pub requests: f64,
    pub duration_secs: f64,
}

impl RunConfig {
    pub fn expected(&self) -> Expected {
        let n = self.users as f64;
        let minutes = self.duration.as_secs_f64() / 60.0;
        let events = n * self.events_per_min as f64 * minutes;
        let errors = n * self.issues_per_min as f64 * minutes;
        // one request per identify (N) + one per event tick + one per issue tick.
        let requests = n + events + errors;
        Expected {
            requests,
            duration_secs: self.duration.as_secs_f64(),
        }
    }
}

fn interval_from_rate(per_min: u32) -> Option<Duration> {
    (per_min > 0).then(|| Duration::from_secs_f64(60.0 / per_min as f64))
}

impl Args {
    /// Validate flags + environment into a `(RunConfig, Mode)`.
    pub fn resolve(self) -> anyhow::Result<(RunConfig, Mode)> {
        if self.users == 0 {
            anyhow::bail!("--users must be at least 1");
        }
        if self.duration == 0 {
            anyhow::bail!("--duration must be at least 1 second");
        }

        let dsn = self.dsn.or_else(|| env_nonempty("CREBAIN_DSN"));

        let mode = match (self.isolated, dsn) {
            (true, Some(_)) => {
                anyhow::bail!("--isolated and --dsn are mutually exclusive")
            }
            (false, None) => {
                anyhow::bail!("choose a target: pass --dsn <DSN> (or CREBAIN_DSN), or --isolated")
            }
            (false, Some(dsn)) => Mode::Direct { dsn },
            (true, None) => {
                let admin_database_url = self
                    .database_url
                    .or_else(|| env_nonempty("DATABASE_URL"))
                    .ok_or_else(|| {
                        anyhow::anyhow!("--isolated needs --database-url (or DATABASE_URL)")
                    })?;
                let redis_url = self
                    .redis_url
                    .or_else(|| env_nonempty("REDIS_URL"))
                    .unwrap_or_else(|| "redis://127.0.0.1:6379".to_string());
                Mode::Isolated(IsolatedConfig {
                    admin_database_url,
                    redis_url,
                    redis_bench_db: self.redis_bench_db,
                    ingest_port: self.ingest_port,
                    ingest_bin: self.ingest_bin,
                    rate_limit: self.rate_limit,
                    keep: self.keep,
                })
            }
        };

        let cfg = RunConfig {
            users: self.users,
            duration: Duration::from_secs(self.duration),
            event_interval: interval_from_rate(self.events_per_min),
            issue_interval: interval_from_rate(self.issues_per_min),
            gzip: !self.no_gzip,
            events_per_min: self.events_per_min,
            issues_per_min: self.issues_per_min,
        };
        Ok((cfg, mode))
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_disabled_at_zero() {
        assert_eq!(interval_from_rate(0), None);
        assert_eq!(interval_from_rate(60), Some(Duration::from_secs(1)));
        assert_eq!(interval_from_rate(10), Some(Duration::from_secs(6)));
    }

    #[test]
    fn expected_requests_matches_model() {
        let cfg = RunConfig {
            users: 1000,
            duration: Duration::from_secs(60),
            event_interval: interval_from_rate(10),
            issue_interval: interval_from_rate(10),
            gzip: true,
            events_per_min: 10,
            issues_per_min: 10,
        };
        // identifies 1000 + events 10000 + errors 10000 = 21000 requests
        assert_eq!(cfg.expected().requests.round() as u64, 21_000);
    }
}
