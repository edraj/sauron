//! Parse a verbatim Dart (Flutter AOT) obfuscated stack trace.
//!
//! Release Dart stack traces are PC offsets, not symbols. The SDK ships the raw
//! trace string; we pull out the `build_id`, the `isolate_dso_base`, and each
//! frame's `abs`/`virt` addresses so the DWARF resolver can look them up in the
//! matching `--split-debug-info` ELF.
//!
//! Shape (abridged, and matching what a real device emits — note that BOTH
//! dso-base keys share one line):
//! ```text
//! *** *** ***
//! build_id: 'a1b2c3d4'
//! isolate_dso_base: 7f0000000000, vm_dso_base: 7f0000000000
//!     #00 abs 00007f0000001560 virt 0000000000001560 _kDartIsolateSnapshotInstructions+0x1560
//! ```

#[derive(Debug, Clone)]
pub struct DartTrace {
    pub build_id: Option<String>,
    pub dso_base: Option<u64>,
    pub frames: Vec<DartFrameRef>,
}

#[derive(Debug, Clone)]
pub struct DartFrameRef {
    pub index: u32,
    pub abs: Option<u64>,
    pub virt: Option<u64>,
}

impl DartFrameRef {
    /// The address to look up in the debug-info ELF: prefer `virt` (already the
    /// DSO-relative virtual address), else `abs - dso_base`.
    pub fn lookup_addr(&self, dso_base: Option<u64>) -> Option<u64> {
        if let Some(v) = self.virt {
            return Some(v);
        }
        match (self.abs, dso_base) {
            (Some(a), Some(base)) => a.checked_sub(base),
            _ => None,
        }
    }
}

fn parse_hex(s: &str) -> Option<u64> {
    let s = s.trim().trim_start_matches("0x");
    u64::from_str_radix(s, 16).ok()
}

/// The leading comma/whitespace-delimited token of `s`.
///
/// Real Dart AOT puts BOTH dso-base keys on one line:
///
/// ```text
/// isolate_dso_base: 7b9c2b7000, vm_dso_base: 7b9c2b7000
/// ```
///
/// so the text after `isolate_dso_base:` is not a bare address. Measured on a
/// real device 2026-08-08: passing the whole remainder to `parse_hex` made it
/// return `None`, so `dso_base` was `None` for all 14 captured traces and the
/// `abs - dso_base` fallback in [`DartFrameRef::lookup_addr`] could not resolve
/// any of them. It stayed latent only because real traces also carry `virt`,
/// which `lookup_addr` prefers — the Dart VM appends ` virt %016lx` as a
/// separate conditional fragment, so the fallback is a live path, not dead code.
///
/// Applied here rather than inside `parse_hex` on purpose: frame `abs`/`virt`
/// values arrive already split on whitespace, and a malformed one should keep
/// failing loudly instead of silently parsing a prefix.
fn first_token(s: &str) -> &str {
    s.trim()
        .split(|c: char| c == ',' || c.is_whitespace())
        .next()
        .unwrap_or("")
}

pub fn parse(raw: &str) -> DartTrace {
    let mut build_id = None;
    let mut dso_base = None;
    let mut frames = Vec::new();

    for line in raw.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("build_id:") {
            build_id = Some(rest.trim().trim_matches(['\'', '"']).to_string());
        } else if let Some(rest) = line.strip_prefix("isolate_dso_base:") {
            // `first_token` because real AOT output continues the line with
            // `, vm_dso_base: ...`. Also recovers already-stored polluted values.
            dso_base = parse_hex(first_token(rest));
        } else if line.starts_with('#') {
            frames.push(parse_frame(line));
        }
    }

    DartTrace {
        build_id,
        dso_base,
        frames,
    }
}

