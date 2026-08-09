//! GNU build-id extraction from an uploaded ELF.
//!
//! Dart symbol artifacts match on `debug_id` alone (`arch` is accepted but
//! ignored — see `engine.rs`), and that id is the ELF's build-id. Requiring the
//! uploader to produce it by hand via `readelf -n` is most of why this upload
//! path stayed CLI-only, and a mismatched id fails *silently*: the event
//! symbolicates as `no_artifacts`, indistinguishable from never having
//! uploaded.
//!
//! # Why this walks the notes itself
//!
//! The ELF is untrusted, and `object`'s convenience `File::build_id()` is not
//! safe to call on untrusted bytes: it walks every note of *every* `SHT_NOTE`
//! section, and nothing stops thousands of section headers from aliasing the
//! same large note blob. Cost is then `section_count x blob_size` — minutes to
//! hours of CPU on one wedged thread from a single upload. `catch_unwind` gives
//! zero protection against that (it is a hang, not a panic), so the walk is
//! done here with an explicit budget instead: see the constants below.
//!
//! Doing the walk here also lets us validate what `build_id()` validates
//! (`n_type == NT_GNU_BUILD_ID` **and** `name == "GNU"`) while being more
//! forgiving than it on two points where it refuses files that are legal ELF:
//!
//! - `sh_addralign`. `object`'s `NoteIterator::new` returns `Err` for any
//!   alignment outside `{0,1,2,3,4,8}` (`read/elf/note.rs:38-43`), and
//!   `build_id()` propagates that with `?` — one odd note section and the whole
//!   file is rejected. binutils instead falls back to 4 and reads the notes, and
//!   so do we: Dart's `gen_snapshot` emits ELF from its own hand-rolled writer,
//!   and its alignment is not something we control.
//! - `e_shstrndx`. `object::File::parse` builds the section-name string table
//!   eagerly and fails with "Missing ELF e_shstrndx" when it is `SHN_UNDEF`,
//!   even though a section-name table is optional in ELF. We never need section
//!   *names*, so we read the section headers directly and skip that entirely.
//!
//! `build_id_hex` is still wrapped in `catch_unwind` for the same reason
//! `dart::resolve` is — defence in depth so a pathological upload degrades to a
//! clean 400, never takes down an API handler. Every allocation on this path is
//! bounded before it happens, so there is no OOM-abort route around it.

use object::read::elf::{FileHeader, ProgramHeader, SectionHeader};
use object::{elf, Endian, Endianness, FileKind};

use crate::content::SymbolError;

/// Section/program headers we are willing to look at.
///
/// Real linked ELFs measured on this toolchain: `sample.elf` 36, `git` 32,
/// `gcc` 36, `podman` 40, `glibc` 68 section headers; program headers top out
/// at 14. A Dart `--split-debug-info` artifact is at the low end of that. 4096
/// is ~60x the largest thing we measured and still far below the 65535 that
/// `e_shnum` can name (let alone the 2^32 the extended-`shnum` escape hatch
/// can), so an aliasing attack cannot buy a large multiplier here.
const MAX_HEADERS: usize = 4096;

