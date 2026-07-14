//! SSRF guard: reject probing loopback / private / link-local / metadata
//! targets by default. The classifier is pure and unit-tested; `guard_target`
//! resolves DNS and checks every resolved address (defends against rebinding).

use std::net::IpAddr;

/// True if the address is one we refuse to probe unless explicitly allowed.
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
        }
    }
}

/// Resolve `host` and fail if any resolved address is blocked (unless
/// `allow_private`). `host` is a bare hostname or IP literal (no port).
pub async fn guard_target(host: &str, allow_private: bool) -> Result<(), String> {
    if allow_private {
        return Ok(());
    }
    // `lookup_host` needs a port; :0 is fine, we only use the IPs.
    let addrs = tokio::net::lookup_host((host, 0u16))
        .await
        .map_err(|e| format!("DNS resolution failed: {e}"))?;
    let mut saw_any = false;
    for addr in addrs {
        saw_any = true;
        if is_blocked_ip(addr.ip()) {
            return Err(format!("target {} resolves to a blocked address", host));
        }
    }
    if !saw_any {
        return Err(format!("target {host} did not resolve"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr { s.parse().unwrap() }

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
    fn allows_public_v4() {
        assert!(!is_blocked_ip(ip("8.8.8.8")));
        assert!(!is_blocked_ip(ip("1.1.1.1")));
        assert!(!is_blocked_ip(ip("93.184.216.34")));
    }

    #[test]
    fn blocks_local_v6_allows_public_v6() {
        assert!(is_blocked_ip(ip("::1")));
        assert!(is_blocked_ip(ip("fc00::1")));   // unique local
        assert!(is_blocked_ip(ip("fe80::1")));   // link local
        assert!(is_blocked_ip(ip("::ffff:127.0.0.1"))); // v4-mapped loopback
        assert!(!is_blocked_ip(ip("2606:4700:4700::1111")));
    }
}
