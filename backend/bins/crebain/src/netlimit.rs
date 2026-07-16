//! Ephemeral-port budget planning, loopback detection, and RLIMIT_NOFILE raising.
//! Pure functions are unit-tested; the libc calls are best-effort and reported.

use std::net::Ipv4Addr;

pub const FALLBACK_PORT_BUDGET: u32 = 28_232;
/// Fraction of a tuple's raw port range we treat as usable (TIME_WAIT headroom).
const USABLE_FRACTION: f64 = 0.9;

/// Usable simultaneous connections per (src_ip, dst_ip, dst_port) tuple, read from
/// `/proc/sys/net/ipv4/ip_local_port_range` and de-rated for TIME_WAIT headroom.
pub fn ephemeral_port_budget() -> u32 {
    let raw = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|s| {
            let mut it = s.split_whitespace();
            let lo: u32 = it.next()?.parse().ok()?;
            let hi: u32 = it.next()?.parse().ok()?;
            (hi >= lo).then_some(hi - lo + 1)
        })
        .unwrap_or(FALLBACK_PORT_BUDGET);
    ((raw as f64 * USABLE_FRACTION) as u32).max(1)
}

pub fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(v4) = host.parse::<Ipv4Addr>() {
        return v4.is_loopback();
    }
    if let Ok(v6) = host.parse::<std::net::Ipv6Addr>() {
        return v6.is_loopback();
    }
    false
}

/// The i-th (0-indexed) usable loopback source address, walking 127.0.0.0/8 as
/// 127.0.0.1, 127.0.0.2, … 127.0.0.254, 127.0.1.1, … (skip .0 and .255 in the low octet).
pub fn nth_loopback_ip(i: usize) -> Ipv4Addr {
    let per_block = 254usize; // .1 ..= .254
    let block = i / per_block; // increments the third octet
    let within = (i % per_block) as u8; // 0 ..= 253
    let c = (block % 256) as u8;
    let b = ((block / 256) % 256) as u8;
    Ipv4Addr::new(127, b, c, within + 1)
}

#[derive(Debug, Clone)]
pub struct FanoutPlan {
    pub source_ips: Vec<Ipv4Addr>,
    pub effective: usize,
    pub warning: Option<String>,
}

/// Decide how many loopback source IPs to bind and the effective concurrency the
/// port budget allows. Non-loopback → single IP (127.x can't reach a remote), so
/// effective is capped at one tuple's budget with a warning.
pub fn plan_fanout(
    requested: usize,
    loopback: bool,
    per_ip_budget: u32,
    max_ips: usize,
    source_ips_override: Option<usize>,
) -> FanoutPlan {
    // Clamp to at least 1 so `requested.div_ceil(per)` below can never divide by
    // zero (a later task may wire a user-supplied budget into this function).
    let per = (per_ip_budget as usize).max(1);
    let mut warning = None;

    let want_ips = if !loopback {
        1
    } else if let Some(n) = source_ips_override {
        n.max(1)
    } else {
        requested.div_ceil(per).max(1)
    };
    let ips = want_ips.min(max_ips.max(1));
    if loopback && ips < want_ips {
        warning = Some(format!(
            "capped at {ips} source IPs (max-ips); need {want_ips} for {requested} concurrent"
        ));
    }
    if !loopback && requested > per {
        warning = Some(format!(
            "non-loopback target: one source IP allows ~{per} concurrent, but {requested} requested; \
             excess would exhaust ephemeral ports (use distributed workers for more)"
        ));
    }

    let capacity = ips.saturating_mul(per);
    let effective = requested.min(capacity);
    let source_ips = if loopback {
        (0..ips).map(nth_loopback_ip).collect()
    } else {
        Vec::new()
    };
    FanoutPlan {
        source_ips,
        effective,
        warning,
    }
}