/// Notes iterated across the whole file.
///
/// The previous bound (4096) and its justification comment were both wrong:
/// the comment claimed 162 (`glibc`) was "the most note-heavy binary on this
/// system" and called 4096 "~25x that worst case". Measured directly with the
/// exact parsing this module does (not text-scraped from `readelf`), swept
/// across every ELF under `/usr/bin`, `/usr/sbin`, `/usr/lib` and `/usr/lib64`
/// on this machine (11,894 files): `/usr/bin/qemu-system-x86_64` carries
/// **4,574** notes — over the old 4096 cap, not 25x under it — almost
/// entirely SystemTap probe descriptors in `.note.stapsdt` (4,570 of them in
/// one section). `qemu-system-i386` is close behind at 4,561. `git` (4),
/// `sample.elf` (12) and `glibc`/libc.so.6 (83) are nowhere near the real
/// worst case; a Dart symbols file carries a single-digit number of notes.
///
/// 32768 is ~7.2x the measured worst case (4574) — genuine headroom this
/// time, checked against the actual population instead of a handful of
/// binaries picked by eye. Raising this does *not* weaken the defence against
/// the aliasing attack described above `MAX_NOTE_BYTES`: that attack's cost is
/// `section_count x blob_size`, which `MAX_NOTE_BYTES` bounds independently of
/// how many notes are inside the blob, and it triggers within two encounters
/// of a maximally-sized blob regardless of this constant. This bound instead
/// guards the cheap-in-bytes/expensive-in-iterations case (many tiny notes
/// packed under the byte budget) — see `rejects_many_tiny_notes` — and
/// per-note work here is O(1) arithmetic, so 32768 iterations is still
/// microseconds, not the minutes/hours the byte budget exists to prevent.
const MAX_NOTES: usize = 32768;

/// Bytes of note payload walked across the whole file.
///
/// Measured totals: 284 B (`git`), 456 B (`sample.elf`), 7 KB (`glibc`). 1 MiB
/// is ~150x the largest real file measured. This is the bound that actually
/// defeats the aliasing attack: however many section headers point at a note
/// blob, we stop after walking a megabyte of notes in total.
const MAX_NOTE_BYTES: usize = 1024 * 1024;

/// Plausible GNU build-id descriptor lengths, in bytes.
///
/// `n_descsz` is attacker-controlled, and the descriptor is hex-encoded (2x) into
/// a `String` that becomes a `debug_id` column value — unbounded, a 128 MB note
/// becomes a 256 MB allocation. In practice: lld `--build-id=fast` emits 8,
/// `md5`/`uuid` 16, `sha1` 20 (what `sample.elf` and Dart carry), and the
/// largest hash any linker would plausibly use is SHA-512 at 64. The upper
/// bound is the security-relevant one — 64 bytes hex-encodes to 128 chars, far
/// inside the `(app_id, debug_id)` btree index's 2704-byte row limit. The lower
/// bound only excludes degenerate ids that would collide across builds.
const MIN_BUILD_ID_BYTES: usize = 4;
const MAX_BUILD_ID_BYTES: usize = 64;

/// `n_namesz` + `n_descsz` + `n_type`, each a `u32` — identical in ELF32 and
/// ELF64 (GNU/Linux uses 32-bit note headers in both).
const NOTE_HEADER_LEN: usize = 12;

/// Lowercase hex of the ELF's GNU build-id note.
pub fn build_id_hex(elf: &[u8]) -> Result<String, SymbolError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_id_inner(elf)))
        .unwrap_or_else(|_| Err(SymbolError::Corrupt("panic while reading build-id".into())))
}

fn build_id_inner(elf: &[u8]) -> Result<String, SymbolError> {
    match FileKind::parse(elf).map_err(|e| SymbolError::Corrupt(format!("elf parse: {e}")))? {
        FileKind::Elf32 => scan::<elf::FileHeader32<Endianness>>(elf),
        FileKind::Elf64 => scan::<elf::FileHeader64<Endianness>>(elf),
        other => Err(SymbolError::Corrupt(format!(
            "not an ELF file (detected {other:?}) — pass debug_id explicitly"
        ))),
    }
}

