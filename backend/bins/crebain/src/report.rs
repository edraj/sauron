//! Human-readable output: the once-a-second live line (stderr) and the final
//! summary table (stdout).

use std::io::Write;

use crate::cli::Expected;
use crate::generator::ItemCounts;
use crate::metrics::{Snapshot, Summary, LATENCY_CAP};

/// Overwrite the current stderr line with live progress.
pub fn live_line(s: &Snapshot) {
    let mut err = std::io::stderr().lock();
    let _ = write!(
        err,
        "\r  {:>5.1}s | users {:>6} | {:>11} req | {:>8.0} req/s (now {:>8.0}) | ok {:>11} | fail {:>9}  ",
        s.elapsed.as_secs_f64(),
        s.users,
        group(s.requests),
        s.rate_cumulative,
        s.rate_interval,
        group(s.accepted),
        group(s.failed),
    );
    let _ = err.flush();
}

/// Erase the live line so the summary starts clean.
pub fn clear_live_line() {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "\r{:100}\r", "");
    let _ = err.flush();
}

pub fn print_summary(s: &Summary, expected: &Expected) {
    let rule = "─".repeat(60);
    let achieved_rps = s.requests as f64 / s.elapsed.as_secs_f64().max(1e-9);
    let target_rps = expected.requests / expected.duration_secs.max(1e-9);
    let pct = |num: u64, den: u64| {
        if den == 0 {
            "—".to_string()
        } else {
            format!("{:.1}%", 100.0 * num as f64 / den as f64)
        }
    };

    println!("\n{rule}");
    println!("  crebain results");
    println!("{rule}");
    println!(
        "  duration   {:.1}s      users   {}      requests   {}",
        s.elapsed.as_secs_f64(),
        group(s.users as u64),
        group(s.requests),
    );

    println!("\n  throughput");
    println!(
        "    requests/sec   achieved {:>10.0}    target {:>10.0}",
        achieved_rps, target_rps
    );
    println!(
        "    requests       achieved {:>10}    target {:>10}",
        group(s.requests),
        group(expected.requests.round() as u64),
    );

    println!("\n  outcomes ({} total requests)", group(s.requests));
    println!("    accepted (2xx)  {:>12}   {}", group(s.accepted), pct(s.accepted, s.requests));
    println!("    rate-limited    {:>12}   {}", group(s.rate_limited), pct(s.rate_limited, s.requests));
    println!("    http errors     {:>12}   {}", group(s.http_errors), pct(s.http_errors, s.requests));
    println!("    transport errs  {:>12}   {}", group(s.transport), pct(s.transport, s.requests));
    if !s.status_counts.is_empty() {
        let codes = s
            .status_counts
            .iter()
            .map(|(c, n)| format!("{c}×{}", group(*n)))
            .collect::<Vec<_>>()
            .join("   ");
        println!("    status codes    {codes}");
    }

    println!("\n  items  accepted / attempted  (by signal type)");
    item_row("errors", s.accepted_items.errors, s.attempted.errors);
    item_row("events", s.accepted_items.events, s.attempted.events);
    item_row("transactions", s.accepted_items.transactions, s.attempted.transactions);
    item_row("identifies", s.accepted_items.identifies, s.attempted.identifies);
    item_row("breadcrumbs", s.accepted_items.breadcrumbs, s.attempted.breadcrumbs);
    item_row("total", total(&s.accepted_items), total(&s.attempted));

    println!("\n  latency  (per request, includes failures)");
    println!(
        "    p50 {:>10}    p90 {:>10}    p99 {:>10}    max {:>10}",
        fmt_us(s.p50_us),
        fmt_us(s.p90_us),
        fmt_us(s.p99_us),
        fmt_us(s.max_us),
    );
    if s.latency_truncated {
        println!(
            "    (percentiles from the first {} of {} requests)",
            group(LATENCY_CAP as u64),
            group(s.latency_samples),
        );
    }
    println!("{rule}");
}

fn item_row(label: &str, accepted: u64, attempted: u64) {
    println!("    {label:<14} {:>12} / {:<12}", group(accepted), group(attempted));
}

pub(crate) fn total(c: &ItemCounts) -> u64 {
    c.errors + c.events + c.identifies + c.transactions + c.breadcrumbs
}

/// Group a number: 1234567 → "1,234,567".
pub(crate) fn group(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Format a microsecond latency compactly.
pub(crate) fn fmt_us(us: u64) -> String {
    if us < 1000 {
        format!("{us}us")
    } else {
        format!("{:.2}ms", us as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_thousands() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(1234567), "1,234,567");
    }

    #[test]
    fn formats_latency() {
        assert_eq!(fmt_us(500), "500us");
        assert_eq!(fmt_us(1500), "1.50ms");
    }
}
