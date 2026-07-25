//! SSRF guard: refuse to connect to loopback / private / link-local / metadata
//! addresses.
//!
//! The classifier ([`is_blocked_ip`]) is pure and unit-tested. Enforcement is
//! **IP-pinned**: [`SsrfResolver`] implements `reqwest::dns::Resolve`, so the
//! validation happens *inside* the resolution the HTTP client actually connects
//! with. There is exactly one DNS lookup and the addresses that pass the check
//! are the addresses hyper dials — which closes the DNS-rebinding / TOCTOU hole
//! that a separate "check then connect" guard leaves open (a low-TTL record
//! could previously answer with a public IP for the check and a private one for
//! the connect).
//!
//! For raw TCP probes, [`resolve_checked`] returns the validated `SocketAddr`s
//! so the caller can connect to a pinned address rather than re-resolving the
//! hostname.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// True if the address is one we refuse to connect to unless explicitly allowed.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || o[0] == 0
                // 100.64.0.0/10 carrier-grade NAT
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                // 192.0.0.0/24 IETF protocol assignments
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                // 198.18.0.0/15 benchmarking
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                // 240.0.0.0/4 reserved
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique local
                || (seg[0] & 0xfe00) == 0xfc00
                // fe80::/10 link local
                || (seg[0] & 0xffc0) == 0xfe80
                // 2002::/16 6to4 — embeds a v4 address; check the embedded v4
                || (seg[0] == 0x2002 && {
                    let embedded = std::net::Ipv4Addr::new(
                        (seg[1] >> 8) as u8,
                        (seg[1] & 0xff) as u8,
                        (seg[2] >> 8) as u8,
                        (seg[2] & 0xff) as u8,
                    );
                    is_blocked_ip(IpAddr::V4(embedded))
                })
                // ::ffff:0:0/96 handled by to_ipv4_mapped above; ::/96 (v4-compatible)
                || (seg[0..6] == [0, 0, 0, 0, 0, 0] && seg[6] != 0)
        }
    }
}

/// Resolve `host` and return the addresses, failing if **any** resolved address
/// is blocked. Returning the addresses lets the caller pin them, so the value
/// that was validated is the value that gets dialed.
///
/// `host` is a bare hostname or IP literal (no port, no brackets).
pub async fn resolve_checked(host: &str, allow_private: bool) -> Result<Vec<SocketAddr>, String> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, 0u16))
        .await
        .map_err(|e| format!("DNS resolution failed: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("target {host} did not resolve"));
    }
    if !allow_private {
        for addr in &addrs {
            if is_blocked_ip(addr.ip()) {
                return Err(format!("target {host} resolves to a blocked address"));
            }
        }
    }
    Ok(addrs)
}

// `guard_target` used to live here: it resolved a host, checked the addresses,
// then threw them away and let the caller re-resolve at connect time — a TOCTOU
// window a rebinding DNS server could drive straight through. Both probe paths
// now pin what they validated (`resolve_checked`) or validate inside the
// client's own resolution (`SsrfResolver`), leaving it with no callers, so it is
// gone rather than left as a weaker alternative someone could reach for.

/// A `reqwest` DNS resolver that rejects blocked addresses at resolution time.
///
/// Because reqwest connects to exactly what this returns, a host that resolves
/// to a private/loopback/metadata address can never be dialed — no second
/// lookup happens between the check and the connect.
#[derive(Debug, Clone)]
pub struct SsrfResolver {
    allow_private: bool,
}

impl SsrfResolver {
    pub fn new(allow_private: bool) -> Self {
        Self { allow_private }
    }
}

impl Resolve for SsrfResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private = self.allow_private;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = resolve_checked(&host, allow_private)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// A `reqwest::ClientBuilder` pre-wired with the SSRF-guarding resolver and
/// redirects disabled (a followed redirect would otherwise be a second,
/// unvalidated request target).
///
/// Environment proxies are disabled too. reqwest picks up `HTTP_PROXY` /
/// `HTTPS_PROXY` / `ALL_PROXY` by default and dials the proxy through this same
/// connector, so the *proxy's* hostname gets SSRF-checked: a self-hosted
/// deployment whose proxy resolves to a private address (the normal case for an
/// internal Squid) had every probe fail, including probes of public targets —
/// and reqwest's error `Display` hides the cause, so it surfaced as an opaque
/// "request failed".
///
/// Routing egress through a proxy would also defeat the guard's premise: the
/// address this resolver validates would no longer be the address ultimately
/// connected to, since the proxy does its own resolution server-side. Bypassing
/// the proxy is therefore both the working and the safe choice.
pub fn guarded_client_builder(allow_private: bool) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .dns_resolver(Arc::new(SsrfResolver::new(allow_private)))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn blocks_private_and_local_v4() {
        assert!(is_blocked_ip(ip("127.0.0.1")));
        assert!(is_blocked_ip(ip("10.1.2.3")));
        assert!(is_blocked_ip(ip("192.168.0.5")));
        assert!(is_blocked_ip(ip("172.16.9.9")));
        assert!(is_blocked_ip(ip("169.254.169.254"))); // cloud metadata
        assert!(is_blocked_ip(ip("0.0.0.0")));
        assert!(is_blocked_ip(ip("100.64.0.1"))); // CGNAT
    }

    #[test]
    fn blocks_additional_reserved_v4_ranges() {
        assert!(is_blocked_ip(ip("192.0.0.1"))); // IETF protocol assignments
        assert!(is_blocked_ip(ip("198.18.0.1"))); // benchmarking
        assert!(is_blocked_ip(ip("198.19.255.255"))); // benchmarking (upper half)
        assert!(is_blocked_ip(ip("240.0.0.1"))); // reserved
        assert!(is_blocked_ip(ip("255.255.255.255"))); // broadcast
    }

    #[test]
    fn allows_public_v4() {
        assert!(!is_blocked_ip(ip("8.8.8.8")));
        assert!(!is_blocked_ip(ip("1.1.1.1")));
        assert!(!is_blocked_ip(ip("93.184.216.34")));
        // 192.0.2.0/24 (TEST-NET) is not in a blocked range and stays allowed.
        assert!(!is_blocked_ip(ip("192.0.2.1")));
    }

    #[test]
    fn blocks_local_v6_allows_public_v6() {
        assert!(is_blocked_ip(ip("::1")));
        assert!(is_blocked_ip(ip("fc00::1"))); // unique local
        assert!(is_blocked_ip(ip("fe80::1"))); // link local
        assert!(is_blocked_ip(ip("::ffff:127.0.0.1"))); // v4-mapped loopback
        assert!(!is_blocked_ip(ip("2606:4700:4700::1111")));
    }

    #[test]
    fn blocks_6to4_wrapping_a_private_v4() {
        // 2002:a00:0001:: embeds 10.0.0.1
        assert!(is_blocked_ip(ip("2002:a00:1::")));
        // 2002:0808:0808:: embeds 8.8.8.8 (public) → allowed
        assert!(!is_blocked_ip(ip("2002:808:808::")));
    }

    #[tokio::test]
    async fn resolve_checked_rejects_loopback_literal() {
        let err = resolve_checked("127.0.0.1", false).await.unwrap_err();
        assert!(err.contains("blocked"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn resolve_checked_allows_loopback_when_permitted() {
        let addrs = resolve_checked("127.0.0.1", true).await.unwrap();
        assert!(!addrs.is_empty());
    }
}
