//! Setting aside cold files that cannot be Parquet.
//!
//! DuckDB refuses a whole `read_parquet` glob when ANY file it matches is
//! unreadable — "Invalid Input Error: File '…' too small to be a Parquet file".
//! Every cold read goes through such a glob, so one bad file poisons its entire
//! table: on a production host a single 0-byte file, left by an export killed
//! mid-write, stopped `error_events` tiering for 18 days and broke the admin
//! Storage page. The export's own cleanup (`remove_files_added_since`) cannot
//! catch that case, because it only runs when the COPY returns an error — not
//! when the process is killed — and it never looks at files an earlier process
//! left behind.
//!
//! So the tier worker sweeps each table's cold directory before reading it. A
//! file whose Parquet framing is broken is renamed to `<name>.parquet.unreadable`
//! in place: out of every `*.parquet` glob, kept beside its data for whoever
//! investigates, and never deleted.
//!
//! # What counts as unreadable
//!
//! Only the framing is checked: the `PAR1` magic at both ends and a footer
//! length that fits inside the file. That is exactly what a truncated write
//! breaks — a writer killed before closing never gets to write the trailing
//! footer — and it costs two tiny reads per file, so the sweep can run every
//! cycle over every file. A file with intact framing but corrupt metadata would
//! pass; that is not a failure mode anything here has produced.
//!
//! # What is never touched
//!
//! A file modified within [`MIN_AGE`]. The sweep runs at the start of a table's
//! turn, before that table's exports, and the tier worker is the only writer,
//! so it cannot race its own output — but a file still being written looks
//! exactly like a truncated one, and "recently modified" is the cheap, certain
//! way to leave one alone should a second writer ever exist.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Appended to a set-aside file's name: `x.parquet` becomes `x.parquet.unreadable`.
pub const QUARANTINE_SUFFIX: &str = "unreadable";

/// Files modified more recently than this are skipped.
pub const MIN_AGE: Duration = Duration::from_secs(10 * 60);

const MAGIC: &[u8; 4] = b"PAR1";
/// Header magic + footer length + footer magic, with an empty footer.
const MIN_LEN: u64 = 12;

/// One file set aside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quarantined {
    pub from: PathBuf,
    pub to: PathBuf,
    pub bytes: u64,
    pub reason: &'static str,
}

/// What one sweep did.
#[derive(Debug, Default)]
pub struct SweepReport {
    pub moved: Vec<Quarantined>,
    /// Files found unreadable that could not be renamed, with the error.
    pub failed: Vec<(PathBuf, String)>,
}

/// Why `path` cannot be a readable Parquet file, or `None` if its framing is
/// intact.
pub fn unreadable_reason(path: &Path) -> std::io::Result<Option<&'static str>> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(Some("empty file"));
    }
    if len < MIN_LEN {
        return Ok(Some("shorter than the Parquet framing"));
    }
    let mut head = [0u8; 4];
    f.read_exact(&mut head)?;
    if &head != MAGIC {
        return Ok(Some("no PAR1 header"));
    }
    let mut tail = [0u8; 8];
    f.seek(SeekFrom::End(-8))?;
    f.read_exact(&mut tail)?;
    if &tail[4..] != MAGIC {
        return Ok(Some("no PAR1 footer (truncated write)"));
    }
    let footer_len = u64::from(u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]));
    if footer_len + MIN_LEN > len {
        return Ok(Some("footer length exceeds the file"));
    }
    Ok(None)
}

/// Rename every unreadable `*.parquet` under `dir` older than `min_age` (as of
/// `now`) to `*.parquet.unreadable`. A missing `dir` is an empty sweep.
///
/// `now` is a parameter so tests can age files without touching their mtimes.
pub fn quarantine_unreadable(dir: &Path, min_age: Duration, now: SystemTime) -> SweepReport {
    let mut report = SweepReport::default();
    let mut files: Vec<PathBuf> = crate::duck::parquet_files_under(dir).into_iter().collect();
    files.sort();
    for path in files {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue; // gone since the listing — nothing to set aside
        };
        // An mtime in the future (clock skew) fails `duration_since`, and is
        // treated as young: when in doubt, leave the file alone.
        let old_enough = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= min_age);
        if !old_enough {
            continue;
        }
        let reason = match unreadable_reason(&path) {
            Ok(Some(reason)) => reason,
            Ok(None) => continue,
            Err(e) => {
                report
                    .failed
                    .push((path, format!("could not inspect: {e}")));
                continue;
            }
        };
        let to = quarantine_target(&path);
        match std::fs::rename(&path, &to) {
            Ok(()) => report.moved.push(Quarantined {
                from: path,
                to,
                bytes: meta.len(),
                reason,
            }),
            Err(e) => report
                .failed
                .push((path, format!("{reason}; rename failed: {e}"))),
        }
    }
    report
}

