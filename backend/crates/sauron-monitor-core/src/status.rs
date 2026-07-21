//! Expected-status parsing and HTTP result evaluation (pure).

/// True if `code` satisfies the `expected` spec: comma-separated parts, each a
/// single code (`204`) or an inclusive range (`200-399`). Unparseable parts are
/// skipped.
pub fn status_matches(expected: &str, code: u16) -> bool {
    for part in expected.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = part.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (lo.trim().parse::<u16>(), hi.trim().parse::<u16>()) {
                if code >= lo && code <= hi {
                    return true;
                }
            }
        } else if let Ok(v) = part.parse::<u16>() {
            if code == v {
                return true;
            }
        }
    }
    false
}

/// Evaluate an HTTP response. Returns `(up, error_reason)`.
pub fn evaluate_http(
    status_code: u16,
    body: &str,
    expected: &str,
    assertion: Option<&str>,
) -> (bool, Option<String>) {
    if !status_matches(expected, status_code) {
        return (false, Some(format!("HTTP {status_code}")));
    }
    if let Some(a) = assertion {
        if !a.is_empty() && !body.contains(a) {
            return (false, Some(format!("assertion '{a}' not found")));
        }
    }
    (true, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_and_csv_matching() {
        assert!(status_matches("200-399", 200));
        assert!(status_matches("200-399", 301));
        assert!(status_matches("200-399", 399));
        assert!(!status_matches("200-399", 400));
        assert!(!status_matches("200-399", 500));
        assert!(status_matches("200,204", 204));
        assert!(!status_matches("200,204", 201));
        assert!(status_matches("200-299,301", 301));
    }

    #[test]
    fn evaluate_http_status_and_assertion() {
        // status ok, no assertion -> up
        assert_eq!(evaluate_http(200, "hello", "200-399", None), (true, None));
        // status mismatch -> down with "HTTP 503"
        assert_eq!(
            evaluate_http(503, "boom", "200-399", None),
            (false, Some("HTTP 503".to_string()))
        );
        // status ok but assertion missing -> down
        assert_eq!(
            evaluate_http(200, "hello world", "200-399", Some("OK")),
            (false, Some("assertion 'OK' not found".to_string()))
        );
        // status ok and assertion present -> up
        assert_eq!(
            evaluate_http(200, "all OK here", "200-399", Some("OK")),
            (true, None)
        );
        // empty assertion is ignored
        assert_eq!(evaluate_http(200, "x", "200-399", Some("")), (true, None));
    }
}
