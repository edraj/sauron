//! Probe execution. HTTP via reqwest, TCP via a raw connect. Each returns a
//! `ProbeResult`; a failed probe is data (down + reason), never an error.

use std::time::{Duration, Instant};

use crate::ssrf::resolve_checked;
use crate::state::ProbeResult;
use crate::status::evaluate_http;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Http,
    Tcp,
}

#[derive(Clone, Debug)]
pub struct ProbeSpec {
    pub kind: Kind,
    /// URL for HTTP; `host:port` for TCP.
    pub target: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub expected_status: String,
    pub body_assertion: Option<String>,
    pub follow_redirects: bool,
    pub timeout: Duration,
}

/// Maximum HTTP response body we buffer, in bytes. Prevents an unbounded read
/// (or a gzip decompression bomb — `gzip` is enabled on the `reqwest` client)
/// from exhausting memory; only affects assertion matching, never the status
/// code, which is already known before the body is read.
const MAX_BODY_BYTES: usize = 1_048_576;

fn down(reason: impl Into<String>, ms: Option<i32>) -> ProbeResult {
    ProbeResult {
        up: false,
        status_code: None,
        response_time_ms: ms,
        error: Some(reason.into()),
    }
}

/// Strip a single leading `[` and matching trailing `]` from an IPv6 literal
/// (`[::1]` -> `::1`), if present. Bare hosts pass through unchanged.
fn strip_brackets(s: &str) -> &str {
    s.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(s)
}

/// Extract the bare host from an HTTP URL or a `host:port` string (for the SSRF guard).
///
/// IPv6 literals are bracketed in both forms (`https://[::1]:8080/`,
/// `[::1]:443`) — `Url::host_str()` and `rsplit_once(':')` both hand back the
/// host *with* its brackets still attached. `tokio::net::lookup_host` cannot
/// resolve a bracketed string, so we strip them here before returning.
pub(crate) fn host_of(spec: &ProbeSpec) -> Option<String> {
    match spec.kind {
        Kind::Http => reqwest::Url::parse(&spec.target)
            .ok()
            .and_then(|u| u.host_str().map(|s| strip_brackets(s).to_string())),
        Kind::Tcp => spec
            .target
            .rsplit_once(':')
            .map(|(h, _)| strip_brackets(h).to_string()),
    }
}

/// Run one probe: SSRF-guard the target, then dispatch HTTP or TCP.
///
/// SSRF enforcement is IP-pinned, so DNS rebinding cannot slip a private
/// address past the check:
/// - **HTTP** — `client` must be built with
///   [`ssrf::guarded_client_builder`](crate::ssrf::guarded_client_builder), whose
///   resolver validates addresses *inside* the resolution hyper connects with.
///   The explicit check below still runs so IP-literal targets (which hyper
///   resolves internally, never calling the resolver) are rejected too, and so a
///   bad target fails fast without opening a connection.
/// - **TCP** — [`probe_tcp`] connects to the exact `SocketAddr` that was
///   validated, never re-resolving the hostname.
pub async fn probe(spec: &ProbeSpec, client: &reqwest::Client, allow_private: bool) -> ProbeResult {
    // SSRF guard first, bounded by the probe's own timeout — an unbounded
    // guard call would let a slow/unresponsive resolver make a single probe
    // run far longer than `spec.timeout`.
    let Some(host) = host_of(spec) else {
        return down("invalid target", None);
    };
    let pinned =
        match tokio::time::timeout(spec.timeout, resolve_checked(&host, allow_private)).await {
            Ok(Ok(addrs)) => addrs,
            Ok(Err(e)) => return down(e, None),
            Err(_) => return down("DNS resolution timeout", None),
        };

    match spec.kind {
        Kind::Http => probe_http(spec, client).await,
        Kind::Tcp => probe_tcp(spec, &pinned).await,
    }
}

