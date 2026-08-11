//! Keeps `wiki/Search.md`'s field tables honest against `catalog.rs`.
//!
//! The wiki is the only place a user learns what they may type. Its per-page
//! field lists were written by hand from the catalog, and nothing kept the two
//! together: a dimension added to `CATALOG` is invisible to every reader, and —
//! worse — a dimension renamed or dropped leaves the wiki advertising a spelling
//! that now answers **400**. The user follows the documentation exactly and is
//! told they are wrong.
//!
//! S2c closed with an interim rule that a `catalog.rs` change must edit the wiki
//! table in the same change. A rule nothing enforces is a rule that holds until
//! the first hurried afternoon. This is the enforcement.
//!
//! ## What is checked, and what deliberately is not
//!
//! Only the **first column** of the field tables under `## Fields by page`, and
//! only against dimension names and aliases. Types, notes and prose are the
//! author's to write; a test that policed them would be edited into silence the
//! first time it disagreed with a sentence that was fine.
//!
//! Both directions, because they fail differently:
//!
//! - **catalog → wiki**: an undocumented field is a feature nobody can find.
//! - **wiki → catalog**: a documented field that does not resolve is worse than
//!   no documentation, because the reader has no reason to doubt it.

use std::collections::BTreeSet;
use std::path::PathBuf;

use sauron_query::{dimensions_for, lookup, Resource};

/// Wiki headings under `## Fields by page`, and the resource each documents.
const SECTIONS: &[(&str, Resource)] = &[
    ("### Exceptions (Issues)", Resource::Issues),
    ("### Issue → Occurrences", Resource::Occurrences),
    ("### Events", Resource::Events),
];

/// Names that appear in a field column but are not catalog dimensions, with the
/// reason each is exempt. Kept as an explicit list rather than a pattern: every
/// entry is a claim about the docs that someone had to make deliberately.
const NOT_DIMENSIONS: &[(&str, &str)] = &[
    // The tag escape hatch is `TAG_DIM`, reached through the `tag.` PREFIX rather
    // than by name, so `lookup("tag.<key>")` cannot resolve it by construction.
    (
        "tag.<key>",
        "the tag prefix, resolved by `resolve_field`, not by name",
    ),
];

fn wiki_path() -> PathBuf {
    // backend/crates/sauron-query -> repo root
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../wiki/Search.md")
        .canonicalize()
        .unwrap_or_else(|e| {
            panic!(
                "wiki/Search.md not reachable from {}: {e}. This test exists to stop \
                 the documentation drifting from the catalog, so a missing file is a \
                 failure, not a reason to skip — a skip here is how the check quietly \
                 stops running.",
                env!("CARGO_MANIFEST_DIR")
            )
        })
}

/// The `## Fields by page` section, split at each `### ` heading.
fn read_sections() -> Vec<(String, String)> {
    let path = wiki_path();
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = text.find("\n## Fields by page").unwrap_or_else(|| {
        panic!(
            "`## Fields by page` heading is gone from {}",
            path.display()
        )
    });
    // Ends at the next `## ` (level two) heading.
    let rest = &text[start + 1..];
    let end = rest[3..].find("\n## ").map(|i| i + 4).unwrap_or(rest.len());
    let body = &rest[..end];

    let mut out = Vec::new();
    let mut heading: Option<String> = None;
    let mut buf = String::new();
    for line in body.lines() {
        if line.starts_with("### ") {
            if let Some(h) = heading.take() {
                out.push((h, std::mem::take(&mut buf)));
            }
            heading = Some(line.trim_end().to_string());
        } else if heading.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if let Some(h) = heading {
        out.push((h, buf));
    }
    out
}

/// Field names documented in one section: the first cell of every table row,
/// with each backticked token taken as a separate name.
///
/// A cell may hold a canonical name plus an alias — `` `is` (`status`) `` — or a
/// group of related fields on one row. Every backticked token in the cell has to
/// resolve, which is the point: an alias that stopped being an alias is exactly
/// the kind of quiet breakage this file is for.
fn documented_fields(section: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in section.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let first = line.trim_start_matches('|').split('|').next().unwrap_or("");
        // The header separator row, `|---|---|---|`.
        if first
            .trim()
            .chars()
            .all(|c| c == '-' || c == ':' || c.is_whitespace())
        {
            continue;
        }
        // The header row itself.
        if first.trim().eq_ignore_ascii_case("field") {
            continue;
        }
        for token in backticked(first) {
            out.insert(token);
        }
    }
    out
}

