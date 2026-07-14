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
        })
    }
}
