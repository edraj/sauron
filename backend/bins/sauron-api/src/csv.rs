//! RFC 4180 CSV writing, plus the spreadsheet formula-injection guard.
//!
//! The `csv` crate was rejected and the reason is recorded here so it is not
//! re-litigated: `backend/Cargo.toml` has no `csv` dependency, adding one puts
//! a crate in every RPM build, and — the decisive point — the `csv` crate does
//! not do formula-injection escaping, so the one non-trivial rule below is
//! hand-rolled either way. The repo's precedent is to hand-roll small,
//! fully-testable primitives (`sauron_alerts::render::substitute` instead of a
//! template engine, hand-rolled `hmac_sha256_hex`, hand-rolled config parsing).
//!
//! This module exists even though v1's four columns (an ISO date and three
//! integers) trigger none of the guard, because a hand-rolled
//! join-with-commas at the one call site is exactly what would get copied into
//! the next export — the one that carries app, environment and person names.
//!
//! **No UTF-8 BOM.** v1 emits pure ASCII so the question is moot, and a BOM
//! breaks naive line-oriented tooling in a way that is harder to diagnose than
//! an Excel encoding prompt. Revisit on the first export that carries
//! non-ASCII text.

/// Escape one field per RFC 4180, with a formula-injection guard in front.
///
/// Order is load-bearing: the `'` prefix goes on BEFORE quoting. A spreadsheet
/// strips the surrounding quotes before deciding whether a cell is a formula,
/// so a prefix added outside them protects nothing.
pub fn escape_field(s: &str) -> String {
    let guarded = match s.as_bytes().first() {
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r') => format!("'{s}"),
        _ => s.to_string(),
    };
    // A tab needs no quoting under RFC 4180 itself, but an importer that sniffs
    // the delimiter rather than being told it (Excel's text import, several BI
    // loaders) splits an unquoted tab-bearing field into two columns and
    // silently shifts every column after it. Quoting is what stops that.
    let needs_quotes = guarded.contains(',')
        || guarded.contains('"')
        || guarded.contains('\t')
        || guarded.contains('\r')
        || guarded.contains('\n')
        || guarded.starts_with(' ')
        || guarded.ends_with(' ');
    if needs_quotes {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

/// Append one CRLF-terminated row. `\r\n` rather than `\n` because RFC 4180
/// says so and because the consumer is a spreadsheet, not a unix pipeline.
pub fn write_row(out: &mut String, fields: &[&str]) {
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&escape_field(f));
    }
    out.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use super::{escape_field, write_row};

    #[test]
    fn a_plain_field_is_not_quoted() {
        assert_eq!(escape_field("2026-05-04"), "2026-05-04");
        assert_eq!(escape_field("42"), "42");
    }

    #[test]
    fn an_empty_field_emits_nothing() {
        assert_eq!(escape_field(""), "");
        let mut out = String::new();
        write_row(&mut out, &["a", "", "b"]);
        assert_eq!(out, "a,,b\r\n");
    }

    #[test]
    fn a_comma_forces_quoting() {
        assert_eq!(escape_field("a,b"), "\"a,b\"");
    }

    #[test]
    fn a_quote_is_doubled_inside_quotes() {
        assert_eq!(escape_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn embedded_newlines_force_quoting() {
        assert_eq!(escape_field("a\r\nb"), "\"a\r\nb\"");
        assert_eq!(escape_field("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn leading_or_trailing_space_forces_quoting() {
        assert_eq!(escape_field(" a"), "\" a\"");
        assert_eq!(escape_field("a "), "\"a \"");
    }

    /// A cell a spreadsheet would EVALUATE rather than display. The `'` goes
    /// on before quoting, because the spreadsheet strips the surrounding
    /// quotes before deciding whether the cell is a formula — a `'` added
    /// outside them would do nothing.
    #[test]
    fn a_formula_leading_byte_gets_a_text_prefix() {
        assert_eq!(escape_field("=1+1"), "'=1+1");
        assert_eq!(escape_field("+1"), "'+1");
        assert_eq!(escape_field("-1"), "'-1");
        assert_eq!(escape_field("@SUM"), "'@SUM");
        assert_eq!(escape_field("\tx"), "\"'\tx\"");
        assert_eq!(escape_field("\rx"), "\"'\rx\"");
    }

    #[test]
    fn the_row_terminator_is_crlf() {
        let mut out = String::new();
        write_row(&mut out, &["day", "active_total"]);
        write_row(&mut out, &["2026-05-04", "7"]);
        assert_eq!(out, "day,active_total\r\n2026-05-04,7\r\n");
    }
}