async fn probe_http(spec: &ProbeSpec, client: &reqwest::Client) -> ProbeResult {
    let method =
        reqwest::Method::from_bytes(spec.method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut req = client.request(method, &spec.target).timeout(spec.timeout);
    for (k, v) in &spec.headers {
        req = req.header(k, v);
    }
    if let Some(b) = &spec.body {
        req = req.body(b.clone());
    }

    let start = Instant::now();
    match req.send().await {
        Ok(mut resp) => {
            let code = resp.status().as_u16();
            // Bounded body read: stop pulling chunks once we hit the cap so a
            // huge (or gzip-bomb-decompressed) body can't exhaust memory. The
            // status code above is already final; a truncated body only ever
            // affects the assertion substring check below.
            let mut buf: Vec<u8> = Vec::new();
            while buf.len() < MAX_BODY_BYTES {
                match resp.chunk().await {
                    Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            buf.truncate(MAX_BODY_BYTES);
            let body = String::from_utf8_lossy(&buf);
            let ms = start.elapsed().as_millis() as i32;
            let (up, err) = evaluate_http(
                code,
                &body,
                &spec.expected_status,
                spec.body_assertion.as_deref(),
            );
            ProbeResult {
                up,
                status_code: Some(code as i32),
                response_time_ms: Some(ms),
                error: err,
            }
        }
        Err(e) => {
            let ms = start.elapsed().as_millis() as i32;
            let reason = if e.is_timeout() {
                "connection timeout".to_string()
            } else {
                format!("request failed: {e}")
            };
            ProbeResult {
                up: false,
                status_code: None,
                response_time_ms: Some(ms),
                error: Some(reason),
            }
        }
    }
}

/// Connect to a **pinned**, already-validated address. `pinned` carries the
/// addresses `resolve_checked` approved (with port 0); the port comes from the
/// target string, so no second resolution can substitute a different host.
async fn probe_tcp(spec: &ProbeSpec, pinned: &[std::net::SocketAddr]) -> ProbeResult {
    let start = Instant::now();
    let Some(port) = spec
        .target
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
    else {
        return down("invalid target: expected host:port", None);
    };
    if pinned.is_empty() {
        return down("target did not resolve", None);
    }
    // Try every resolved address, not just the first. Passing a `host:port`
    // string to `TcpStream::connect` used to make tokio walk the whole list, so
    // narrowing to `pinned.first()` for SSRF pinning quietly turned a host with
    // several A records into a single-address probe: one drained node behind a
    // failover pair, or an AAAA sorted first on a v4-only prober, and the
    // monitor reports DOWN while the service is up. The timeout is the budget
    // for the attempts as a whole, so a long list cannot extend the probe.
    let addrs: Vec<std::net::SocketAddr> = pinned
        .iter()
        .map(|a| std::net::SocketAddr::new(a.ip(), port))
        .collect();
    let connect_any = async {
        let mut last_err = None;
        for addr in &addrs {
            match tokio::net::TcpStream::connect(addr).await {
                Ok(stream) => return Ok(stream),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.expect("addrs is non-empty"))
    };
    match tokio::time::timeout(spec.timeout, connect_any).await {
        Ok(Ok(_stream)) => {
            let ms = start.elapsed().as_millis() as i32;
            ProbeResult {
                up: true,
                status_code: None,
                response_time_ms: Some(ms),
                error: None,
            }
        }
        Ok(Err(e)) => down(
            format!("TCP connect failed: {e}"),
            Some(start.elapsed().as_millis() as i32),
        ),
        Err(_) => down(
            "connection timeout",
            Some(start.elapsed().as_millis() as i32),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_http(target: &str) -> ProbeSpec {
        ProbeSpec {
            kind: Kind::Http,
            target: target.to_string(),
            method: "GET".to_string(),
            headers: Vec::new(),
            body: None,
            expected_status: "200-399".to_string(),
            body_assertion: None,
            follow_redirects: true,
            timeout: Duration::from_millis(1000),
        }
    }

    fn spec_tcp(target: &str) -> ProbeSpec {
        ProbeSpec {
            kind: Kind::Tcp,
            target: target.to_string(),
            method: "GET".to_string(),
            headers: Vec::new(),
            body: None,
            expected_status: "200-399".to_string(),
            body_assertion: None,
            follow_redirects: false,
            timeout: Duration::from_millis(1000),
        }
    }

    #[test]
    fn host_of_http_plain_host() {
        assert_eq!(
            host_of(&spec_http("https://example.com/health")),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn host_of_http_ipv6_strips_brackets() {
        assert_eq!(
            host_of(&spec_http("https://[::1]:8080/")),
            Some("::1".to_string())
        );
    }

    #[test]
    fn host_of_tcp_plain_host() {
        assert_eq!(
            host_of(&spec_tcp("db.example.com:5432")),
            Some("db.example.com".to_string())
        );
    }

    #[test]
    fn host_of_tcp_ipv6_strips_brackets() {
        assert_eq!(host_of(&spec_tcp("[::1]:443")), Some("::1".to_string()));
    }

    #[test]
    fn host_of_http_unparseable_url_is_none() {
        assert_eq!(host_of(&spec_http("not a url")), None);
    }

    #[test]
    fn host_of_http_url_without_host_is_none() {
        // Well-formed URL, but schemes like `mailto:` have no authority/host.
        assert_eq!(host_of(&spec_http("mailto:test@example.com")), None);
    }
}
