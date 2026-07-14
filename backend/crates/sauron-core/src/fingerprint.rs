//! Error → issue grouping.
//!
//! Two occurrences of "the same bug" must collapse into one issue even as line
//! numbers, minified chunk hashes, and embedded ids drift release to release.
//! The algorithm:
//!
//! 1. Honor an explicit client fingerprint if present.
//! 2. Otherwise pick the top K in-app frames (crashing frame first) and reduce
//!    each to a stable `module::function` (or filename) signature — **dropping
//!    line/column numbers** and masking digits/hex/uuids.
//! 3. Hash `exception_type \n sig1 \n sig2 …` with SHA-256.
//! 4. With no frames, fall back to `type + normalized message` so
//!    "User 123 not found" and "User 456 not found" group together.

use sha2::{Digest, Sha256};

use crate::envelope::{ExceptionInfo, Frame};

/// Number of frames from the crash site used to build the signature.
const FRAME_DEPTH: usize = 5;

/// Compute the grouping fingerprint (lowercase hex SHA-256).
pub fn fingerprint(
    exception: Option<&ExceptionInfo>,
    message: Option<&str>,
    client: Option<&[String]>,
) -> String {
    if let Some(fp) = client {
        if !fp.is_empty() {
            return hash(&fp.join("\n"));
        }
    }

    match exception {
        Some(exc) => {
            let ty = normalize_type(&exc.ty);
            let frames = pick_frames(&exc.stacktrace);
            if frames.is_empty() {
                let msg = normalize_message(exc.value.as_deref().or(message).unwrap_or(""));
                hash(&format!("{ty}\n{msg}"))
            } else {
                let mut parts = Vec::with_capacity(frames.len() + 1);
                parts.push(ty);
                parts.extend(frames.into_iter().map(frame_signature));
                hash(&parts.join("\n"))
            }
        }
        None => {
            let msg = normalize_message(message.unwrap_or("unknown"));
            hash(&format!("message\n{msg}"))
        }
    }
}

/// Prefer in-app frames; take up to [`FRAME_DEPTH`] closest to the crash.
/// Frames arrive crashing-last, so we walk from the end.
fn pick_frames(frames: &[Frame]) -> Vec<&Frame> {
    let in_app: Vec<&Frame> = frames.iter().filter(|f| f.in_app == Some(true)).collect();
    let pool: Vec<&Frame> = if in_app.is_empty() {
        frames.iter().collect()
    } else {
        in_app
    };
    pool.into_iter().rev().take(FRAME_DEPTH).collect()
}

/// Reduce a frame to a stable signature, ignoring line/column numbers.
fn frame_signature(f: &Frame) -> String {
    match (f.module.as_deref(), f.function.as_deref()) {
        (Some(m), Some(func)) if !m.is_empty() && !func.is_empty() => {
            format!("{}::{}", normalize_symbol(m), normalize_symbol(func))
        }
        (_, Some(func)) if !func.is_empty() => normalize_symbol(func),
        _ => match f.filename.as_deref().or(f.abs_path.as_deref()) {
            Some(file) => normalize_filename(file),
            None => "?".to_string(),
        },
    }
}

fn normalize_type(ty: &str) -> String {
    // Strip trailing addresses/numbers e.g. "SIGSEGV(0x1a2b)" → "SIGSEGV"
    normalize_symbol(ty.split('(').next().unwrap_or(ty).trim())
}

/// Lowercase-preserving symbol cleanup: mask numeric runs and hex blobs so that
/// generated/anonymous suffixes don't split a group.
fn normalize_symbol(s: &str) -> String {
    mask_volatile(s.trim())
}

/// Reduce a filename/URL to a stable basename: drop query string, keep the last
/// path segment, collapse content-hash chunks (`app.4f3a2b91.js` → `app.js`),
/// and mask numbers.
fn normalize_filename(file: &str) -> String {
    let no_query = file.split(['?', '#']).next().unwrap_or(file);
    let base = no_query
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(no_query)
        .trim();

    // Collapse a content-hash segment: chunk.<hash>.js → chunk.js
    let collapsed = collapse_hash_segment(base);
    mask_volatile(&collapsed)
}

/// Remove a dotted hex/hash segment that looks like a build hash.
fn collapse_hash_segment(name: &str) -> String {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() >= 3 {
        let kept: Vec<&str> = parts
            .iter()
            .copied()
            .filter(|seg| !looks_like_hash(seg))
            .collect();
        if kept.len() >= 2 {
            return kept.join(".");
        }
    }
    name.to_string()
}

fn looks_like_hash(seg: &str) -> bool {
    seg.len() >= 6
        && seg.len() <= 40
        && seg.chars().all(|c| c.is_ascii_hexdigit())
        && seg.chars().any(|c| c.is_ascii_digit())
}

