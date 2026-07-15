//! Source Map v3 parsing + frame resolution.
//!
//! A minified stack frame gives `(line, column)` into the generated bundle;
//! this resolves it back to the original source position + function name and,
//! on request, the surrounding source lines.
//!
//! Coordinate conventions: stack `lineno`/`colno` are **1-based**; Source Map v3
//! stores lines/columns **0-based**. Conversions happen at the boundary here.

use std::iter::Peekable;
use std::str::Chars;

use crate::content::SymbolError;

/// A resolved original-source location for a single frame.
#[derive(Debug, Clone)]
pub struct ResolvedLoc {
    pub source: String,
    /// 1-based original line.
    pub line: u32,
    /// 1-based original column.
    pub column: u32,
    pub name: Option<String>,
    pub source_index: usize,
}

/// A slice of original source around a resolved line.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceContext {
    pub pre: Vec<String>,
    pub line: String,
    pub post: Vec<String>,
    /// 1-based line number of the first `pre` line (or of `line` when `pre` is empty).
    pub start_line: u32,
}

#[derive(Debug, Clone)]
struct SrcRef {
    source: u32,
    line: u32,
    col: u32,
    name: Option<u32>,
}

#[derive(Debug, Clone)]
struct Segment {
    /// 0-based generated column.
    gen_col: u32,
    src: Option<SrcRef>,
}

/// A parsed, lookup-ready Source Map v3.
#[derive(Debug, Clone)]
pub struct ParsedSourceMap {
    sources: Vec<String>,
    names: Vec<String>,
    sources_content: Vec<Option<String>>,
    /// Segments per generated line (each inner vec is sorted by `gen_col`).
    lines: Vec<Vec<Segment>>,
    approx_bytes: usize,
}

fn b64(c: char) -> Option<i64> {
    match c {
        'A'..='Z' => Some(c as i64 - 'A' as i64),
        'a'..='z' => Some(c as i64 - 'a' as i64 + 26),
        '0'..='9' => Some(c as i64 - '0' as i64 + 52),
        '+' => Some(62),
        '/' => Some(63),
        _ => None,
    }
}

/// Decode one Base64 VLQ value, consuming its characters.
fn decode_vlq(chars: &mut Peekable<Chars>) -> Option<i64> {
    let mut result: i64 = 0;
    let mut shift: u32 = 0;
    loop {
        let c = chars.next()?;
        let digit = b64(c)?;
        let cont = digit & 32; // continuation bit
        result += (digit & 31) << shift;
        shift += 5;
        if cont == 0 {
            break;
        }
    }
    let negate = result & 1 == 1;
    let value = result >> 1;
    Some(if negate { -value } else { value })
}