/// Walk the file's notes under [`Budget`] and return the build-id as hex.
///
/// Section headers are preferred over program headers, and program headers are
/// consulted only when there is no section table at all — the same precedence
/// `object::File::build_id()` uses.
fn scan<Elf: FileHeader<Endian = Endianness>>(data: &[u8]) -> Result<String, SymbolError> {
    let header = Elf::parse(data).map_err(|e| SymbolError::Corrupt(format!("elf header: {e}")))?;
    let endian = header
        .endian()
        .map_err(|e| SymbolError::Corrupt(format!("elf endian: {e}")))?;
    let mut budget = Budget::default();

    let sections = header
        .section_headers(endian, data)
        .map_err(|e| SymbolError::Corrupt(format!("elf section headers: {e}")))?;
    if sections.len() > MAX_HEADERS {
        return Err(SymbolError::Corrupt(format!(
            "implausible ELF: {} section headers (limit {MAX_HEADERS})",
            sections.len()
        )));
    }

    if !sections.is_empty() {
        for section in sections {
            if section.sh_type(endian) != elf::SHT_NOTE {
                continue;
            }
            // A section whose declared range falls outside the file is skipped,
            // not fatal: other sections may still carry a well-formed note.
            let Ok(bytes) = section.data(endian, data) else {
                continue;
            };
            let align: u64 = section.sh_addralign(endian).into();
            if let Some(desc) = find_build_id(bytes, align, endian, &mut budget)? {
                return encode(desc);
            }
        }
    } else {
        let segments = header
            .program_headers(endian, data)
            .map_err(|e| SymbolError::Corrupt(format!("elf program headers: {e}")))?;
        if segments.len() > MAX_HEADERS {
            return Err(SymbolError::Corrupt(format!(
                "implausible ELF: {} program headers (limit {MAX_HEADERS})",
                segments.len()
            )));
        }
        for segment in segments {
            if segment.p_type(endian) != elf::PT_NOTE {
                continue;
            }
            let Ok(bytes) = segment.data(endian, data) else {
                continue;
            };
            let align: u64 = segment.p_align(endian).into();
            if let Some(desc) = find_build_id(bytes, align, endian, &mut budget)? {
                return encode(desc);
            }
        }
    }

    Err(SymbolError::Corrupt(
        "no GNU build-id note in this file — pass debug_id explicitly".into(),
    ))
}

/// Hex-encode a build-id descriptor, rejecting implausible lengths *before*
/// allocating (see [`MIN_BUILD_ID_BYTES`]).
fn encode(desc: &[u8]) -> Result<String, SymbolError> {
    if desc.len() < MIN_BUILD_ID_BYTES || desc.len() > MAX_BUILD_ID_BYTES {
        return Err(SymbolError::Corrupt(format!(
            "implausible GNU build-id: {} bytes (expected {MIN_BUILD_ID_BYTES}..={MAX_BUILD_ID_BYTES})",
            desc.len()
        )));
    }
    Ok(crate::content::hex(desc))
}

/// Walk one note blob, returning the descriptor of the first
/// `NT_GNU_BUILD_ID`/`"GNU"` note in it.
///
/// Charges `budget` for the whole blob up front and for every note header it
/// reads; returns `Err` the moment either budget is spent, so the caller's loop
/// over aliasing section headers cannot run away.
fn find_build_id<'d>(
    bytes: &'d [u8],
    declared_align: u64,
    endian: Endianness,
    budget: &mut Budget,
) -> Result<Option<&'d [u8]>, SymbolError> {
    budget.charge_bytes(bytes.len())?;

    // binutils honours 8 and treats everything else (including the 0/1/2 that
    // hand-rolled writers emit) as 4, rather than refusing the file.
    let align = if declared_align == 8 { 8 } else { 4 };

    let mut off = 0usize;
    while let Some(rest) = bytes.get(off..) {
        if rest.len() < NOTE_HEADER_LEN {
            break;
        }
        budget.charge_note()?;

        let namesz = endian.read_u32_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        let descsz = endian.read_u32_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let ntype = endian.read_u32_bytes([rest[8], rest[9], rest[10], rest[11]]);

        // Any header field that walks off the end of the blob means the note
        // stream is malformed from here on; stop rather than guess.
        let Some(name_end) = NOTE_HEADER_LEN
            .checked_add(namesz)
            .filter(|e| *e <= rest.len())
        else {
            break;
        };
        let Some(desc_start) = align_up(name_end, align) else {
            break;
        };
        let Some(desc_end) = desc_start.checked_add(descsz).filter(|e| *e <= rest.len()) else {
            break;
        };

        if ntype == elf::NT_GNU_BUILD_ID
            && trim_nulls(&rest[NOTE_HEADER_LEN..name_end]) == elf::ELF_NOTE_GNU
        {
            return Ok(Some(&rest[desc_start..desc_end]));
        }

        // `desc_end >= NOTE_HEADER_LEN`, so this always advances — no spin.
        let Some(next) = align_up(desc_end, align).and_then(|n| off.checked_add(n)) else {
            break;
        };
        off = next;
    }

    Ok(None)
}

