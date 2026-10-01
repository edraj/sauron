//! Dart (Flutter AOT) symbolication.
//!
//! Flutter's `--split-debug-info` emits a file containing DWARF for each build:
//! an ELF on Android, a Mach-O dSYM companion on iOS/macOS (Flutter builds Apple
//! targets with `app-aot-macho-dylib`). `object` reads both and maps the DWARF
//! section names, so the resolver is the same. We resolve a stack frame's
//! address to the original function/file/line via DWARF, exactly as
//! `flutter symbolize` / `addr2line` would.
//!
//! v1 builds the DWARF context per call (no in-process context cache — Dart
//! error volume is low and the ELF bytes are still served from the blob cache).
//! DWARF is format-identical whether the ELF came from Dart or `gcc -g`, so this
//! is verified against a real compiled-C fixture in the tests.

use std::borrow::Cow;

use object::{Object, ObjectSection, ObjectSymbol};

use crate::content::SymbolError;
use crate::dart_trace::{DartFrameRef, DartTrace, InstructionsSection};
use crate::js::ResolvedLoc;

/// The root loading unit's id. Only it is covered by a build's main debug file;
/// every deferred unit ships its own (`package:native_stack_traces`'s
/// `rootLoadingUnitId`).
const ROOT_LOADING_UNIT: u32 = 1;

/// Resolve every frame of a parsed trace against the debug file. Same slot
/// contract as [`resolve`]: one slot per frame, in order.
///
/// Each frame's address is chosen the way Dart's own decoder,
/// `package:native_stack_traces`, chooses it — the trace's
/// `<instructions symbol>+0x<offset>` added to where the debug file puts that
/// symbol, before `virt`. The offset is the only address the running image and
/// the debug file agree on regardless of layout. `virt` and `abs - dso_base`
/// are only right when the loaded image is laid out exactly like the debug
/// file: true of an ELF snapshot (Android), false of an assembly-built one,
/// whose frames carry no `virt` and whose load-base fallback resolved to
/// unrelated functions with full confidence. Those two remain the fallback for
/// a file without the instruction symbols.
///
/// Frames from a deferred loading unit are never looked up: their addresses
/// belong to that unit's own debug file.
pub fn resolve_trace(elf: &[u8], trace: &DartTrace) -> Result<Vec<Vec<ResolvedLoc>>, SymbolError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let file = parse(elf)?;
        let vm = symbol_address(&file, InstructionsSection::Vm);
        let isolate = symbol_address(&file, InstructionsSection::Isolate);
        let addrs: Vec<Option<u64>> = trace
            .frames
            .iter()
            .map(|f| frame_address(f, trace.dso_base, vm, isolate))
            .collect();
        lookup(&file, &addrs)
    }))
    .unwrap_or_else(|_| Err(SymbolError::Corrupt("panic while parsing ELF/DWARF".into())))
}

/// Whether an uploaded Dart symbols file carries DWARF at all.
///
/// `Some(false)` for a file that parses as an object file but has no
/// `.debug_info` (`__debug_info` in a Mach-O). In practice that is the app
/// binary — `libapp.so` on Android, `App` on iOS — whose build-id is identical
/// to its `.symbols` file's, so nothing else about the upload looks wrong.
/// `None` when the file does not parse: judging those stays with the upload
/// route's explicit-`debug_id` escape hatch.
pub fn has_debug_info(file: &[u8]) -> Option<bool> {
    std::panic::catch_unwind(|| {
        let file = object::File::parse(file).ok()?;
        Some(
            file.section_by_name(".debug_info")
                .is_some_and(|s| s.size() > 0),
        )
    })
    .ok()
    .flatten()
}

fn frame_address(
    frame: &DartFrameRef,
    dso_base: Option<u64>,
    vm: Option<u64>,
    isolate: Option<u64>,
) -> Option<u64> {
    if frame.unit.is_some_and(|u| u != ROOT_LOADING_UNIT) {
        return None;
    }
    if let Some((section, offset)) = frame.symbol_offset {
        let start = match section {
            InstructionsSection::Vm => vm,
            InstructionsSection::Isolate => isolate,
        };
        if let Some(start) = start {
            return start.checked_add(offset);
        }
    }
    frame.lookup_addr(dso_base)
}

/// Where the debug file puts an instruction section's start symbol. Dart writes
/// it to the static symbol table and, in an ELF, the dynamic one too.
fn symbol_address(file: &object::File<'_>, section: InstructionsSection) -> Option<u64> {
    let name = section.symbol();
    file.symbols()
        .chain(file.dynamic_symbols())
        .find(|s| s.name() == Ok(name))
        .map(|s| s.address())
}

