//! Best-effort Linux `/proc` sampling of a target process: CPU time (jiffies)
//! and resident memory (RSS). Everything is `Option`-returning — a missing or
//! malformed `/proc` (including on non-Linux, where it does not exist) yields
//! `None` so the caller simply omits the resource charts, never failing a run.

/// utime + stime (fields 14 and 15) from `/proc/<pid>/stat`, in clock ticks.
/// The `comm` field (field 2) may contain spaces and parentheses, so we split
/// on the LAST ')': everything after it starts at field 3 (`state`), which puts
/// utime at token index 11 and stime at index 12.
pub fn parse_pid_stat_cpu_jiffies(contents: &str) -> Option<u64> {
    let rparen = contents.rfind(')')?;
    let rest = contents.get(rparen + 1..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Sum of every field on the aggregate `cpu ` line of `/proc/stat` (total system
/// jiffies across all cores). The trailing space avoids matching `cpu0`, `cpu1`.
pub fn parse_stat_total_jiffies(contents: &str) -> Option<u64> {
    let line = contents.lines().find(|l| l.starts_with("cpu "))?;
    let mut total: u64 = 0;
    for tok in line.split_whitespace().skip(1) {
        total = total.checked_add(tok.parse::<u64>().ok()?)?;
    }
    Some(total)
}

/// `VmRSS` from `/proc/<pid>/status` (reported in kB) converted to bytes.
pub fn parse_status_vmrss_bytes(contents: &str) -> Option<u64> {
    let line = contents.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// Cores used over an interval from CPU-jiffie deltas. USER_HZ cancels because
/// numerator and denominator share the same jiffie unit, so no `libc` is needed.
pub fn cores_from_deltas(dproc: u64, dtotal: u64, ncpus: f64) -> f64 {
    if dtotal == 0 {
        0.0
    } else {
        dproc as f64 / dtotal as f64 * ncpus
    }
}

/// One raw reading of the target process and the system CPU counter.
pub struct RawSample {
    pub proc_jiffies: u64,
    pub total_jiffies: u64,
    pub rss_bytes: u64,
}

/// Reads `/proc` for a fixed pid. Holds the (fixed) core count so callers can
/// convert jiffie deltas to cores.
pub struct Sampler {
    pid: u32,
    ncpus: f64,
}

impl Sampler {
    pub fn new(pid: u32) -> Self {
        let ncpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        Sampler { pid, ncpus }
    }

    pub fn ncpus(&self) -> f64 {
        self.ncpus
    }

    /// Read `/proc/<pid>/stat`, `/proc/stat`, and `/proc/<pid>/status`. `None` if
    /// any read or parse fails (process gone, non-Linux, unexpected format).
    pub fn sample(&self) -> Option<RawSample> {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.pid)).ok()?;
        let total = std::fs::read_to_string("/proc/stat").ok()?;
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.pid)).ok()?;
        Some(RawSample {
            proc_jiffies: parse_pid_stat_cpu_jiffies(&stat)?,
            total_jiffies: parse_stat_total_jiffies(&total)?,
            rss_bytes: parse_status_vmrss_bytes(&status)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // comm field intentionally contains spaces and a ')': the parser must key
    // off the LAST ')'. Fields after it: state ppid pgrp session tty tpgid flags
    // minflt cminflt majflt cmajflt utime stime ... → utime is the 12th token.
    const PID_STAT: &str =
        "1234 (in gest (x)) S 1 1234 1234 0 -1 4194560 100 0 0 0 4200 1800 0 0 20 0 8 0 999";

    #[test]
    fn parses_utime_plus_stime() {
        assert_eq!(parse_pid_stat_cpu_jiffies(PID_STAT), Some(4200 + 1800));
    }

    #[test]
    fn parses_total_jiffies_from_cpu_line() {
        let stat = "cpu  100 20 30 400 5 0 6 0 0 0\ncpu0 50 10 15 200 2 0 3 0 0 0\n";
        assert_eq!(parse_stat_total_jiffies(stat), Some(100 + 20 + 30 + 400 + 5 + 0 + 6));
    }

    #[test]
    fn parses_vmrss_kb_to_bytes() {
        let status = "Name:\tingest\nState:\tS\nVmRSS:\t  20480 kB\nThreads:\t8\n";
        assert_eq!(parse_status_vmrss_bytes(status), Some(20480 * 1024));
    }

    #[test]
    fn malformed_inputs_return_none() {
        assert_eq!(parse_pid_stat_cpu_jiffies("garbage no paren"), None);
        assert_eq!(parse_stat_total_jiffies("no cpu line here"), None);
        assert_eq!(parse_status_vmrss_bytes("no vmrss"), None);
    }

    #[test]
    fn cores_formula_is_userhz_independent() {
        // proc used 400 of 4000 total jiffies over the interval on an 8-core box
        // → 0.1 of all-core time × 8 = 0.8 cores.
        assert!((cores_from_deltas(400, 4000, 8.0) - 0.8).abs() < 1e-9);
        // zero elapsed total → 0, never a divide-by-zero.
        assert_eq!(cores_from_deltas(5, 0, 8.0), 0.0);
    }
}