fn align_up(value: usize, align: usize) -> Option<usize> {
    value.checked_add(align - 1).map(|v| v & !(align - 1))
}

/// Note names are conventionally NUL-terminated but `n_namesz` may or may not
/// count the terminator; compare on the trimmed form, as `object` does.
fn trim_nulls(mut name: &[u8]) -> &[u8] {
    while let [rest @ .., 0] = name {
        name = rest;
    }
    name
}

/// Work allowance for one `build_id_hex` call, shared across every note blob in
/// the file so that aliasing section headers cannot multiply it.
#[derive(Default)]
struct Budget {
    notes: usize,
    bytes: usize,
}

impl Budget {
    fn charge_note(&mut self) -> Result<(), SymbolError> {
        self.notes += 1;
        if self.notes > MAX_NOTES {
            return Err(SymbolError::Corrupt(format!(
                "implausible ELF: more than {MAX_NOTES} notes"
            )));
        }
        Ok(())
    }

    fn charge_bytes(&mut self, n: usize) -> Result<(), SymbolError> {
        self.bytes = self.bytes.saturating_add(n);
        if self.bytes > MAX_NOTE_BYTES {
            return Err(SymbolError::Corrupt(format!(
                "implausible ELF: more than {MAX_NOTE_BYTES} bytes of notes"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // The same real ELF the DWARF resolver is verified against, built from
    // tests/fixtures/sample.c via `gcc -g -O0 -no-pie`.
    const ELF: &[u8] = include_bytes!("../tests/fixtures/sample.elf");

    // Same source, built with `gcc -g -O0 -no-pie -Wl,--build-id=none` so the
    // linker emits no build-id note. It still carries .note.gnu.property,
    // .note.ABI-tag and Fedora annobin's .gnu.build.attributes, so the whole
    // note walk runs and finds nothing — the most likely real-world bad upload.
    const ELF_NO_BUILD_ID: &[u8] = include_bytes!("../tests/fixtures/sample_no_buildid.elf");

    // `readelf -n tests/fixtures/sample.elf` → Build ID: ab3696...51a3. Pinning
    // the exact value, not just its shape: a wrong-but-well-formed id fails
    // *silently* downstream (events symbolicate as `no_artifacts`), so a shape
    // assertion cannot tell the working case from the broken one.
    const SAMPLE_BUILD_ID: &str = "ab36961b44baef9d7e3b9296dff3ce3e59be51a3";

    #[test]
    fn extracts_the_exact_build_id() {
        assert_eq!(build_id_hex(ELF).unwrap(), SAMPLE_BUILD_ID);
    }

    #[test]
    fn rejects_a_non_elf() {
        assert!(build_id_hex(b"not an elf at all").is_err());
    }

    #[test]
    fn rejects_a_truncated_elf() {
        // A prefix of a real ELF: valid magic, truncated structure. Renamed from
        // `rejects_truncated_input_without_panicking` — it never reached the
        // `catch_unwind` path and passed identically with the wrapper removed.
        // No input is known to make `object` unwind here; the wrapper stays as
        // defence in depth, not because this test exercises it.
        assert!(build_id_hex(&ELF[..64]).is_err());
    }

    #[test]
    fn valid_elf_without_a_build_id_note_errors() {
        let err = build_id_hex(ELF_NO_BUILD_ID).unwrap_err().to_string();
        assert!(
            err.contains("no GNU build-id note"),
            "expected the no-note error, got: {err}"
        );
    }

    // --- Budget regression pins (C1/C2) -----------------------------------
    //
    // Without these the budgets can be deleted later and every other test here
    // stays green. The elapsed-time bounds are deliberately loose: they exist to
    // separate "returns immediately" from "wedges a thread for minutes", not to
    // measure anything.

    #[test]
    fn rejects_an_absurd_section_count() {
        // 60000 SHT_NOTE section headers, all aliasing one note blob — the
        // quadratic shape. Must be refused on the header count alone.
        let elf = synth_elf64(60_000, &gnu_note(elf::NT_GNU_BUILD_ID, &[0xab; 20]), 4);
        let started = Instant::now();
        let err = build_id_hex(&elf).unwrap_err().to_string();
        assert!(
            err.contains("section headers"),
            "expected the header-count error, got: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "did not bail fast"
        );
    }

    #[test]
    fn rejects_note_bytes_aliased_by_many_sections() {
        // Under the header cap, but 4000 sections x ~4 KB of notes = ~16 MB of
        // note payload. The byte budget is what has to stop this.
        let blob = gnu_note(elf::NT_GNU_ABI_TAG, &[0u8; 4000]);
        let elf = synth_elf64(4000, &blob, 4);
        let started = Instant::now();
        let err = build_id_hex(&elf).unwrap_err().to_string();
        assert!(
            err.contains("bytes of notes"),
            "expected the note-byte budget error, got: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "did not bail fast"
        );
    }

    #[test]
    fn rejects_many_tiny_notes() {
        // 40000 empty notes in one blob: cheap in bytes (16 B each, 625 KB
        // total, well under MAX_NOTE_BYTES), expensive in iteration count —
        // must trip the note-count budget (32768), not the byte budget.
        let blob: Vec<u8> = std::iter::repeat_with(|| gnu_note(elf::NT_GNU_ABI_TAG, &[]))
            .take(40_000)
            .flatten()
            .collect();
        let elf = synth_elf64(2, &blob, 4);
        let err = build_id_hex(&elf).unwrap_err().to_string();
        assert!(
            err.contains("more than 32768 notes"),
            "expected the note-count budget error, got: {err}"
        );
    }

    #[test]
    fn rejects_an_absurd_build_id_descriptor() {
        // A well-formed, correctly-typed GNU build-id note whose descriptor is
        // 4 KB. Small enough to stay inside the note budgets, so it reaches the
        // descriptor bound — which must refuse it before hex-encoding it.
        let elf = synth_elf64(2, &gnu_note(elf::NT_GNU_BUILD_ID, &[0u8; 4096]), 4);
        let err = build_id_hex(&elf).unwrap_err().to_string();
        assert!(
            err.contains("implausible GNU build-id"),
            "expected the descriptor-length error, got: {err}"
        );
    }

    #[test]
    fn rejects_a_one_byte_build_id() {
        let elf = synth_elf64(2, &gnu_note(elf::NT_GNU_BUILD_ID, &[0xff]), 4);
        assert!(build_id_hex(&elf)
            .unwrap_err()
            .to_string()
            .contains("implausible GNU build-id"));
    }

    // --- Validation the deleted section-name fallback used to skip ---------

    #[test]
    fn ignores_a_note_with_the_wrong_type() {
        // Name "GNU", but n_type is NT_GNU_ABI_TAG. The old fallback hand-parsed
        // whatever sat in a section *named* .note.gnu.build-id and would have
        // fabricated a confident-looking id from bytes like these.
        let elf = synth_elf64(2, &gnu_note(elf::NT_GNU_ABI_TAG, &[0xab; 20]), 4);
        assert!(build_id_hex(&elf)
            .unwrap_err()
            .to_string()
            .contains("no GNU build-id note"));
    }

    #[test]
    fn ignores_a_note_with_the_wrong_name() {
        let elf = synth_elf64(
            2,
            &named_note(b"Go\0\0", elf::NT_GNU_BUILD_ID, &[0xab; 20]),
            4,
        );
        assert!(build_id_hex(&elf)
            .unwrap_err()
            .to_string()
            .contains("no GNU build-id note"));
    }

    #[test]
    fn reads_a_note_section_with_odd_alignment() {
        // sh_addralign = 16. `object::File::build_id()` rejects the whole file
        // here — `NoteIterator::new` errors on any alignment outside
        // {0,1,2,3,4,8} (object-0.36.7 read/elf/note.rs:38-43) and `build_id()`
        // propagates it. binutils falls back to 4 and reads the notes, and so do
        // we: Dart's gen_snapshot writes its own ELF and its note alignment is
        // not something we control. This is the case the deleted section-name
        // fallback was the only rescue for; it is now covered *with* validation.
        let elf = synth_elf64(2, &gnu_note(elf::NT_GNU_BUILD_ID, &[0xab; 20]), 16);
        assert_eq!(build_id_hex(&elf).unwrap(), "ab".repeat(20));
    }

    #[test]
    fn reads_a_big_endian_elf() {
        // The endianness of the note header comes from e_ident, never assumed.
        let elf = synth_elf64_be(&gnu_note_be(elf::NT_GNU_BUILD_ID, &[0xcd; 16]));
        assert_eq!(build_id_hex(&elf).unwrap(), "cd".repeat(16));
    }

    #[test]
    fn reads_a_32_bit_elf() {
        // D14: the ELF32 arm (`scan::<elf::FileHeader32<Endianness>>` at
        // build_id.rs:96) and the `u32 -> u64` `sh_addralign` widening it does
        // a few lines later had zero coverage — `synth` hardcoded ELFCLASS64
        // and both fixtures above (`sample.elf`, `sample_no_buildid.elf`) are
        // 64-bit binaries. Flutter ships `armeabi-v7a` (32-bit ARM), so this
        // is the arm most likely to break unnoticed on a real artifact.
        // Alignment is 16, not the default 4, so this also exercises the
        // widened value taking the "fall back to 4" branch (only a declared
        // alignment of exactly 8 does not) on a 32-bit `Elf32_Shdr`, the same
        // case `reads_a_note_section_with_odd_alignment` covers for ELF64.
        let elf = synth_elf32(2, &gnu_note(elf::NT_GNU_BUILD_ID, &[0xef; 20]), 16);
        assert_eq!(build_id_hex(&elf).unwrap(), "ef".repeat(20));
    }

    // --- Synthetic ELF builders -------------------------------------------

    fn named_note(name: &[u8], ntype: u32, desc: &[u8]) -> Vec<u8> {
        note_bytes(name, ntype, desc, false)
    }

    fn gnu_note(ntype: u32, desc: &[u8]) -> Vec<u8> {
        note_bytes(b"GNU\0", ntype, desc, false)
    }

    fn gnu_note_be(ntype: u32, desc: &[u8]) -> Vec<u8> {
        note_bytes(b"GNU\0", ntype, desc, true)
    }

    fn note_bytes(name: &[u8], ntype: u32, desc: &[u8], big_endian: bool) -> Vec<u8> {
        let w = |v: u32| -> [u8; 4] {
            if big_endian {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let mut v = Vec::new();
        v.extend_from_slice(&w(name.len() as u32));
        v.extend_from_slice(&w(desc.len() as u32));
        v.extend_from_slice(&w(ntype));
        v.extend_from_slice(name);
        v.extend_from_slice(desc);
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v
    }

    /// A little-endian ELF64 with `shnum` section headers: index 0 is the
    /// mandatory SHT_NULL entry and every other one is an `SHT_NOTE` section
    /// aliasing the *same* `note` blob — the exact shape C1 describes.
    fn synth_elf64(shnum: usize, note: &[u8], addralign: u64) -> Vec<u8> {
        synth(shnum, note, addralign, Class::Elf64, false)
    }

    fn synth_elf64_be(note: &[u8]) -> Vec<u8> {
        synth(2, note, 4, Class::Elf64, true)
    }

    /// Same shape as [`synth_elf64`] but ELFCLASS32 (`Elf32_Ehdr`/`Elf32_Shdr`,
    /// 4-byte address/offset fields throughout) — see D14 on
    /// `reads_a_32_bit_elf` for why this exists.
    fn synth_elf32(shnum: usize, note: &[u8], addralign: u64) -> Vec<u8> {
        synth(shnum, note, addralign, Class::Elf32, false)
    }

    #[derive(Clone, Copy)]
    enum Class {
        Elf32,
        Elf64,
    }

    fn synth(shnum: usize, note: &[u8], addralign: u64, class: Class, big_endian: bool) -> Vec<u8> {
        assert!(shnum <= u16::MAX as usize, "e_shnum is a u16");
        let w16 = |v: u16| -> [u8; 2] {
            if big_endian {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let w32 = |v: u32| -> [u8; 4] {
            if big_endian {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let w64 = |v: u64| -> [u8; 8] {
            if big_endian {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };

        // Elf32_Ehdr/Elf32_Shdr use 4-byte fields everywhere Elf64 uses 8, so
        // the header and section-header sizes (and every offset within a
        // section-header entry past sh_flags) differ between classes.
        let (ehsize, shentsize) = match class {
            Class::Elf32 => (52usize, 40usize),
            Class::Elf64 => (64usize, 64usize),
        };

        let note_off = ehsize;
        let shoff = (note_off + note.len()).next_multiple_of(8);
        let mut buf = vec![0u8; shoff + shnum * shentsize];

        buf[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        buf[4] = match class {
            Class::Elf32 => 1, // ELFCLASS32
            Class::Elf64 => 2, // ELFCLASS64
        };
        buf[5] = if big_endian { 2 } else { 1 }; // ELFDATA2MSB / ELFDATA2LSB
        buf[6] = 1; // EV_CURRENT
        buf[16..18].copy_from_slice(&w16(3)); // e_type = ET_DYN
        buf[20..24].copy_from_slice(&w32(1)); // e_version
        match class {
            Class::Elf32 => {
                buf[18..20].copy_from_slice(&w16(0x28)); // e_machine = EM_ARM
                buf[32..36].copy_from_slice(&w32(shoff as u32)); // e_shoff
                buf[40..42].copy_from_slice(&w16(ehsize as u16)); // e_ehsize
                buf[46..48].copy_from_slice(&w16(shentsize as u16)); // e_shentsize
                buf[48..50].copy_from_slice(&w16(shnum as u16)); // e_shnum
            }
            Class::Elf64 => {
                buf[18..20].copy_from_slice(&w16(0x3e)); // e_machine = EM_X86_64
                buf[40..48].copy_from_slice(&w64(shoff as u64)); // e_shoff
                buf[52..54].copy_from_slice(&w16(ehsize as u16)); // e_ehsize
                buf[58..60].copy_from_slice(&w16(shentsize as u16)); // e_shentsize
                buf[60..62].copy_from_slice(&w16(shnum as u16)); // e_shnum
            }
        }

        buf[note_off..note_off + note.len()].copy_from_slice(note);

        for i in 1..shnum {
            let sh = shoff + i * shentsize;
            buf[sh + 4..sh + 8].copy_from_slice(&w32(elf::SHT_NOTE)); // sh_type
            match class {
                Class::Elf32 => {
                    buf[sh + 16..sh + 20].copy_from_slice(&w32(note_off as u32)); // sh_offset
                    buf[sh + 20..sh + 24].copy_from_slice(&w32(note.len() as u32)); // sh_size
                    buf[sh + 32..sh + 36].copy_from_slice(&w32(addralign as u32));
                    // sh_addralign
                }
                Class::Elf64 => {
                    buf[sh + 24..sh + 32].copy_from_slice(&w64(note_off as u64)); // sh_offset
                    buf[sh + 32..sh + 40].copy_from_slice(&w64(note.len() as u64)); // sh_size
                    buf[sh + 48..sh + 56].copy_from_slice(&w64(addralign)); // sh_addralign
                }
            }
        }
        buf
    }
}