fn backticked(cell: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = cell;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let token = after[..close].trim();
        if !token.is_empty() {
            out.push(token.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

/// Every spelling the catalog accepts for a resource — canonical names and
/// aliases alike, since the wiki documents both.
fn catalog_spellings(r: Resource) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for d in dimensions_for(r) {
        out.insert(d.name.to_string());
        for a in d.aliases {
            out.insert((*a).to_string());
        }
    }
    out
}

#[test]
fn every_documented_field_resolves() {
    let sections = read_sections();
    let mut problems = Vec::new();

    for (heading, resource) in SECTIONS {
        let (_, body) = sections
            .iter()
            .find(|(h, _)| h == heading)
            .unwrap_or_else(|| panic!("`{heading}` is gone from wiki/Search.md's field tables"));
        let documented = documented_fields(body);
        assert!(
            !documented.is_empty(),
            "`{heading}` has no field table rows — either the table was removed or its \
             format changed, and this check silently stopped covering the section"
        );
        for field in &documented {
            if NOT_DIMENSIONS.iter().any(|(name, _)| name == field) {
                continue;
            }
            if lookup(field, *resource).is_none() {
                problems.push(format!(
                    "  {heading}: `{field}` is documented but does not resolve on \
                     {resource:?} — a reader who types it gets a 400"
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "wiki/Search.md documents fields the catalog does not accept:\n{}\n\n\
         Either restore the field in catalog.rs or correct the wiki table.",
        problems.join("\n")
    );
}

#[test]
fn every_catalog_field_is_documented() {
    let sections = read_sections();
    let mut problems = Vec::new();

    for (heading, resource) in SECTIONS {
        let (_, body) = sections
            .iter()
            .find(|(h, _)| h == heading)
            .unwrap_or_else(|| panic!("`{heading}` is gone from wiki/Search.md's field tables"));
        let documented = documented_fields(body);
        for d in dimensions_for(*resource) {
            // A row may document the dimension under an alias instead of its
            // canonical name; either satisfies "the reader can find it".
            let found =
                documented.contains(d.name) || d.aliases.iter().any(|a| documented.contains(*a));
            if !found {
                problems.push(format!(
                    "  {heading}: `{}` is searchable on {resource:?} and appears in no \
                     table row — nobody reading the docs can discover it",
                    d.name
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "catalog.rs has fields wiki/Search.md never mentions:\n{}\n\n\
         Add a row per field, or drop the dimension.",
        problems.join("\n")
    );
}

/// The catalog spellings a section documents must not include names from a
/// DIFFERENT resource.
///
/// The failure this catches is a copy-paste between two tables that look alike:
/// Occurrences and Events share most of their fields, so a row moved between
/// them reads perfectly and is wrong only for the handful that differ
/// (`handled`, `message`, `screen`, `name`). `every_documented_field_resolves`
/// already covers it — this exists to state the intent, and to make the
/// counter-example concrete if the tables are ever restructured.
#[test]
fn the_sections_do_not_document_each_others_fields() {
    let occ = catalog_spellings(Resource::Occurrences);
    let events = catalog_spellings(Resource::Events);
    assert!(
        occ.contains("handled") && !events.contains("handled"),
        "`handled` is meant to separate Occurrences from Events; if that changed, \
         this test's premise needs revisiting"
    );
    assert!(
        events.contains("name") && !occ.contains("name"),
        "`name` is meant to be Events-only"
    );
}

/// Prints a table row per dimension, for whoever has to answer a failure above.
///
/// The two checks say *which* field is missing; writing the row still needs its
/// type, its aliases and whether it is a scan — all of which live in
/// `catalog.rs` behind a `Dimension` literal. This prints them in the wiki's own
/// column order so the row can be pasted and then edited into prose.
///
/// `#[ignore]` because it asserts nothing: it is a tool, and a tool that runs on
/// every `cargo test` is just noise.
///
/// ```text
/// cargo test -p sauron-query --test wiki_catalog catalog_rows -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a generator for the wiki tables, not a check"]
fn catalog_rows() {
    for (heading, r) in SECTIONS {
        println!("{heading}");
        for d in dimensions_for(*r) {
            let aliases = if d.aliases.is_empty() {
                String::new()
            } else {
                format!(" (`{}`)", d.aliases.join("`, `"))
            };
            println!(
                "| `{}`{} | {:?} | {:?} | {:?} |",
                d.name, aliases, d.ty, d.store, d.index
            );
        }
    }
}
