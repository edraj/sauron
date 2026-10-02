//! Free space on the filesystem that holds cold storage.
//!
//! On a typical install that is the same disk as Postgres, and a full disk
//! does not degrade Postgres gracefully: it aborts the moment a WAL segment
//! cannot be created, which takes every writer down at once, including the
//! cleanup that would have freed space. The tier worker is the component that
//! moves bulk data off that disk, so it is the one that watches it: each cycle
//! it reads [`usage`] and, through [`classify`], decides whether to warn or to
//! tier more aggressively.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// One `statvfs` reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskUsage {
    /// Bytes available to an unprivileged writer (`f_bavail`), which is what
    /// Postgres, running as its own user, can actually use.
    pub free_bytes: u64,
    pub total_bytes: u64,
}

impl DiskUsage {
    /// Free space as a whole percentage of the filesystem, rounded down.
    /// `0` for a filesystem that reports no size.
    pub fn free_pct(&self) -> u8 {
        if self.total_bytes == 0 {
            return 0;
        }
        ((self.free_bytes as u128 * 100) / self.total_bytes as u128).min(100) as u8
    }
}

/// `statvfs` on `path`.
pub fn usage(path: &Path) -> std::io::Result<DiskUsage> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the
    // call, and `st` points to writable memory of the right type. `statvfs`
    // fully initializes `st` when it returns 0, and only then is it read.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `statvfs` returned 0, so `st` is initialized.
    let st = unsafe { st.assume_init() };
    // These fields are `c_ulong`/`fsblkcnt_t`, which are already `u64` on
    // 64-bit Linux but narrower elsewhere, so the casts are not redundant
    // everywhere this may build.
    #[allow(clippy::unnecessary_cast)]
    let (frsize, bavail, blocks) = (st.f_frsize as u64, st.f_bavail as u64, st.f_blocks as u64);
    Ok(DiskUsage {
        free_bytes: bavail.saturating_mul(frsize),
        total_bytes: blocks.saturating_mul(frsize),
    })
}

/// How the tier worker should treat a disk reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    Normal,
    /// Below the warning threshold: tier as usual, but say so every cycle.
    Low,
    /// Below the emergency threshold: tier on a shorter rotation age and skip
    /// the drop lag, so verified partitions leave Postgres this cycle.
    Critical,
}

/// Classify `free_pct` against the two thresholds. An emergency threshold of
/// `0` disables emergency mode; a warning threshold below the emergency one is
/// treated as equal to it, so a critical disk is never reported as merely low.
pub fn classify(free_pct: u8, warn_pct: u8, emergency_pct: u8) -> Pressure {
    if emergency_pct > 0 && free_pct < emergency_pct {
        Pressure::Critical
    } else if free_pct < warn_pct.max(emergency_pct) {
        Pressure::Low
    } else {
        Pressure::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_real_filesystem() {
        let u = usage(&std::env::temp_dir()).unwrap();
        assert!(u.total_bytes > 0);
        assert!(u.free_bytes <= u.total_bytes);
        assert!(u.free_pct() <= 100);
    }

    #[test]
    fn a_missing_path_is_an_error_not_a_panic() {
        assert!(usage(Path::new("/nonexistent/sauron-tier-disk-check")).is_err());
    }

    #[test]
    fn percent_rounds_down_and_handles_an_empty_filesystem() {
        let u = |free, total| DiskUsage {
            free_bytes: free,
            total_bytes: total,
        };
        assert_eq!(u(0, 0).free_pct(), 0);
        assert_eq!(u(99, 1000).free_pct(), 9);
        assert_eq!(u(1000, 1000).free_pct(), 100);
        // A 152 GB disk with 0 bytes left, as on the host that prompted this.
        assert_eq!(u(0, 152 << 30).free_pct(), 0);
        assert_eq!(u(u64::MAX, u64::MAX).free_pct(), 100);
    }

    #[test]
    fn classifies_against_both_thresholds() {
        assert_eq!(classify(50, 15, 10), Pressure::Normal);
        assert_eq!(classify(15, 15, 10), Pressure::Normal);
        assert_eq!(classify(14, 15, 10), Pressure::Low);
        assert_eq!(classify(10, 15, 10), Pressure::Low);
        assert_eq!(classify(9, 15, 10), Pressure::Critical);
        assert_eq!(classify(0, 15, 10), Pressure::Critical);
    }

    #[test]
    fn zero_disables_emergency_mode_but_not_the_warning() {
        assert_eq!(classify(0, 15, 0), Pressure::Low);
        assert_eq!(classify(20, 15, 0), Pressure::Normal);
    }

    #[test]
    fn a_warning_threshold_below_the_emergency_one_is_raised_to_it() {
        // Misconfigured: warn at 5%, emergency at 10%. 12% free is neither.
        assert_eq!(classify(12, 5, 10), Pressure::Normal);
        assert_eq!(classify(9, 5, 10), Pressure::Critical);
    }
}