/// Normalize a free-text message for the no-frame fallback: lowercase, mask
/// numbers, hex addresses, uuids, and quoted literals.
fn normalize_message(msg: &str) -> String {
    let masked = mask_volatile(&msg.to_lowercase());
    // Collapse repeated whitespace.
    masked.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Replace volatile tokens (digit runs, `0x…` addresses, uuids) with a
/// placeholder so semantically-identical strings hash the same.
fn mask_volatile(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for token in s.split_inclusive(|c: char| c.is_whitespace()) {
        // Split off trailing whitespace to re-append after masking.
        let trimmed_len = token.trim_end_matches(char::is_whitespace).len();
        let (word, ws) = token.split_at(trimmed_len);
        out.push_str(&mask_word(word));
        out.push_str(ws);
    }
    out
}

fn mask_word(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }
    if is_uuid(word) {
        return "{uuid}".to_string();
    }
    if word.starts_with("0x") && word.len() > 2 && word[2..].chars().all(|c| c.is_ascii_hexdigit())
    {
        return "{addr}".to_string();
    }
    // Mask any maximal run of digits with a single placeholder.
    let mut out = String::with_capacity(word.len());
    let mut in_digits = false;
    for c in word.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push_str("{n}");
                in_digits = true;
            }
        } else {
            in_digits = false;
            out.push(c);
        }
    }
    out
}

fn is_uuid(word: &str) -> bool {
    let w = word.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == '.');
    let bytes = w.as_bytes();
    w.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && w.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn hash(canonical: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Frame;

    fn frame(func: &str, file: &str, line: u32) -> Frame {
        Frame {
            function: Some(func.into()),
            module: None,
            filename: Some(file.into()),
            abs_path: None,
            lineno: Some(line),
            colno: Some(1),
            in_app: Some(true),
        }
    }

    fn exc(ty: &str, frames: Vec<Frame>) -> ExceptionInfo {
        ExceptionInfo {
            ty: ty.into(),
            value: None,
            mechanism: None,
            stacktrace: frames,
        }
    }

    #[test]
    fn line_numbers_do_not_change_the_group() {
        let a = exc("TypeError", vec![frame("loadUser", "app.js", 42)]);
        let b = exc("TypeError", vec![frame("loadUser", "app.js", 87)]);
        assert_eq!(
            fingerprint(Some(&a), None, None),
            fingerprint(Some(&b), None, None),
            "same call site on a different line must group together"
        );
    }

    #[test]
    fn content_hash_chunks_collapse() {
        let mut f1 = frame("", "main.a1b2c3.js", 10);
        f1.function = None;
        let mut f2 = frame("", "main.9f8e7d.js", 55);
        f2.function = None;
        let a = exc("Error", vec![f1]);
        let b = exc("Error", vec![f2]);
        assert_eq!(
            fingerprint(Some(&a), None, None),
            fingerprint(Some(&b), None, None),
            "hashed chunk names must normalize to the same signature"
        );
    }

    #[test]
    fn different_exceptions_differ() {
        let a = exc("TypeError", vec![frame("loadUser", "app.js", 42)]);
        let b = exc("RangeError", vec![frame("loadUser", "app.js", 42)]);
        assert_ne!(
            fingerprint(Some(&a), None, None),
            fingerprint(Some(&b), None, None)
        );
    }

    #[test]
    fn numeric_messages_group_when_no_frames() {
        let a = exc("DBError", vec![]);
        let b = exc("DBError", vec![]);
        assert_eq!(
            fingerprint(Some(&a), Some("User 123 not found"), None),
            fingerprint(Some(&b), Some("User 456 not found"), None),
            "messages differing only in numbers must group"
        );
    }

    #[test]
    fn uuid_messages_group_when_no_frames() {
        assert_eq!(
            fingerprint(
                None,
                Some("order aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee failed"),
                None
            ),
            fingerprint(
                None,
                Some("order 11111111-2222-3333-4444-555555555555 failed"),
                None
            ),
        );
    }

    #[test]
    fn in_app_frames_are_preferred_over_library_frames() {
        // Two errors that differ only in library (non-in_app) frames but share
        // the in-app crash frame must group together.
        let lib1 = Frame {
            function: Some("vendor_a".into()),
            module: None,
            filename: Some("vendor_a.js".into()),
            abs_path: None,
            lineno: Some(1),
            colno: None,
            in_app: Some(false),
        };
        let lib2 = Frame {
            function: Some("vendor_b".into()),
            module: None,
            filename: Some("vendor_b.js".into()),
            abs_path: None,
            lineno: Some(2),
            colno: None,
            in_app: Some(false),
        };
        let app_frame = frame("handler", "app.js", 20);
        let a = exc("Error", vec![lib1, app_frame.clone()]);
        let b = exc("Error", vec![lib2, app_frame]);
        assert_eq!(
            fingerprint(Some(&a), None, None),
            fingerprint(Some(&b), None, None),
            "differing library frames must not split a group with the same in-app frame"
        );
    }

    #[test]
    fn falls_back_to_all_frames_when_none_in_app() {
        let mut f = frame("only", "x.js", 3);
        f.in_app = Some(false);
        let a = exc("Error", vec![f]);
        // Should still produce a stable, non-empty fingerprint.
        assert_eq!(fingerprint(Some(&a), None, None).len(), 64);
    }

    #[test]
    fn client_fingerprint_is_honored() {
        let a = exc("TypeError", vec![frame("a", "a.js", 1)]);
        let b = exc("RangeError", vec![frame("z", "z.js", 9)]);
        let fp = vec!["custom-group".to_string()];
        assert_eq!(
            fingerprint(Some(&a), None, Some(&fp)),
            fingerprint(Some(&b), None, Some(&fp)),
            "explicit client fingerprint overrides everything"
        );
    }
}