impl ParsedSourceMap {
    /// An empty map that resolves nothing — used as a cache placeholder when a
    /// blob can't be fetched or parsed, so a transient failure isn't retried on
    /// every frame within one request.
    pub fn empty() -> Self {
        ParsedSourceMap {
            sources: Vec::new(),
            names: Vec::new(),
            sources_content: Vec::new(),
            lines: Vec::new(),
            approx_bytes: 0,
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<ParsedSourceMap, SymbolError> {
        let v: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| SymbolError::Corrupt(e.to_string()))?;

        if v.get("version").and_then(|x| x.as_i64()) != Some(3) {
            return Err(SymbolError::Corrupt("unsupported source map version".into()));
        }

        let source_root = v
            .get("sourceRoot")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let sources: Vec<String> = v
            .get("sources")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|s| {
                        let s = s.as_str().unwrap_or("").to_string();
                        if source_root.is_empty() || s.contains("://") {
                            s
                        } else {
                            format!("{}{}", source_root.trim_end_matches('/'), prefix_slash(&s))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let names: Vec<String> = v
            .get("names")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().map(|s| s.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default();
        let sources_content: Vec<Option<String>> = v
            .get("sourcesContent")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().map(|s| s.as_str().map(|t| t.to_string())).collect())
            .unwrap_or_default();

        let mappings = v
            .get("mappings")
            .and_then(|x| x.as_str())
            .ok_or_else(|| SymbolError::Corrupt("missing mappings".into()))?;

        let mut lines: Vec<Vec<Segment>> = Vec::new();
        // These deltas persist across the whole file; only gen_col resets per line.
        let (mut src_idx, mut src_line, mut src_col, mut name_idx) = (0i64, 0i64, 0i64, 0i64);

        for line_str in mappings.split(';') {
            let mut segs = Vec::new();
            let mut gen_col: i64 = 0;
            for seg_str in line_str.split(',') {
                if seg_str.is_empty() {
                    continue;
                }
                let mut chars = seg_str.chars().peekable();
                gen_col += decode_vlq(&mut chars)
                    .ok_or_else(|| SymbolError::Corrupt("bad vlq (gen col)".into()))?;
                let src = if chars.peek().is_some() {
                    src_idx += decode_vlq(&mut chars)
                        .ok_or_else(|| SymbolError::Corrupt("bad vlq (src idx)".into()))?;
                    src_line += decode_vlq(&mut chars)
                        .ok_or_else(|| SymbolError::Corrupt("bad vlq (src line)".into()))?;
                    src_col += decode_vlq(&mut chars)
                        .ok_or_else(|| SymbolError::Corrupt("bad vlq (src col)".into()))?;
                    let name = if chars.peek().is_some() {
                        name_idx += decode_vlq(&mut chars)
                            .ok_or_else(|| SymbolError::Corrupt("bad vlq (name)".into()))?;
                        Some(name_idx.max(0) as u32)
                    } else {
                        None
                    };
                    Some(SrcRef {
                        source: src_idx.max(0) as u32,
                        line: src_line.max(0) as u32,
                        col: src_col.max(0) as u32,
                        name,
                    })
                } else {
                    None
                };
                segs.push(Segment {
                    gen_col: gen_col.max(0) as u32,
                    src,
                });
            }
            lines.push(segs);
        }

        let approx_bytes = bytes.len()
            + sources_content
                .iter()
                .map(|c| c.as_ref().map(|s| s.len()).unwrap_or(0))
                .sum::<usize>();

        Ok(ParsedSourceMap {
            sources,
            names,
            sources_content,
            lines,
            approx_bytes,
        })
    }

    /// Resolve a generated `(lineno, colno)` (both 1-based) to the original position.
    pub fn resolve(&self, lineno_1based: u32, colno_1based: u32) -> Option<ResolvedLoc> {
        let li = (lineno_1based as usize).checked_sub(1)?;
        let segs = self.lines.get(li)?;
        if segs.is_empty() {
            return None;
        }
        let col0 = colno_1based.checked_sub(1)?;
        // Greatest segment with gen_col <= col0.
        let idx = match segs.binary_search_by(|s| s.gen_col.cmp(&col0)) {
            Ok(i) => i,
            Err(0) => return None, // column before the first segment
            Err(i) => i - 1,
        };
        let src = segs[idx].src.as_ref()?;
        let source = self.sources.get(src.source as usize).cloned()?;
        let name = src
            .name
            .and_then(|n| self.names.get(n as usize).cloned())
            .filter(|s| !s.is_empty());
        Some(ResolvedLoc {
            source,
            line: src.line + 1,
            column: src.col + 1,
            name,
            source_index: src.source as usize,
        })
    }

    /// Extract `radius` lines of source around a 1-based original line.
    pub fn context(
        &self,
        source_index: usize,
        line_1based: u32,
        radius: usize,
    ) -> Option<SourceContext> {
        let content = self.sources_content.get(source_index)?.as_ref()?;
        let lines: Vec<&str> = content.split('\n').collect();
        let li = (line_1based as usize).checked_sub(1)?;
        if li >= lines.len() {
            return None;
        }
        let start = li.saturating_sub(radius);
        let end = (li + radius + 1).min(lines.len());
        Some(SourceContext {
            pre: lines[start..li].iter().map(|s| s.to_string()).collect(),
            line: lines[li].to_string(),
            post: lines[li + 1..end].iter().map(|s| s.to_string()).collect(),
            start_line: start as u32 + 1,
        })
    }

    /// Approximate resident size, for the byte-bounded LRU.
    pub fn weight(&self) -> usize {
        self.approx_bytes
    }
}

fn prefix_slash(s: &str) -> String {
    if s.starts_with('/') {
        s.to_string()
    } else {
        format!("/{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP1: &str = r#"{"version":3,"sources":["foo.ts"],"names":["greet"],
        "mappings":"AAAAA",
        "sourcesContent":["export function greet(){\n  return 'hi'\n}\n"]}"#;

    // Two segments on gen line 0: [0,0,0,0,0] then [4,0,1,1,0] == "AAAAA,IACCA".
    // seg2 -> foo.ts line 2 col 2 (src_line +1, src_col +1).
    const MAP2: &str = r#"{"version":3,"sources":["foo.ts"],"names":["greet"],
        "mappings":"AAAAA,IACCA",
        "sourcesContent":["line0\nline1\nline2\n"]}"#;

    #[test]
    fn vlq_decodes_negative_and_zero() {
        assert_eq!(decode_vlq(&mut "A".chars().peekable()), Some(0));
        assert_eq!(decode_vlq(&mut "D".chars().peekable()), Some(-1));
        assert_eq!(decode_vlq(&mut "C".chars().peekable()), Some(1));
        assert_eq!(decode_vlq(&mut "I".chars().peekable()), Some(4));
    }

    #[test]
    fn resolves_single_segment() {
        let m = ParsedSourceMap::parse(MAP1.as_bytes()).unwrap();
        let r = m.resolve(1, 1).unwrap();
        assert_eq!(r.source, "foo.ts");
        assert_eq!(r.line, 1);
        assert_eq!(r.column, 1);
        assert_eq!(r.name.as_deref(), Some("greet"));
    }

    #[test]
    fn picks_greatest_segment_le_column() {
        let m = ParsedSourceMap::parse(MAP2.as_bytes()).unwrap();
        let r = m.resolve(1, 6).unwrap(); // 0-based col 5 -> seg2 (gen_col 4)
        assert_eq!((r.line, r.column), (2, 2));
        let r1 = m.resolve(1, 2).unwrap(); // 0-based col 1 -> seg1 (gen_col 0)
        assert_eq!((r1.line, r1.column), (1, 1));
    }

    #[test]
    fn column_before_first_and_missing_line_are_none() {
        let m = ParsedSourceMap::parse(MAP2.as_bytes()).unwrap();
        assert!(m.resolve(1, 1).is_some()); // col 1 -> 0-based 0 -> seg1
        assert!(m.resolve(9, 1).is_none()); // no such generated line
    }

    #[test]
    fn context_extracts_surrounding_lines() {
        let m = ParsedSourceMap::parse(MAP2.as_bytes()).unwrap();
        let c = m.context(0, 2, 1).unwrap();
        assert_eq!(c.line, "line1");
        assert_eq!(c.pre, vec!["line0".to_string()]);
        assert_eq!(c.post, vec!["line2".to_string()]);
        assert_eq!(c.start_line, 1);
    }

    #[test]
    fn rejects_non_v3() {
        let bad = br#"{"version":2,"sources":[],"names":[],"mappings":""}"#;
        assert!(ParsedSourceMap::parse(bad).is_err());
    }
}