/// `x.parquet` → `x.parquet.unreadable`, or a numbered variant if that name is
/// already taken, so an earlier set-aside file is never overwritten.
fn quarantine_target(path: &Path) -> PathBuf {
    let base = format!("{}.{QUARANTINE_SUFFIX}", path.display());
    let mut candidate = PathBuf::from(&base);
    let mut n = 1;
    while candidate.exists() {
        candidate = PathBuf::from(format!("{base}.{n}"));
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duck::DuckEngine;
    use uuid::Uuid;

    /// A fresh directory with one valid Parquet file of `rows` rows in a
    /// hive-style subdirectory, written by DuckDB itself.
    fn cold_dir_with_valid_file(rows: i64) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("sauron-quarantine-{}", Uuid::new_v4()));
        let part = root.join("app_id=a/year=2026/month=8");
        std::fs::create_dir_all(&part).unwrap();
        let good = part.join("good.parquet");
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT range AS n FROM range({rows})) TO '{}' (FORMAT PARQUET);",
            good.display()
        ))
        .unwrap();
        (root, good)
    }

    /// Well past `MIN_AGE`, so every file in a test counts as settled.
    fn later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(3600)
    }

    fn glob(root: &Path) -> String {
        format!("{}/**/*.parquet", root.display())
    }

    #[test]
    fn a_valid_file_passes_the_framing_check() {
        let (root, good) = cold_dir_with_valid_file(10);
        assert_eq!(unreadable_reason(&good).unwrap(), None);
        std::fs::remove_dir_all(root).ok();
    }

    /// The production case: a 0-byte file from an export killed mid-write. It
    /// poisons the whole glob until it is set aside, and then the glob reads.
    #[test]
    fn an_empty_file_is_set_aside_and_the_glob_reads_again() {
        let (root, good) = cold_dir_with_valid_file(10);
        let bad = good.with_file_name("killed.parquet");
        std::fs::write(&bad, b"").unwrap();

        let eng = DuckEngine::open().unwrap();
        let err = eng
            .count_parquet_rows(&glob(&root))
            .expect_err("one empty file must poison the whole glob, or this test proves nothing");
        assert!(format!("{err:#}").contains("too small"), "{err:#}");

        let report = quarantine_unreadable(&root, MIN_AGE, later());
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(report.moved.len(), 1);
        assert_eq!(report.moved[0].from, bad);
        assert_eq!(report.moved[0].reason, "empty file");
        assert!(report.moved[0].to.exists(), "kept, not deleted");
        assert!(!bad.exists());
        assert_eq!(
            report.moved[0].to.file_name().unwrap(),
            "killed.parquet.unreadable"
        );

        assert_eq!(eng.count_parquet_rows(&glob(&root)).unwrap(), 10);
        std::fs::remove_dir_all(root).ok();
    }

    /// A write killed after the header but before the footer.
    #[test]
    fn a_truncated_file_is_set_aside() {
        let (root, good) = cold_dir_with_valid_file(10_000);
        let bytes = std::fs::read(&good).unwrap();
        let cut = good.with_file_name("cut.parquet");
        std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();

        assert_eq!(
            unreadable_reason(&cut).unwrap(),
            Some("no PAR1 footer (truncated write)")
        );
        let report = quarantine_unreadable(&root, MIN_AGE, later());
        assert_eq!(report.moved.len(), 1);
        assert_eq!(report.moved[0].from, cut);
        assert!(good.exists(), "the valid file is untouched");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn short_and_headerless_files_are_set_aside() {
        let (root, good) = cold_dir_with_valid_file(10);
        let short = good.with_file_name("short.parquet");
        std::fs::write(&short, b"PAR1").unwrap();
        let junk = good.with_file_name("junk.parquet");
        std::fs::write(&junk, b"this is not parquet at all, PAR1").unwrap();

        assert_eq!(
            unreadable_reason(&short).unwrap(),
            Some("shorter than the Parquet framing")
        );
        assert_eq!(unreadable_reason(&junk).unwrap(), Some("no PAR1 header"));
        assert_eq!(
            quarantine_unreadable(&root, MIN_AGE, later()).moved.len(),
            2
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// A file still being written looks exactly like a truncated one. Anything
    /// modified within `MIN_AGE` is left alone.
    #[test]
    fn a_recently_modified_file_is_left_alone() {
        let (root, good) = cold_dir_with_valid_file(10);
        let fresh = good.with_file_name("in-progress.parquet");
        std::fs::write(&fresh, b"").unwrap();

        let report = quarantine_unreadable(&root, MIN_AGE, SystemTime::now());
        assert!(report.moved.is_empty(), "{:?}", report.moved);
        assert!(fresh.exists());
        std::fs::remove_dir_all(root).ok();
    }

    /// Set-aside files leave every `*.parquet` listing, so a second sweep finds
    /// nothing, and a second bad file with the same name never overwrites the
    /// first one's evidence.
    #[test]
    fn sweeps_are_idempotent_and_never_overwrite_evidence() {
        let (root, good) = cold_dir_with_valid_file(10);
        let bad = good.with_file_name("dup.parquet");
        std::fs::write(&bad, b"").unwrap();
        assert_eq!(
            quarantine_unreadable(&root, MIN_AGE, later()).moved.len(),
            1
        );
        assert!(quarantine_unreadable(&root, MIN_AGE, later())
            .moved
            .is_empty());

        std::fs::write(&bad, b"").unwrap();
        let second = quarantine_unreadable(&root, MIN_AGE, later());
        assert_eq!(second.moved.len(), 1);
        assert_eq!(
            second.moved[0].to.file_name().unwrap(),
            "dup.parquet.unreadable.1"
        );
        assert!(good.with_file_name("dup.parquet.unreadable").exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_cold_dir_that_does_not_exist_yet_is_an_empty_sweep() {
        let missing = std::env::temp_dir().join(format!("sauron-absent-{}", Uuid::new_v4()));
        let report = quarantine_unreadable(&missing, MIN_AGE, later());
        assert!(report.moved.is_empty() && report.failed.is_empty());
    }
}
