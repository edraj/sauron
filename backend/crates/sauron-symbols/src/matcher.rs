//! Match a stack frame's file path to an uploaded artifact's `name`.
//!
//! v1 matching is by release + path. Frame paths are full URLs
//! (`https://cdn.example.com/static/app.min.js?v=3`); artifacts are typically
//! uploaded under a normalized `~/path` name (or just a basename). We normalize
//! both to a canonical `~/path` form (origin + query/fragment stripped) and
//! compare, with a basename fallback.

/// Canonicalize a path/URL to a `~/path` form: strip scheme+host, query, and
/// fragment. Already-normalized `~/…` values pass through unchanged.
pub fn normalize(abs_path: &str) -> String {
    let p = abs_path
        .split(['?', '#'])
        .next()
        .unwrap_or(abs_path)
        .trim();

    if let Some(rest) = p.strip_prefix("~/") {
        return format!("~/{}", rest.trim_start_matches('/'));
    }

    let path = if let Some(idx) = p.find("://") {
        let after = &p[idx + 3..];
        match after.find('/') {
            Some(slash) => &after[slash..], // keep the leading '/'
            None => "/",
        }
    } else {
        p
    };
    format!("~/{}", path.trim_start_matches('/'))
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Exact normalized-path match — no basename fallback. Preferred over [`matches`]
/// so a precise path wins over a coincidental same-basename collision.
pub fn matches_exact(frame_path: &str, artifact_name: &str) -> bool {
    normalize(frame_path) == normalize(artifact_name)
}

/// True when `frame_path` refers to the same file as an artifact named
/// `artifact_name`: exact normalized match, or same basename.
pub fn matches(frame_path: &str, artifact_name: &str) -> bool {
    if matches_exact(frame_path, artifact_name) {
        return true;
    }
    let nf = normalize(frame_path);
    let na = normalize(artifact_name);
    let bf = basename(&nf);
    !bf.is_empty() && bf == basename(&na)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_origin_and_query() {
        assert_eq!(
            normalize("https://cdn.example.com/static/app.min.js?v=3"),
            "~/static/app.min.js"
        );
        assert_eq!(normalize("/static/app.min.js"), "~/static/app.min.js");
        assert_eq!(normalize("app.min.js"), "~/app.min.js");
        assert_eq!(normalize("~/static/app.min.js"), "~/static/app.min.js");
        assert_eq!(
            normalize("https://x.io/a/b.js#frag"),
            "~/a/b.js"
        );
    }

    #[test]
    fn matches_full_and_basename() {
        assert!(matches(
            "https://x.io/static/app.min.js",
            "~/static/app.min.js"
        ));
        assert!(matches("https://x.io/static/app.min.js", "app.min.js"));
        assert!(!matches(
            "https://x.io/static/app.min.js",
            "~/static/other.js"
        ));
    }

    #[test]
    fn exact_match_excludes_basename_collisions() {
        // Exact requires the whole path; a same-basename different-dir does not.
        assert!(matches_exact(
            "https://x.io/static/app.min.js",
            "~/static/app.min.js"
        ));
        assert!(!matches_exact(
            "https://x.io/static/app.min.js",
            "~/vendor/app.min.js"
        ));
        // ...whereas the lenient matcher would accept the basename collision.
        assert!(matches(
            "https://x.io/static/app.min.js",
            "~/vendor/app.min.js"
        ));
    }
}
