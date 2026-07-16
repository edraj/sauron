//! Command-line surface and its validation into a [`RunConfig`] + [`Mode`].

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};

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

    /// Write a self-contained HTML benchmark report to this path.
    #[arg(long)]
    pub report: Option<std::path::PathBuf>,

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

    // --- concurrency / transport knobs ---
    /// Max concurrent in-flight requests (= worker-pool size). The connection ceiling.
    #[arg(long = "max-inflight", default_value_t = 8192)]
    pub max_inflight: usize,

    /// Seconds over which the initial per-user identify is spread (no t=0 herd).
    #[arg(long, default_value_t = 5)]
    pub ramp: u64,

    /// Explicit aggregate request rate (req/s). Overrides the users×rates derivation.
    #[arg(long)]
    pub rps: Option<f64>,

    /// Number of loopback source IPs to fan out across (default: auto from --max-inflight).
    #[arg(long = "source-ips")]
    pub source_ips: Option<usize>,

    /// Transport for requests.
    #[arg(long, value_enum, default_value_t = Transport::Tcp)]
    pub transport: Transport,

    /// Unix-domain-socket path (isolated mode auto-picks one when --transport uds).
    #[arg(long = "uds-path")]
    pub uds_path: Option<PathBuf>,

    /// Hold connections open for a literal-concurrency (peak-sockets) demo instead of a request loop.
    #[arg(long = "live-sockets")]
    pub live_sockets: bool,
}

/// Transport used to carry requests to the ingest edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Transport {
    Tcp,
    Uds,
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
    pub report_path: Option<std::path::PathBuf>,
    /// Max concurrent in-flight requests (= worker-pool size). The connection ceiling.
    pub max_inflight: usize,
    /// Duration over which the initial per-user identify is spread (no t=0 herd).
    pub ramp: Duration,
    /// Number of loopback source IPs to fan out across (`None` = auto from `max_inflight`).
    pub source_ips: Option<usize>,
    /// Transport for requests (TCP or Unix-domain socket).
    pub transport: Transport,
    /// Unix-domain-socket path (isolated mode auto-picks one when `transport` is `Uds`).
    pub uds_path: Option<PathBuf>,
    /// Hold connections open for a literal-concurrency (peak-sockets) demo instead of a request loop.
    pub live_sockets: bool,
    /// Explicit aggregate request rate (req/s). Overrides the users×rates derivation.
    pub rps: Option<f64>,
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
    pub transport: Transport,
    /// Unix-domain-socket path the spawned ingest listens on when `transport`
    /// is `Uds` (auto-picked by `resolve()` when not given explicitly).
    pub uds_path: Option<PathBuf>,
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
        if self.max_inflight == 0 {
            anyhow::bail!("--max-inflight must be at least 1");
        }

        let dsn = self.dsn.or_else(|| env_nonempty("CREBAIN_DSN"));

        // Resolved once and shared between `RunConfig` and `IsolatedConfig`:
        // an explicit `--uds-path` wins; otherwise isolated + UDS auto-picks a
        // fresh path in the system temp dir. Direct mode never auto-picks —
        // there's no harness to spawn an ingest listening on it.
        let uds_path = self.uds_path.clone().or_else(|| {
            (self.transport == Transport::Uds && self.isolated).then(|| {
                std::env::temp_dir().join(format!("crebain-{}.sock", uuid::Uuid::new_v4().simple()))
            })
        });

        if self.transport == Transport::Uds && uds_path.is_none() {
            anyhow::bail!("--transport uds requires --uds-path in direct mode");
        }

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
                    transport: self.transport,
                    uds_path: uds_path.clone(),
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
            report_path: self.report,
            max_inflight: self.max_inflight,
            ramp: Duration::from_secs(self.ramp),
            source_ips: self.source_ips,
            transport: self.transport,
            uds_path,
            live_sockets: self.live_sockets,
            rps: self.rps,
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
            report_path: None,
            max_inflight: 8192,
            ramp: Duration::from_secs(5),
            source_ips: None,
            transport: Transport::Tcp,
            uds_path: None,
            live_sockets: false,
            rps: None,
        };
        // identifies 1000 + events 10000 + errors 10000 = 21000 requests
        assert_eq!(cfg.expected().requests.round() as u64, 21_000);
    }

    #[test]
    fn report_path_flows_into_runconfig() {
        let args = Args::try_parse_from([
            "crebain", "--isolated", "--database-url", "postgres://x/y", "--report", "out.html",
        ])
        .unwrap();
        let (cfg, _mode) = args.resolve().unwrap();
        assert_eq!(cfg.report_path, Some(std::path::PathBuf::from("out.html")));
    }

    #[test]
    fn report_path_defaults_to_none() {
        let args = Args::try_parse_from([
            "crebain", "--isolated", "--database-url", "postgres://x/y",
        ])
        .unwrap();
        let (cfg, _mode) = args.resolve().unwrap();
        assert_eq!(cfg.report_path, None);
    }

    #[test]
    fn defaults_max_inflight_and_transport() {
        let args = Args::try_parse_from(["crebain", "--isolated", "--database-url", "postgres://x/y"]).unwrap();
        let (cfg, _m) = args.resolve().unwrap();
        assert_eq!(cfg.max_inflight, 8192);
        assert_eq!(cfg.transport, Transport::Tcp);
        assert!(!cfg.live_sockets);
        assert_eq!(cfg.ramp, std::time::Duration::from_secs(5));
    }

    #[test]
    fn parses_concurrency_and_uds() {
        let args = Args::try_parse_from([
            "crebain", "--isolated", "--database-url", "postgres://x/y",
            "--max-inflight", "50000", "--transport", "uds", "--live-sockets", "--ramp", "10",
        ]).unwrap();
        let (cfg, _m) = args.resolve().unwrap();
        assert_eq!(cfg.max_inflight, 50000);
        assert_eq!(cfg.transport, Transport::Uds);
        assert!(cfg.live_sockets);
        assert_eq!(cfg.ramp, std::time::Duration::from_secs(10));
    }

    #[test]
    fn isolated_uds_auto_picks_shared_path() {
        let args = Args::try_parse_from([
            "crebain",
            "--isolated",
            "--database-url",
            "postgres://x/y",
            "--transport",
            "uds",
        ])
        .unwrap();
        let (cfg, mode) = args.resolve().unwrap();
        let Mode::Isolated(icfg) = mode else {
            panic!("expected Mode::Isolated");
        };
        assert_eq!(icfg.transport, Transport::Uds);
        assert!(icfg.uds_path.is_some());
        assert_eq!(cfg.uds_path, icfg.uds_path);
    }

    #[test]
    fn direct_uds_without_path_is_rejected() {
        let args = Args::try_parse_from([
            "crebain",
            "--dsn",
            "http://pk@localhost:8081/app",
            "--transport",
            "uds",
        ])
        .unwrap();
        assert!(args.resolve().is_err());
    }

    #[test]
    fn rejects_max_inflight_zero() {
        let args = Args::try_parse_from([
            "crebain", "--isolated", "--database-url", "postgres://x/y", "--max-inflight", "0",
        ]).unwrap();
        assert!(args.resolve().is_err());
    }
}
