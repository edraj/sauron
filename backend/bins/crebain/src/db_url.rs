//! Pure helpers for deriving the isolated stack's connection URLs from the
//! admin/base URLs. Kept dependency-free and unit-tested — no network, no I/O.

use uuid::Uuid;

/// A fresh, safe database name for one benchmark run, e.g.
/// `crebain_bench_0f8c…`. Lowercase hex only, so it passes
/// `sauron_db`'s identifier validation.
pub fn bench_db_name() -> String {
    format!("crebain_bench_{}", Uuid::new_v4().simple())
}

/// Return `url` with its database (path) segment replaced by `new_db`, preserving
/// scheme, authority (user:pass@host:port), and any `?query`.
///
/// `postgres://u:p@h:5432/app?sslmode=disable` → `…/crebain_bench_x?sslmode=disable`
pub fn swap_database(url: &str, new_db: &str) -> anyhow::Result<String> {
    let (scheme, rest) = split_scheme(url)?;
    // authority runs until the first '/' or '?'
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let after = &rest[auth_end..];
    // preserve any query string that followed the old db name
    let query = after.find('?').map(|i| &after[i..]).unwrap_or("");
    Ok(format!("{scheme}://{authority}/{new_db}{query}"))
}

/// Return `url` with its Redis database index set to `index`, preserving scheme
/// and authority. `redis://h:6379` → `redis://h:6379/15`.
pub fn swap_redis_db(url: &str, index: u8) -> anyhow::Result<String> {
    let (scheme, rest) = split_scheme(url)?;
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    Ok(format!("{scheme}://{authority}/{index}"))
}

/// Split `scheme://rest` into `(scheme, rest)`.
fn split_scheme(url: &str) -> anyhow::Result<(&str, &str)> {
    url.split_once("://")
        .filter(|(s, _)| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("malformed URL (expected scheme://…): {url:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_name_is_safe_and_unique() {
        let a = bench_db_name();
        let b = bench_db_name();
        assert_ne!(a, b);
        assert!(a.starts_with("crebain_bench_"));
        assert!(a
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'));
    }

    #[test]
    fn swaps_database_preserving_authority() {
        assert_eq!(
            swap_database("postgres://sauron:sauron@localhost:5432/sauron", "bench").unwrap(),
            "postgres://sauron:sauron@localhost:5432/bench"
        );
    }

    #[test]
    fn swaps_database_preserving_query() {
        assert_eq!(
            swap_database("postgres://u:p@h:5432/app?sslmode=disable", "bench").unwrap(),
            "postgres://u:p@h:5432/bench?sslmode=disable"
        );
    }

    #[test]
    fn swaps_database_when_no_db_present() {
        assert_eq!(
            swap_database("postgres://localhost", "bench").unwrap(),
            "postgres://localhost/bench"
        );
    }

    #[test]
    fn swaps_redis_index() {
        assert_eq!(
            swap_redis_db("redis://127.0.0.1:6379", 15).unwrap(),
            "redis://127.0.0.1:6379/15"
        );
        assert_eq!(
            swap_redis_db("redis://:pass@host:6379/0", 7).unwrap(),
            "redis://:pass@host:6379/7"
        );
        assert_eq!(
            swap_redis_db("rediss://host", 3).unwrap(),
            "rediss://host/3"
        );
    }

    #[test]
    fn rejects_malformed_url() {
        assert!(swap_database("not-a-url", "bench").is_err());
        assert!(swap_redis_db("://nohost", 1).is_err());
    }
}