/// Result of a best-effort RLIMIT_NOFILE raise.
#[derive(Debug, Clone, Copy)]
pub struct NofileStatus {
    pub soft: u64,
    pub hard: u64,
    pub capped: bool, // true when the desired raise exceeded the hard cap
}

/// Raise the open-file soft limit toward `desired` (clamped to the hard cap). A
/// process may raise soft up to hard but cannot raise the hard cap unprivileged;
/// pass `0` to just read the current limits. Non-Linux / failure → reports what it
/// could read (or zeros) and never panics.
pub fn raise_nofile(desired: u64) -> NofileStatus {
    #[cfg(unix)]
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return NofileStatus {
                soft: 0,
                hard: 0,
                capped: false,
            };
        }
        let hard = lim.rlim_max;
        if desired > 0 {
            let target = desired.min(hard);
            if (target as libc::rlim_t) > lim.rlim_cur {
                let new = libc::rlimit {
                    rlim_cur: target as libc::rlim_t,
                    rlim_max: lim.rlim_max,
                };
                let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &new);
                let _ = libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim);
            }
        }
        NofileStatus {
            soft: lim.rlim_cur,
            hard,
            capped: desired > hard,
        }
    }
    #[cfg(not(unix))]
    {
        NofileStatus {
            soft: 0,
            hard: 0,
            capped: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("example.com"));
        assert!(!is_loopback_host("10.0.0.5"));
    }

    #[test]
    fn nth_loopback_walks_127_8_skipping_edges() {
        assert_eq!(nth_loopback_ip(0), Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(nth_loopback_ip(1), Ipv4Addr::new(127, 0, 0, 2));
        // 0-index 253 -> 127.0.0.254 (last of first low block, skipping .0/.255)
        assert_eq!(nth_loopback_ip(253), Ipv4Addr::new(127, 0, 0, 254));
        // wraps into the next 'c' octet, again starting at .1
        assert_eq!(nth_loopback_ip(254), Ipv4Addr::new(127, 0, 1, 1));
    }

    #[test]
    fn plan_single_ip_when_under_budget() {
        let p = plan_fanout(1000, true, 28232, 512, None);
        assert_eq!(p.source_ips.len(), 1);
        assert_eq!(p.effective, 1000);
        assert!(p.warning.is_none());
    }

    #[test]
    fn plan_fans_out_to_cover_requested_on_loopback() {
        // 1,000,000 / 28232 = ceil 36
        let p = plan_fanout(1_000_000, true, 28232, 512, None);
        assert_eq!(p.source_ips.len(), 36);
        assert_eq!(p.effective, 1_000_000);
    }

    #[test]
    fn plan_caps_effective_on_remote_single_ip_with_warning() {
        let p = plan_fanout(1_000_000, false, 28232, 512, None);
        assert!(p.source_ips.is_empty());
        assert_eq!(p.effective, 28232);
        assert!(p.warning.is_some());
    }

    #[test]
    fn plan_honors_max_ips_cap_and_warns() {
        let p = plan_fanout(100_000_000, true, 28232, 8, None);
        assert_eq!(p.source_ips.len(), 8);
        assert_eq!(p.effective, 8 * 28232);
        assert!(p.warning.is_some());
    }

    #[test]
    fn plan_survives_zero_per_ip_budget() {
        let p = plan_fanout(1000, true, 0, 512, None);
        assert!(!p.source_ips.is_empty());
        // with a clamped per-ip budget of 1, effective is bounded by capacity
        assert!(p.effective >= 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn raise_nofile_reports_soft_at_least_min_of_desired_and_hard() {
        let before = raise_nofile(0); // no-op read of current
                                      // Ask for +1024 over current soft, bounded by hard.
        let want = before.soft + 1024;
        let st = raise_nofile(want);
        assert!(st.hard >= st.soft, "soft must not exceed hard");
        assert!(st.soft >= before.soft.min(st.hard), "soft should not drop");
        assert_eq!(st.capped, want > st.hard);
    }
}
