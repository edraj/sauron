//! Dart (Flutter AOT) symbolication.
//!
//! Flutter's `--split-debug-info` emits an ELF containing DWARF for each build
//! (Android *and* iOS — the file format is the same). We resolve a stack frame's
//! DSO-relative virtual address to the original function/file/line via DWARF,
//! exactly as `flutter symbolize` / `addr2line` would.
//!
//! v1 builds the DWARF context per call (no in-process context cache — Dart
//! error volume is low and the ELF bytes are still served from the blob cache).
//! DWARF is format-identical whether the ELF came from Dart or `gcc -g`, so this
//! is verified against a real compiled-C fixture in the tests.

use std::borrow::Cow;

use object::{Object, ObjectSection};

use crate::content::SymbolError;
use crate::js::ResolvedLoc;

/// Resolve each DSO-relative virtual address against the ELF's DWARF. Returns,
/// per input address, the inline frame chain (innermost first). An address that
/// resolves to nothing yields an empty inner vec.
///
/// The ELF is untrusted (uploaded); `object`/`gimli` are panic-resistant, but we
/// wrap parsing in `catch_unwind` so a pathological input can never take down an
/// ingest worker or API handler — it degrades to a clean error instead.
pub fn resolve(elf: &[u8], addrs: &[u64]) -> Result<Vec<Vec<ResolvedLoc>>, SymbolError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| resolve_inner(elf, addrs)))
        .unwrap_or_else(|_| Err(SymbolError::Corrupt("panic while parsing ELF/DWARF".into())))
}

fn resolve_inner(elf: &[u8], addrs: &[u64]) -> Result<Vec<Vec<ResolvedLoc>>, SymbolError> {
    let file =
        object::File::parse(elf).map_err(|e| SymbolError::Corrupt(format!("elf parse: {e}")))?;

    let endian = if file.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };
    let load_section = |id: gimli::SectionId| -> Result<Cow<'_, [u8]>, gimli::Error> {
        Ok(match file.section_by_name(id.name()) {
            Some(s) => s.uncompressed_data().unwrap_or(Cow::Borrowed(&[][..])),
            None => Cow::Borrowed(&[][..]),
        })
    };
    let dwarf_sections = gimli::DwarfSections::load(load_section)
        .map_err(|e| SymbolError::Corrupt(format!("dwarf: {e}")))?;
    let dwarf = dwarf_sections.borrow(|section| gimli::EndianSlice::new(section, endian));
    let ctx = addr2line::Context::from_dwarf(dwarf)
        .map_err(|e| SymbolError::Corrupt(format!("dwarf ctx: {e}")))?;

    let mut out = Vec::with_capacity(addrs.len());
    for &addr in addrs {
        let mut locs = Vec::new();
        if let Ok(mut iter) = ctx.find_frames(addr).skip_all_loads() {
            while let Ok(Some(frame)) = iter.next() {
                let name = frame
                    .function
                    .and_then(|f| f.demangle().ok().map(|c| c.into_owned()))
                    .filter(|s| !s.is_empty());
                let (source, line, column) = match frame.location {
                    Some(loc) => (loc.file.map(|s| s.to_string()), loc.line, loc.column),
                    None => (None, None, None),
                };
                if name.is_none() && source.is_none() {
                    continue;
                }
                locs.push(ResolvedLoc {
                    source: source.unwrap_or_default(),
                    line: line.unwrap_or(0),
                    column: column.unwrap_or(0),
                    name,
                    source_index: 0,
                });
            }
        }
        out.push(locs);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real ELF with DWARF, built from tests/fixtures/sample.c via `gcc -g
    // -O0 -no-pie`. Addresses below come from `nm`; DWARF lines from objdump.
    const ELF: &[u8] = include_bytes!("../tests/fixtures/sample.elf");

    #[test]
    fn resolves_real_dwarf_functions() {
        let out = resolve(ELF, &[0x400446, 0x400457]).unwrap();

        let compute = &out[0];
        assert!(!compute.is_empty(), "compute_total did not resolve");
        assert_eq!(compute[0].name.as_deref(), Some("compute_total"));
        assert!(
            compute[0].source.ends_with("sample.c"),
            "unexpected source {}",
            compute[0].source
        );
        assert_eq!(compute[0].line, 1);

        let helper = &out[1];
        assert!(!helper.is_empty(), "helper_add did not resolve");
        assert_eq!(helper[0].name.as_deref(), Some("helper_add"));
        assert_eq!(helper[0].line, 4);
    }

    #[test]
    fn unknown_address_resolves_empty() {
        let out = resolve(ELF, &[0xdead_beef]).unwrap();
        assert!(out[0].is_empty());
    }

    #[test]
    fn garbage_elf_errors() {
        assert!(resolve(b"not an elf", &[0]).is_err());
    }

    // Built with `gcc -g -O2 -no-pie`, `scale()` is inlined into `outer()`.
    // 0x400460 is inside the inlined region → resolves to BOTH frames.
    const INLINE_ELF: &[u8] = include_bytes!("../tests/fixtures/sample_inline.elf");

    #[test]
    fn expands_inline_frames() {
        let out = resolve(INLINE_ELF, &[0x400460]).unwrap();
        let frames = &out[0];
        assert!(
            frames.len() >= 2,
            "expected inline expansion, got {}",
            frames.len()
        );
        assert_eq!(frames[0].name.as_deref(), Some("scale")); // innermost inlined
        assert_eq!(frames[0].line, 2);
        assert_eq!(frames[1].name.as_deref(), Some("outer")); // its caller
        assert_eq!(frames[1].line, 5);
    }
}
