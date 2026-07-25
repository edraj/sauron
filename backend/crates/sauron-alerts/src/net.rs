//! SSRF-safe outbound HTTP for alert delivery.
//!
//! Two layers, both reusing the monitor's audited IP classifier:
//!
//! 1. The shared client is built with
//!    [`guarded_client_builder`](sauron_monitor_core::ssrf::guarded_client_builder),
//!    whose DNS resolver validates addresses *inside* the resolution hyper
//!    connects with. The address that passes the check is the address dialed, so
//!    a low-TTL record cannot answer public-then-private (DNS rebinding/TOCTOU).
//! 2. An explicit [`resolve_checked`] pre-flight, because hyper resolves IP
//!    **literals** internally without ever consulting the resolver — so
//!    `http://169.254.169.254/` would otherwise skip layer 1. It also fails fast
//!    before a socket is opened.
//!
//! Redirects are disabled (a 3xx would be an unvalidated second target) and
//! response bodies are read under a hard byte cap.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use sauron_monitor_core::ssrf::resolve_checked;

/// Cap on the response body read back from a delivery endpoint — enough for a
/// useful error in the delivery log, never enough to exhaust memory.
const MAX_RESPONSE_BYTES: usize = 8 * 1024;

/// One shared client per `allow_private` setting: building a client per
/// delivery would throw away the connection pool and redo TLS every time.
static GUARDED: OnceLock<reqwest::Client> = OnceLock::new();
static PERMISSIVE: OnceLock<reqwest::Client> = OnceLock::new();

fn client(allow_private: bool) -> &'static reqwest::Client {
    let cell = if allow_private { &PERMISSIVE } else { &GUARDED };
    cell.get_or_init(|| {
        sauron_monitor_core::ssrf::guarded_client_builder(allow_private)
            .user_agent("Sauron-Alerts/1.0")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Parse `url` into its host, rejecting non-http(s) schemes.
fn host_of(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("bad url: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported scheme: {other}")),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "url has no host".to_string())?;
    // `host_str` keeps the brackets on an IPv6 literal; the resolver cannot.
    Ok(host
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(host)
        .to_string())
}

/// Validate a destination URL's host without sending anything.
pub async fn preflight(url: &str, allow_private: bool) -> Result<(), String> {
    let host = host_of(url)?;
    resolve_checked(&host, allow_private).await.map(|_| ())
}

/// Send `raw` as a JSON request body to `url`, pre-flighting the target and
/// reading back a bounded error snippet. Returns `Err(message)` on a blocked
/// target, transport failure, or non-2xx status.
///
/// Takes the already-serialized bytes (rather than a `Value`) so a caller that
/// signs the payload can sign the exact bytes that go on the wire.
pub async fn send_json_bytes(
    method: reqwest::Method,
    url: &str,
    raw: Vec<u8>,
    headers: &[(String, String)],
    timeout: Duration,
    allow_private: bool,
) -> Result<(), String> {
    preflight(url, allow_private).await?;

    let mut hmap = HeaderMap::new();
    hmap.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    for (k, v) in headers {
        // Skip rather than fail on a malformed custom header.
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            hmap.insert(name, value);
        }
    }

    let resp = client(allow_private)
        .request(method, url)
        .headers(hmap)
        .timeout(timeout)
        .body(raw)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "request failed: timeout".to_string()
            } else {
                format!("request failed: {e}")
            }
        })?;

    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let snippet = read_capped(resp).await;
    Err(format!("HTTP {}: {snippet}", status.as_u16()))
}

/// POST a JSON value to `url`.
pub async fn post_json(
    url: &str,
    body: &serde_json::Value,
    headers: &[(String, String)],
    timeout: Duration,
    allow_private: bool,
) -> Result<(), String> {
    let raw = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    send_json_bytes(
        reqwest::Method::POST,
        url,
        raw,
        headers,
        timeout,
        allow_private,
    )
    .await
}

/// Read at most [`MAX_RESPONSE_BYTES`] of a response body, lossily as UTF-8.
async fn read_capped(mut resp: reqwest::Response) -> String {
    let mut buf: Vec<u8> = Vec::new();
    while buf.len() < MAX_RESPONSE_BYTES {
        match resp.chunk().await {
            Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
            _ => break,
        }
    }
    buf.truncate(MAX_RESPONSE_BYTES);
    String::from_utf8_lossy(&buf).replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_parses_and_strips_v6_brackets() {
        assert_eq!(host_of("https://example.com/x").unwrap(), "example.com");
        assert_eq!(host_of("http://[::1]:8080/y").unwrap(), "::1");
    }

    #[test]
    fn host_of_rejects_bad_scheme() {
        assert!(host_of("file:///etc/passwd").is_err());
        assert!(host_of("ftp://example.com").is_err());
    }

    #[tokio::test]
    async fn preflight_blocks_loopback_and_metadata_literals() {
        assert!(preflight("http://127.0.0.1:8080/x", false)
            .await
            .unwrap_err()
            .contains("blocked"));
        assert!(preflight("http://169.254.169.254/latest/meta-data/", false)
            .await
            .unwrap_err()
            .contains("blocked"));
    }

    #[tokio::test]
    async fn preflight_allows_loopback_when_permitted() {
        assert!(preflight("http://127.0.0.1:8080/x", true).await.is_ok());
    }
}
