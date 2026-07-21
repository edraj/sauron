//! Process configuration, loaded from the environment.
//!
//! Both binaries read the same struct; each uses the subset it needs. Parsing
//! is deliberately hand-rolled (no config crate) so the mapping from env var to
//! field is completely predictable in a container.

use anyhow::Context;

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub redis_url: String,
    pub ingest_port: u16,
    pub api_port: u16,
    pub jwt_secret: String,
    pub jwt_access_ttl_secs: i64,
    pub jwt_refresh_ttl_secs: i64,
    pub worker_concurrency: usize,
    pub cors_allowed_origins: Vec<String>,
    pub ingest_rate_limit_per_min: u32,
    pub ingest_max_body_bytes: usize,
    /// Optional Unix-domain-socket path to listen on instead of TCP. When
    /// unset, `sauron-ingest` binds `ingest_port` on TCP as before.
    pub ingest_uds_path: Option<String>,
    /// TCP `listen()` backlog (ignored when `ingest_uds_path` is set).
    pub ingest_backlog: u32,
    pub monitor_tick_ms: u64,
    pub monitor_batch: i64,
    pub monitor_max_concurrency: usize,
    pub monitor_check_retention_days: i64,
    pub monitor_ssrf_allow_private: bool,
    pub tier_hot_days: i64,
    pub tier_granularity: String,
    pub tier_cold_path: String,
    pub tier_drop_lag_hours: i64,
    pub tier_tick_secs: u64,
    pub tier_partition_ahead: i64,
    // --- symbolication / source maps ---
    /// In-process parsed-index LRU byte budget (megabytes).
    pub symbols_cache_mb: usize,
    /// Warm-blob Redis for symbol artifacts; `None` disables the tier (in-proc
    /// cache only). For true isolation point this at a SEPARATE Redis INSTANCE:
    /// `maxmemory` is instance-wide, so a different DB index on the ingest Redis
    /// would still let symbol blobs evict stream state. The per-blob size cap
    /// (`symbols_redis_max_blob_mb`) is the backstop when isolation isn't used.
    pub symbols_redis_url: Option<String>,
    /// Blobs larger than this are never cached in Redis (in-proc only).
    pub symbols_redis_max_blob_mb: usize,
    /// Reject uploads whose raw file exceeds this size.
    pub symbols_max_artifact_mb: usize,
    /// Decompression-bomb guard: cap on a blob's uncompressed size.
    pub symbols_max_uncompressed_mb: usize,
    /// Ingest-path symbolication time box; on timeout store raw + `pending`.
    pub symbols_ingest_timeout_ms: u64,
}

fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

fn parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    var(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Config {
    /// Load configuration from environment variables. Only `DATABASE_URL` is
    /// strictly required; everything else has a sensible default.
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = var("DATABASE_URL")
            .context("DATABASE_URL is required (e.g. postgres://sauron:sauron@localhost/sauron)")?;

        let jwt_secret = var("JWT_SECRET").unwrap_or_else(|| {
            // Safe for local dev only; production must set a real secret.
            "dev-insecure-change-me-please-0000000000000000".to_string()
        });

        let cors_allowed_origins = var("CORS_ALLOWED_ORIGINS")
            .unwrap_or_else(|| "http://localhost:3000".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(Self {
            database_url,
            redis_url: var("REDIS_URL").unwrap_or_else(|| "redis://127.0.0.1:6379".to_string()),
            ingest_port: parse("INGEST_PORT", 8081),
            api_port: parse("API_PORT", 8080),
            jwt_secret,
            jwt_access_ttl_secs: parse("JWT_ACCESS_TTL_SECS", 900),
            jwt_refresh_ttl_secs: parse("JWT_REFRESH_TTL_SECS", 2_592_000),
            worker_concurrency: parse("WORKER_CONCURRENCY", 4),
            cors_allowed_origins,
            ingest_rate_limit_per_min: parse("INGEST_RATE_LIMIT_PER_MIN", 6000),
            ingest_max_body_bytes: parse("INGEST_MAX_BODY_BYTES", 1_048_576),
            ingest_uds_path: var("INGEST_UDS_PATH"),
            ingest_backlog: parse("INGEST_BACKLOG", 4096),
            monitor_tick_ms: parse("MONITOR_TICK_MS", 1000),
            monitor_batch: parse("MONITOR_BATCH", 100),
            monitor_max_concurrency: parse("MONITOR_MAX_CONCURRENCY", 50),
            monitor_check_retention_days: parse("MONITOR_CHECK_RETENTION_DAYS", 30),
            monitor_ssrf_allow_private: var("MONITOR_SSRF_ALLOW_PRIVATE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            tier_hot_days: parse("TIER_HOT_DAYS", 30),
            tier_granularity: var("TIER_GRANULARITY").unwrap_or_else(|| "day".to_string()),
            tier_cold_path: var("TIER_COLD_PATH")
                .unwrap_or_else(|| "/var/lib/sauron/cold".to_string()),
            tier_drop_lag_hours: parse("TIER_DROP_LAG_HOURS", 24),
            tier_tick_secs: parse("TIER_TICK_SECS", 3600),
            tier_partition_ahead: parse("TIER_PARTITION_AHEAD", 7),
            symbols_cache_mb: parse("SYMBOLS_CACHE_MB", 256),
            symbols_redis_url: var("SYMBOLS_REDIS_URL"),
            symbols_redis_max_blob_mb: parse("SYMBOLS_REDIS_MAX_BLOB_MB", 8),
            symbols_max_artifact_mb: parse("SYMBOLS_MAX_ARTIFACT_MB", 128),
            symbols_max_uncompressed_mb: parse("SYMBOLS_MAX_UNCOMPRESSED_MB", 512),
            symbols_ingest_timeout_ms: parse("SYMBOLS_INGEST_TIMEOUT_MS", 150),
        })
    }
}
