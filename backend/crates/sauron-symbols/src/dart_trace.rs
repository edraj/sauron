//! Parse a verbatim Dart (Flutter AOT) obfuscated stack trace.
//!
//! Release Dart stack traces are PC offsets, not symbols. The SDK ships the raw
//! trace string; we pull out the `build_id`, the `isolate_dso_base`, and each
//! frame's `abs`/`virt` addresses so the DWARF resolver can look them up in the
//! matching `--split-debug-info` ELF.
//!
//! Shape (abridged):
//! ```text
//! *** *** ***
//! build_id: 'a1b2c3d4'
//! isolate_dso_base: 7f0000000000
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

pub fn parse(raw: &str) -> DartTrace {
    let mut build_id = None;
    let mut dso_base = None;
    let mut frames = Vec::new();

    for line in raw.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("build_id:") {
            build_id = Some(rest.trim().trim_matches(['\'', '"']).to_string());
        } else if let Some(rest) = line.strip_prefix("isolate_dso_base:") {
            dso_base = parse_hex(rest);
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

    const TRACE: &str = "\
*** *** ***\n\
build_id: 'a1b2c3d4'\n\
isolate_dso_base: 7f0000000000\n\
    #00 abs 00007f0000001560 virt 0000000000001560 _kDartIsolateSnapshotInstructions+0x1560\n\
    #01 abs 00007f0000001890 virt 0000000000001890 _kDartIsolateSnapshotInstructions+0x1890\n";

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
}