/// Resolve each frame's DSO-relative virtual address against the ELF's DWARF.
/// Returns exactly one slot per input slot, in the same order — callers pair the
/// two positionally. Each slot holds that address's inline frame chain (innermost
/// first); an address that resolves to nothing yields an empty inner vec.
///
/// `None` means the frame's address could not be determined at all (see
/// [`crate::dart_trace::DartFrameRef::lookup_addr`]). Such a slot is not looked
/// up and comes back empty — the same shape as a miss, which is the honest
/// answer. It must not be flattened to a placeholder address first: in
/// `tests/fixtures/sample_zero_base.elf`, whose `.text` starts at 0x0, address 0
/// resolves to `compute_total` at `sample.c:1`, so a placeholder 0 buys a
/// confidently wrong frame instead of a missing one.
///
/// The ELF is untrusted (uploaded); `object`/`gimli` are panic-resistant, but we
/// wrap parsing in `catch_unwind` so a pathological input can never take down an
/// ingest worker or API handler — it degrades to a clean error instead.
pub fn resolve(elf: &[u8], addrs: &[Option<u64>]) -> Result<Vec<Vec<ResolvedLoc>>, SymbolError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| resolve_inner(elf, addrs)))
        .unwrap_or_else(|_| Err(SymbolError::Corrupt("panic while parsing ELF/DWARF".into())))
}

fn resolve_inner(elf: &[u8], addrs: &[Option<u64>]) -> Result<Vec<Vec<ResolvedLoc>>, SymbolError> {
    lookup(&parse(elf)?, addrs)
}

fn parse(elf: &[u8]) -> Result<object::File<'_>, SymbolError> {
    object::File::parse(elf).map_err(|e| SymbolError::Corrupt(format!("elf parse: {e}")))
}

fn lookup(
    file: &object::File<'_>,
    addrs: &[Option<u64>],
) -> Result<Vec<Vec<ResolvedLoc>>, SymbolError> {
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
        // No address for this frame — keep its slot (the caller pairs slots to
        // frames by position) and leave it empty. Deliberately not looked up:
        // see this function's docs on why a placeholder would be worse.
        let Some(addr) = addr else {
            out.push(Vec::new());
            continue;
        };
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
        let out = resolve(ELF, &[Some(0x400446), Some(0x400457)]).unwrap();

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
        let out = resolve(ELF, &[Some(0xdead_beef)]).unwrap();
        assert!(out[0].is_empty());
    }

    /// The same tests/fixtures/sample.c, linked so `.text` starts at 0x0:
    /// `gcc -g -O0 -no-pie -nostdlib -nostartfiles -Wl,--section-start=.text=0x0`
    /// (the linker warns it cannot find `_start`; harmless, since this ELF is only
    /// ever parsed for DWARF, never executed). `readelf -S` shows `.text` at
    /// address 0, `nm` puts `compute_total` at 0x0, and `addr2line -fie` on 0
    /// prints `compute_total` at `…/sample.c:1`.
    ///
    /// Its whole purpose is to make address 0 a *hit*. `sample.elf` above is
    /// based at 0x400000, so 0 misses there and cannot tell a real guard from an
    /// absent one.
    const ZERO_BASE_ELF: &[u8] = include_bytes!("../tests/fixtures/sample_zero_base.elf");

    /// A frame with no determinable address must not borrow address 0's answer.
    ///
    /// Both slots go to the same ELF, so the only difference is `Some(0)` vs
    /// `None`. Substituting 0 for "unknown" is only ever safe if 0 misses, which
    /// is a property of the uploaded ELF and not of this code — here it hits, and
    /// the old `unwrap_or(0)` in `engine::symbolicate_dart` therefore did not
    /// merely lose a frame, it produced a confident wrong one.
    #[test]
    fn an_undeterminable_address_is_not_looked_up_as_zero() {
        let out = resolve(ZERO_BASE_ELF, &[Some(0), None]).unwrap();
        assert_eq!(
            out[0].first().and_then(|l| l.name.as_deref()),
            Some("compute_total"),
            "fixture precondition: address 0 IS real code in this ELF"
        );
        assert!(
            out[1].is_empty(),
            "a None slot must resolve to nothing, got {:?}",
            out[1]
        );
    }

    /// The positional contract `engine::symbolicate_dart` pairs against.
    #[test]
    fn returns_one_slot_per_input_including_none_slots() {
        let out = resolve(ELF, &[None, Some(0x400446), None, Some(0x400457)]).unwrap();
        assert_eq!(out.len(), 4);
        assert!(out[0].is_empty());
        assert_eq!(out[1][0].name.as_deref(), Some("compute_total"));
        assert!(out[2].is_empty());
        assert_eq!(out[3][0].name.as_deref(), Some("helper_add"));
    }

    /// `sample.elf` put through `strip --strip-debug`: the same build-id note
    /// and no DWARF — exactly how a Flutter app's `libapp.so` relates to its
    /// `--split-debug-info` `.symbols` file.
    const STRIPPED_ELF: &[u8] = include_bytes!("../tests/fixtures/sample_stripped.elf");

    #[test]
    fn debug_info_presence_is_only_claimed_for_files_that_parse() {
        assert_eq!(has_debug_info(ELF), Some(true));
        assert_eq!(has_debug_info(STRIPPED_ELF), Some(false));
        assert_eq!(has_debug_info(b"not an elf"), None);
    }

    #[test]
    fn garbage_elf_errors() {
        assert!(resolve(b"not an elf", &[Some(0)]).is_err());
    }

    // Built with `gcc -g -O2 -no-pie`, `scale()` is inlined into `outer()`.
    // 0x400460 is inside the inlined region → resolves to BOTH frames.
    const INLINE_ELF: &[u8] = include_bytes!("../tests/fixtures/sample_inline.elf");

    #[test]
    fn expands_inline_frames() {
        let out = resolve(INLINE_ELF, &[Some(0x400460)]).unwrap();
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