fn parse_frame(line: &str) -> DartFrameRef {
    let mut index = 0;
    let mut abs = None;
    let mut virt = None;

    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if let Some(num) = tok.strip_prefix('#') {
            index = num.parse().unwrap_or(0);
        } else if tok == "abs" {
            abs = tokens.get(i + 1).and_then(|s| parse_hex(s));
            i += 1;
        } else if tok == "virt" {
            virt = tokens.get(i + 1).and_then(|s| parse_hex(s));
            i += 1;
        }
        i += 1;
    }

    DartFrameRef { index, abs, virt }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Carries `, vm_dso_base: ...` because real Dart AOT does. The previous
    // version of this fixture stopped after the first address, and the SDK's own
    // fixture (sdks/flutter/test/dart_symbolication_test.dart) was unrealistic in
    // exactly the same way — which is why a parse bug that broke EVERY real trace
    // passed both suites on both sides of the wire.
    const TRACE: &str = "\
*** *** ***\n\
build_id: 'a1b2c3d4'\n\
isolate_dso_base: 7f0000000000, vm_dso_base: 7f0000000000\n\
    #00 abs 00007f0000001560 virt 0000000000001560 _kDartIsolateSnapshotInstructions+0x1560\n\
    #01 abs 00007f0000001890 virt 0000000000001890 _kDartIsolateSnapshotInstructions+0x1890\n";

    /// Verbatim header captured from a real device (Redmi `camellia`,
    /// Android 13, Flutter 3.44.8) on 2026-08-08.
    const REAL: &str = "\
*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***\n\
os: android arch: arm64 comp: yes sim: no\n\
build_id: 'b7188509e5f19c541ab806422af8410e'\n\
isolate_dso_base: 7b9c2b7000, vm_dso_base: 7b9c2b7000\n\
isolate_instructions: 7b9c38da80, vm_instructions: 7b9c377000\n\
    #00 abs 0000007b9c4bc9b7 virt 00000000002059b7 _kDartIsolateSnapshotInstructions+0x12ef37\n";

    /// The same header with ` virt …` stripped from the frame. The Dart VM emits
    /// ` virt %016lx` as a separate conditional fragment, so this is a shape real
    /// output can take — and it is the only shape that exercises the
    /// `abs - dso_base` fallback.
    const REAL_NO_VIRT: &str = "\
*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***\n\
os: android arch: arm64 comp: yes sim: no\n\
build_id: 'b7188509e5f19c541ab806422af8410e'\n\
isolate_dso_base: 7b9c2b7000, vm_dso_base: 7b9c2b7000\n\
isolate_instructions: 7b9c38da80, vm_instructions: 7b9c377000\n\
    #00 abs 0000007b9c4bc9b7 _kDartIsolateSnapshotInstructions+0x12ef37\n";

    #[test]
    fn parses_header_and_frames() {
        let t = parse(TRACE);
        assert_eq!(t.build_id.as_deref(), Some("a1b2c3d4"));
        assert_eq!(t.dso_base, Some(0x7f0000000000));
        assert_eq!(t.frames.len(), 2);
        assert_eq!(t.frames[0].index, 0);
        assert_eq!(t.frames[0].abs, Some(0x7f0000001560));
        assert_eq!(t.frames[0].virt, Some(0x1560));
        assert_eq!(t.frames[0].lookup_addr(t.dso_base), Some(0x1560));
        assert_eq!(t.frames[1].virt, Some(0x1890));
    }

    #[test]
    fn falls_back_to_abs_minus_base() {
        let f = DartFrameRef {
            index: 0,
            abs: Some(0x7f0000001560),
            virt: None,
        };
        assert_eq!(f.lookup_addr(Some(0x7f0000000000)), Some(0x1560));
        assert_eq!(f.lookup_addr(None), None);
    }

    #[test]
    fn tolerates_missing_header() {
        let t = parse("no frames here\njust text\n");
        assert!(t.build_id.is_none());
        assert!(t.frames.is_empty());
    }

    /// Regression: before `first_token`, the whole remainder of the line
    /// ("7b9c2b7000, vm_dso_base: 7b9c2b7000") went to `parse_hex`, which
    /// returned `None`. Measured against all 14 traces captured from the device.
    #[test]
    fn real_device_header_yields_dso_base() {
        let t = parse(REAL);
        assert_eq!(
            t.build_id.as_deref(),
            Some("b7188509e5f19c541ab806422af8410e")
        );
        assert_eq!(t.dso_base, Some(0x7b9c2b7000));
    }

    /// The consequence that made the parse bug matter: with `virt` absent,
    /// resolution depends entirely on `abs - dso_base`, so a `None` base meant a
    /// real trace could not be resolved at all.
    #[test]
    fn real_device_virtless_frame_resolves_via_abs_minus_base() {
        let t = parse(REAL_NO_VIRT);
        assert_eq!(t.dso_base, Some(0x7b9c2b7000));
        assert_eq!(t.frames.len(), 1);
        assert_eq!(t.frames[0].virt, None, "fixture must not carry virt");
        assert_eq!(t.frames[0].abs, Some(0x7b9c4bc9b7));
        // 0x7b9c4bc9b7 - 0x7b9c2b7000 = 0x2059b7, the address that resolves to
        // probes.dart:24 in the verification run.
        assert_eq!(t.frames[0].lookup_addr(t.dso_base), Some(0x2059b7));
    }

    #[test]
    fn first_token_splits_on_comma_and_whitespace() {
        assert_eq!(
            first_token(" 7b9c2b7000, vm_dso_base: 7b9c2b7000"),
            "7b9c2b7000"
        );
        assert_eq!(first_token(" 7f0000000000"), "7f0000000000");
        assert_eq!(first_token("0"), "0");
        assert_eq!(first_token(""), "");
        // A leading comma yields an empty token rather than skipping to the next
        // value: parse_hex then rejects it, which is the honest outcome for a
        // header we do not recognise.
        assert_eq!(first_token(", 7b9c2b7000"), "");
    }
}
