//! Probe execution. HTTP via reqwest, TCP via a raw connect. Each returns a
//! `ProbeResult`; a failed probe is data (down + reason), never an error.

use std::time::{Duration, Instant};

use crate::ssrf::guard_target;
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

fn down(reason: impl Into<String>, ms: Option<i32>) -> ProbeResult {
    ProbeResult { up: false, status_code: None, response_time_ms: ms, error: Some(reason.into()) }
}

/// Extract the bare host from an HTTP URL or a `host:port` string (for the SSRF guard).
fn host_of(spec: &ProbeSpec) -> Option<String> {
    match spec.kind {
        Kind::Http => reqwest::Url::parse(&spec.target).ok().and_then(|u| u.host_str().map(|s| s.to_string())),
        Kind::Tcp => spec.target.rsplit_once(':').map(|(h, _)| h.to_string()),
    }
}

/// Run one probe: SSRF-guard the target, then dispatch HTTP or TCP.
///
/// Known limitation (MVP): `guard_target` resolves DNS and validates the
/// resolved addresses, but those addresses are then discarded — the actual
/// HTTP/TCP call below re-resolves the host independently. A low-TTL DNS
/// answer that points at a public IP during the guard check and a
/// blocked/private IP moments later (DNS rebinding, TOCTOU) can therefore
/// still slip through. Full IP-pinning (guard and connect using the same
/// resolved address) is a tracked follow-up, not implemented here.
pub async fn probe(spec: &ProbeSpec, client: &reqwest::Client, allow_private: bool) -> ProbeResult {
    // SSRF guard first.
    if let Some(host) = host_of(spec) {
        if let Err(e) = guard_target(&host, allow_private).await {
            return down(e, None);
        }
    } else {
        return down("invalid target", None);
    }

    match spec.kind {
        Kind::Http => probe_http(spec, client).await,
        Kind::Tcp => probe_tcp(spec).await,
    }
}

async fn probe_http(spec: &ProbeSpec, client: &reqwest::Client) -> ProbeResult {
    let method = reqwest::Method::from_bytes(spec.method.as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let mut req = client.request(method, &spec.target).timeout(spec.timeout);
    for (k, v) in &spec.headers {
        req = req.header(k, v);
    }
    if let Some(b) = &spec.body {
        req = req.body(b.clone());
    }

    let start = Instant::now();
    match req.send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            let ms = start.elapsed().as_millis() as i32;
            let (up, err) = evaluate_http(code, &body, &spec.expected_status, spec.body_assertion.as_deref());
            ProbeResult { up, status_code: Some(code as i32), response_time_ms: Some(ms), error: err }
        }
        Err(e) => {
            let ms = start.elapsed().as_millis() as i32;
            let reason = if e.is_timeout() { "connection timeout".to_string() } else { format!("request failed: {e}") };
            ProbeResult { up: false, status_code: None, response_time_ms: Some(ms), error: Some(reason) }
        }
    }
}

async fn probe_tcp(spec: &ProbeSpec) -> ProbeResult {
    let start = Instant::now();
    match tokio::time::timeout(spec.timeout, tokio::net::TcpStream::connect(&spec.target)).await {
        Ok(Ok(_stream)) => {
            let ms = start.elapsed().as_millis() as i32;
            ProbeResult { up: true, status_code: None, response_time_ms: Some(ms), error: None }
        }
        Ok(Err(e)) => down(format!("TCP connect failed: {e}"), Some(start.elapsed().as_millis() as i32)),
        Err(_) => down("connection timeout", Some(start.elapsed().as_millis() as i32)),
    }
}
